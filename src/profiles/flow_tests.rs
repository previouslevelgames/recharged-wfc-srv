//! A test match from the challenge to the result, as two launchers and their
//! games go through it (docs/profiles.md), with GPCM's logins faked.

use std::{net::IpAddr, sync::Arc};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

use super::{Config, router};
use crate::{
    accounts,
    activity::Activity,
    gpcm::Gpcm,
    nas::Tickets,
    pairing::{Pairings, Visible},
};

struct World {
    app: Router,
    gpcm: Arc<Gpcm>,
    pairings: Arc<Pairings>,
    pool: PgPool,
}

impl World {
    fn new(pool: PgPool) -> Self {
        let gpcm = Arc::new(Gpcm::new(
            pool.clone(),
            Arc::<Tickets>::default(),
            Arc::<Activity>::default(),
        ));
        let pairings = Arc::<Pairings>::default();
        let config = Config::new("http://127.0.0.1:8080", None, true).unwrap();
        let app = router(pool.clone(), gpcm.clone(), pairings.clone(), config);
        Self {
            app,
            gpcm,
            pairings,
            pool,
        }
    }

    async fn send(&self, method: &str, uri: &str, token: &str, body: Value) -> (StatusCode, Value) {
        self.send_from(method, uri, token, None, body).await
    }

    /// A request of the game doing `activity` (the `Recharged-Activity`
    /// header); none is the launcher.
    async fn send_from(
        &self,
        method: &str,
        uri: &str,
        token: &str,
        activity: Option<&str>,
        body: Value,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json");
        if !token.is_empty() {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        if let Some(activity) = activity {
            request = request.header("recharged-activity", activity);
        }
        let body = if body.is_null() {
            Body::empty()
        } else {
            Body::from(body.to_string())
        };
        let response = self
            .app
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    async fn sign_in(&self, name: &str) -> String {
        let (status, body) = self
            .send("POST", "/api/v1/sign-in/test", "", json!({ "name": name }))
            .await;
        assert_eq!(status, StatusCode::OK);
        body["token"].as_str().unwrap().to_owned()
    }

    /// A console logged in to WFC as a Mii slot of `game`: its profile id.
    async fn wfc_login(&self, user_id: i64, gsbrcd: &str) -> i32 {
        accounts::log_in(&self.pool, user_id).await.unwrap();
        let profile = accounts::profile_login(&self.pool, user_id, gsbrcd, Some("Mii"), "10.0.0.1")
            .await
            .unwrap();
        self.gpcm
            .fake_login(profile.profile_id, "10.0.0.1".parse::<IpAddr>().unwrap());
        profile.profile_id
    }
}

fn games(mine: [u32; 2], theirs: [u32; 2]) -> Value {
    json!({ "games": [
        {"my_score": mine[0], "opponent_score": mine[1], "my_captain": "mario",
         "opponent_captain": "bowser", "stadium": "the_palace"},
        {"my_score": theirs[0], "opponent_score": theirs[1], "my_captain": "mario",
         "opponent_captain": "bowser", "stadium": "lava_pit"},
    ]})
}

fn mirrored(report: &Value) -> Value {
    let games: Vec<Value> = report["games"]
        .as_array()
        .unwrap()
        .iter()
        .map(|game| {
            json!({
                "my_score": game["opponent_score"], "opponent_score": game["my_score"],
                "my_captain": game["opponent_captain"], "opponent_captain": game["my_captain"],
                "stadium": game["stadium"],
            })
        })
        .collect();
    json!({ "games": games })
}

#[sqlx::test]
async fn a_test_match_from_challenge_to_result(pool: PgPool) {
    let world = World::new(pool);
    let mario = world.sign_in("Mario").await;
    let bowser = world.sign_in("Bowser").await;

    // Each game logs in to WFC; its port proves the login with the ticket.
    let mario_pid = world.wfc_login(1, "R4QEaaaaaaa").await;
    let bowser_pid = world.wfc_login(2, "R4QEbbbbbbb").await;
    let (status, _) = world
        .send(
            "POST",
            "/api/v1/wfc",
            &mario,
            json!({"profile_id": mario_pid, "login_ticket": "wrong"}),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    for (token, pid) in [(&mario, mario_pid), (&bowser, bowser_pid)] {
        let (status, body) = world
            .send(
                "POST",
                "/api/v1/wfc",
                token,
                json!({"profile_id": pid, "login_ticket": format!("ticket-{pid}")}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["game"], "R4QE");
    }
    let (_, me) = world.send("GET", "/api/v1/me", &mario, Value::Null).await;
    assert_eq!(me["wfc"][0]["profile_id"], mario_pid);

    // Mario challenges Bowser by name, with the Competitive rules.
    let (_, rules) = world.send("GET", "/api/v1/rules", "", Value::Null).await;
    let competitive = rules["rule_sets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|set| set["name"] == "Competitive")
        .unwrap()
        .clone();
    let (status, challenge) = world
        .send(
            "POST",
            "/api/v1/matches",
            &mario,
            json!({"opponent": "bowser", "rule_set": competitive["id"]}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{challenge}");
    assert_eq!(challenge["state"], "challenged");
    assert_eq!(challenge["side"], "challenger");
    assert_eq!(challenge["opponent"]["name"], "Bowser");
    assert_eq!(challenge["rules_hash"], competitive["hash"]);
    let id = challenge["id"].as_i64().unwrap();

    let (_, current) = world
        .send("GET", "/api/v1/matches/current", &bowser, Value::Null)
        .await;
    assert_eq!(current["match"]["id"], id);
    assert_eq!(current["match"]["side"], "challenged");
    assert_eq!(
        current["match"]["opponent"]["wfc"][0]["profile_id"],
        mario_pid
    );
    let (status, accepted) = world
        .send(
            "POST",
            &format!("/api/v1/matches/{id}/accept"),
            &bowser,
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(accepted["state"], "accepted");

    // Both games say they are ready, with the match's rules: RANKED then
    // finds only the other one.
    let ready = |pid: i32, hash: &Value| json!({"profile_id": pid, "rules_hash": hash});
    let (status, _) = world
        .send(
            "POST",
            &format!("/api/v1/matches/{id}/ready"),
            &mario,
            ready(mario_pid, &json!("another hash")),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (_, answer) = world
        .send(
            "POST",
            &format!("/api/v1/matches/{id}/ready"),
            &mario,
            ready(mario_pid, &competitive["hash"]),
        )
        .await;
    assert_eq!(answer["paired"], false);
    assert_eq!(world.pairings.visible_to(mario_pid), Visible::Nobody);
    let (_, answer) = world
        .send(
            "POST",
            &format!("/api/v1/matches/{id}/ready"),
            &bowser,
            ready(bowser_pid, &competitive["hash"]),
        )
        .await;
    assert_eq!(answer["paired"], true);
    assert_eq!(
        world.pairings.visible_to(mario_pid),
        Visible::Only(bowser_pid)
    );

    // Bowser's game can't be ready as a profile of somebody else's.
    let (status, _) = world
        .send(
            "POST",
            &format!("/api/v1/matches/{id}/ready"),
            &bowser,
            ready(mario_pid, &competitive["hash"]),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Both report the series the same way: Mario won 2-0.
    let report = games([3, 1], [2, 0]);
    let (_, first) = world
        .send(
            "POST",
            &format!("/api/v1/matches/{id}/report"),
            &mario,
            report.clone(),
        )
        .await;
    assert_eq!(first["state"], "accepted");
    assert_eq!(first["reported"], true);
    let (_, settled) = world
        .send(
            "POST",
            &format!("/api/v1/matches/{id}/report"),
            &bowser,
            mirrored(&report),
        )
        .await;
    assert_eq!(settled["state"], "finished");
    assert_eq!(settled["winner"], "opponent");
    assert_eq!(settled["games"][0]["my_score"], 1);
    assert_eq!(world.pairings.visible_to(mario_pid), Visible::Everyone);

    let (_, current) = world
        .send("GET", "/api/v1/matches/current", &mario, Value::Null)
        .await;
    assert_eq!(current["match"]["winner"], "you");
}

#[sqlx::test]
async fn challenges_need_a_known_opponent(pool: PgPool) {
    let world = World::new(pool);
    let mario = world.sign_in("Mario").await;
    for (opponent, status) in [
        ("nobody", StatusCode::NOT_FOUND),
        ("mario", StatusCode::BAD_REQUEST),
    ] {
        let (got, body) = world
            .send(
                "POST",
                "/api/v1/matches",
                &mario,
                json!({"opponent": opponent, "rule_set": 1}),
            )
            .await;
        assert_eq!(got, status, "{opponent}: {body}");
    }
    let (status, _) = world
        .send("GET", "/api/v1/matches/current", "", Value::Null)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (_, current) = world
        .send("GET", "/api/v1/matches/current", &mario, Value::Null)
        .await;
    assert_eq!(current["match"], Value::Null);
    assert_eq!(current["poll_interval"], 5);

    // A player whose Recharged is closed can't be challenged.
    world.sign_in("Luigi").await;
    sqlx::query!("UPDATE players SET heartbeat_at = now() - interval '2 minutes'")
        .execute(&world.pool)
        .await
        .unwrap();
    let (status, body) = world
        .send(
            "POST",
            "/api/v1/matches",
            &mario,
            json!({"opponent": "luigi", "rule_set": 1}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

#[sqlx::test]
async fn a_set_from_the_lobby_to_a_rematch(pool: PgPool) {
    let world = World::new(pool);
    let mario = world.sign_in("Mario").await;
    let bowser = world.sign_in("Bowser").await;

    // The lobby: Bowser has Recharged open; Mario challenges him from the
    // list, by his player id.
    let (status, lists) = world
        .send("GET", "/api/v1/players", &mario, Value::Null)
        .await;
    assert_eq!(status, StatusCode::OK);
    let online = lists["online"].as_array().unwrap();
    assert_eq!(online.len(), 1);
    assert_eq!(online[0]["name"], "Bowser");
    assert_eq!(
        (&online[0]["online"], &online[0]["busy"]),
        (&json!(true), &json!(false))
    );
    let (_, me) = world.send("GET", "/api/v1/me", &mario, Value::Null).await;
    assert_eq!(me["discord_id"], Value::Null);
    let (status, challenge) = world
        .send(
            "POST",
            "/api/v1/matches",
            &mario,
            json!({"opponent_id": online[0]["id"], "rule_set": 2}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{challenge}");
    assert_eq!(challenge["stage"], "invited");
    let id = challenge["id"].as_i64().unwrap();
    let post = |path: String, token: &str, body: Value| {
        let world = &world;
        let token = token.to_owned();
        async move { world.send("POST", &path, &token, body).await }
    };
    assert_eq!(challenge["you_ready"], json!(true));

    // The accept is Bowser's Ready to play!, the challenge was Mario's: go.
    let (status, go) = post(format!("/api/v1/matches/{id}/accept"), &bowser, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{go}");
    assert_eq!(
        (&go["stage"], &go["you_ready"], &go["opponent_ready"]),
        (&json!("go"), &json!(true), &json!(true))
    );
    assert!(go["go_at"].is_i64());
    // Taken back and pressed again: open, then go.
    let (_, open) = post(
        format!("/api/v1/matches/{id}/ready-to-play"),
        &mario,
        json!({"ready": false}),
    )
    .await;
    assert_eq!(
        (&open["stage"], &open["you_ready"], &open["opponent_ready"]),
        (&json!("open"), &json!(false), &json!(true))
    );
    let (_, go) = post(
        format!("/api/v1/matches/{id}/ready-to-play"),
        &mario,
        json!({"ready": true}),
    )
    .await;
    assert_eq!(go["stage"], "go");
    let (_, current) = world
        .send("GET", "/api/v1/matches/current", &bowser, Value::Null)
        .await;
    assert_eq!(
        (&current["match"]["stage"], &current["poll_interval"]),
        (&json!("go"), &json!(2))
    );

    // The games log in, prove it and meet: playing.
    for (token, user, gsbrcd) in [(&mario, 1, "R4QEaaaaaaa"), (&bowser, 2, "R4QEbbbbbbb")] {
        let pid = world.wfc_login(user, gsbrcd).await;
        post(
            "/api/v1/wfc".into(),
            token,
            json!({"profile_id": pid, "login_ticket": format!("ticket-{pid}")}),
        )
        .await;
        let (status, ready) = post(
            format!("/api/v1/matches/{id}/ready"),
            token,
            json!({"profile_id": pid, "rules_hash": go["rules_hash"]}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{ready}");
    }
    let (_, current) = world
        .send("GET", "/api/v1/matches/current", &mario, Value::Null)
        .await;
    assert_eq!(current["match"]["stage"], "playing");
    assert_eq!(current["match"]["state"], "accepted");

    // Both report the same series; then both ask for a rematch.
    let report = games([2, 1], [3, 0]);
    post(
        format!("/api/v1/matches/{id}/report"),
        &mario,
        report.clone(),
    )
    .await;
    let (_, finished) = post(
        format!("/api/v1/matches/{id}/report"),
        &bowser,
        mirrored(&report),
    )
    .await;
    assert_eq!(finished["stage"], "finished");
    let (_, offered) = post(
        format!("/api/v1/matches/{id}/rematch"),
        &bowser,
        Value::Null,
    )
    .await;
    assert_eq!(
        (&offered["stage"], &offered["rematch_of"]),
        (&json!("invited"), &json!(id))
    );
    let (_, again) = post(format!("/api/v1/matches/{id}/rematch"), &mario, Value::Null).await;
    assert_eq!(
        (&again["id"], &again["stage"]),
        (&offered["id"], &json!("go"))
    );
    let (_, lists) = world
        .send("GET", "/api/v1/players", &mario, Value::Null)
        .await;
    assert_eq!(lists["recent"][0]["name"], "Bowser");
    assert_eq!(lists["online"][0]["busy"], true);

    // Bowser closes Recharged: the set ends for Mario.
    let bowser_id = again["opponent"]["id"].as_i64().unwrap();
    sqlx::query!(
        "UPDATE players SET heartbeat_at = now() - interval '2 minutes' WHERE player_id = $1",
        bowser_id,
    )
    .execute(&world.pool)
    .await
    .unwrap();
    let (_, current) = world
        .send("GET", "/api/v1/matches/current", &mario, Value::Null)
        .await;
    assert_eq!(
        (&current["match"]["stage"], &current["match"]["reason"]),
        (&json!("dissolved"), &json!("dissolved"))
    );
}

#[sqlx::test]
async fn friends_who_keep_each_other_chat(pool: PgPool) {
    let world = World::new(pool);
    let mario = world.sign_in("Mario").await;
    let luigi = world.sign_in("Luigi").await;
    let peach = world.sign_in("Peach").await;
    let id_of = |name: &'static str| {
        let world = &world;
        async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT player_id FROM player_identities WHERE lower(username) = $1",
            )
            .bind(name)
            .fetch_one(&world.pool)
            .await
            .unwrap()
        }
    };
    let (luigi_id, peach_id, mario_id) = (
        id_of("luigi").await,
        id_of("peach").await,
        id_of("mario").await,
    );
    let write = |token: &str, to: i64, body: &str| {
        let (world, token, body) = (&world, token.to_owned(), body.to_owned());
        async move {
            world
                .send(
                    "POST",
                    &format!("/api/v1/chat/{to}"),
                    &token,
                    json!({ "body": body }),
                )
                .await
        }
    };

    // A challenge and its accept: each keeps the other, so they can write.
    let (_, challenge) = world
        .send(
            "POST",
            "/api/v1/matches",
            &mario,
            json!({"opponent": "luigi", "rule_set": 1}),
        )
        .await;
    let id = challenge["id"].as_i64().unwrap();
    world
        .send(
            "POST",
            &format!("/api/v1/matches/{id}/accept"),
            &luigi,
            Value::Null,
        )
        .await;
    let (status, sent) = write(&mario, luigi_id, "  good game?\nrematch!  ").await;
    assert_eq!(status, StatusCode::CREATED, "{sent}");
    assert_eq!(sent["body"], "good game? rematch!");
    assert_eq!(sent["mine"], json!(true));

    // Luigi sees it unread in the list, then reads it.
    let (_, lists) = world
        .send("GET", "/api/v1/players", &luigi, Value::Null)
        .await;
    let marios = &lists["friends"][0];
    assert_eq!(
        (&marios["name"], &marios["chat"], &marios["unread"]),
        (&json!("Mario"), &json!(true), &json!(1))
    );
    let (status, chat) = world
        .send(
            "GET",
            &format!("/api/v1/chat/{mario_id}"),
            &luigi,
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{chat}");
    assert_eq!(chat["can_send"], json!(true));
    assert_eq!(chat["messages"][0]["body"], "good game? rematch!");
    assert_eq!(chat["messages"][0]["mine"], json!(false));
    let (_, lists) = world
        .send("GET", "/api/v1/players", &luigi, Value::Null)
        .await;
    assert_eq!(lists["friends"][0]["unread"], json!(0));
    let (status, reply) = write(&luigi, mario_id, "sure").await;
    assert_eq!(status, StatusCode::CREATED, "{reply}");
    let after = sent["id"].as_i64().unwrap();
    let (_, newer) = world
        .send(
            "GET",
            &format!("/api/v1/chat/{luigi_id}?after={after}"),
            &mario,
            Value::Null,
        )
        .await;
    assert_eq!(newer["messages"].as_array().unwrap().len(), 1);
    assert_eq!(newer["messages"][0]["body"], "sure");

    // Empty and too long are refused.
    assert_eq!(
        write(&mario, luigi_id, " \n ").await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        write(&mario, luigi_id, &"x".repeat(301)).await.0,
        StatusCode::BAD_REQUEST
    );

    // Only friends who keep each other: Mario keeps Peach, not she him.
    world
        .send("POST", "/api/v1/friends", &mario, json!({"name": "peach"}))
        .await;
    let (status, refused) = write(&mario, peach_id, "hello").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    world
        .send("POST", "/api/v1/friends", &peach, json!({"name": "mario"}))
        .await;
    assert_eq!(
        write(&mario, peach_id, "hello").await.0,
        StatusCode::CREATED
    );

    // At most 20 a minute.
    for _ in 0..18 {
        assert_eq!(write(&mario, peach_id, "spam").await.0, StatusCode::CREATED);
    }
    assert_eq!(
        write(&mario, peach_id, "spam").await.0,
        StatusCode::TOO_MANY_REQUESTS
    );

    // Taken off the list: no more writing.
    world
        .send(
            "DELETE",
            &format!("/api/v1/friends/{mario_id}"),
            &luigi,
            Value::Null,
        )
        .await;
    let (_, chat) = world
        .send(
            "GET",
            &format!("/api/v1/chat/{mario_id}"),
            &luigi,
            Value::Null,
        )
        .await;
    assert_eq!(chat["can_send"], json!(false));
    assert_eq!(
        write(&luigi, mario_id, "bye").await.0,
        StatusCode::FORBIDDEN
    );
}

#[sqlx::test]
async fn a_match_in_the_game_shows_and_keeps_challenges_away(pool: PgPool) {
    let world = World::new(pool);
    let mario = world.sign_in("Mario").await;
    let luigi = world.sign_in("Luigi").await;
    let activity_of_luigi = || async {
        let (_, lists) = world
            .send("GET", "/api/v1/players", &mario, Value::Null)
            .await;
        lists["online"][0]["activity"].clone()
    };
    let challenge = || {
        world.send(
            "POST",
            "/api/v1/matches",
            &mario,
            json!({"opponent": "luigi", "rule_set": 1}),
        )
    };
    assert_eq!(activity_of_luigi().await, json!("launcher"));

    // Luigi's game starts a match: shown at once, and no challenge reaches it.
    world
        .send_from(
            "GET",
            "/api/v1/matches/current",
            &luigi,
            Some("game"),
            Value::Null,
        )
        .await;
    assert_eq!(activity_of_luigi().await, json!("game"));
    world
        .send_from(
            "GET",
            "/api/v1/matches/current",
            &luigi,
            Some("match"),
            Value::Null,
        )
        .await;
    assert_eq!(activity_of_luigi().await, json!("match"));
    let (status, refused) = challenge().await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["error"], "that player is playing a match right now");

    // Back in the game's menus, Luigi can be challenged again.
    world
        .send_from(
            "GET",
            "/api/v1/matches/current",
            &luigi,
            Some("game"),
            Value::Null,
        )
        .await;
    let (status, created) = challenge().await;
    assert_eq!(status, StatusCode::CREATED, "{created}");

    // A closed Recharged has no activity.
    sqlx::query("UPDATE players SET heartbeat_at = now() - interval '2 minutes'")
        .execute(&world.pool)
        .await
        .unwrap();
    let (_, lists) = world
        .send("GET", "/api/v1/players", &mario, Value::Null)
        .await;
    assert_eq!(lists["friends"][0]["activity"], Value::Null);
}

#[sqlx::test]
async fn friends_come_from_challenges_and_follow_the_player(pool: PgPool) {
    let world = World::new(pool);
    let mario = world.sign_in("Mario").await;
    let luigi = world.sign_in("Luigi").await;
    let peach = world.sign_in("Peach").await;
    let friends = |token: String| {
        let world = &world;
        async move {
            let (status, lists) = world
                .send("GET", "/api/v1/players", &token, Value::Null)
                .await;
            assert_eq!(status, StatusCode::OK, "{lists}");
            lists["friends"].as_array().unwrap().clone()
        }
    };
    let names = |list: &[Value]| -> Vec<String> {
        list.iter()
            .map(|friend| friend["name"].as_str().unwrap().to_owned())
            .collect()
    };
    assert!(friends(mario.clone()).await.is_empty());

    // A challenge keeps its opponent; the one challenged keeps the
    // challenger only once they accept.
    let (status, challenge) = world
        .send(
            "POST",
            "/api/v1/matches",
            &mario,
            json!({"opponent": "luigi", "rule_set": 1}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{challenge}");
    assert_eq!(names(&friends(mario.clone()).await), ["Luigi"]);
    assert!(friends(luigi.clone()).await.is_empty());
    let id = challenge["id"].as_i64().unwrap();
    world
        .send(
            "POST",
            &format!("/api/v1/matches/{id}/accept"),
            &luigi,
            Value::Null,
        )
        .await;
    let luigis = friends(luigi.clone()).await;
    assert_eq!(names(&luigis), ["Mario"]);
    assert_eq!(
        (
            &luigis[0]["online"],
            &luigis[0]["busy"],
            &luigis[0]["friend"]
        ),
        (&json!(true), &json!(true), &json!(true))
    );
    assert!(luigis[0]["last_online"].as_i64().unwrap() > 0);

    // By hand: by name, then by id; online friends first, the others by
    // when they were last online; the other lists mark friends.
    let (status, added) = world
        .send("POST", "/api/v1/friends", &mario, json!({"name": "PEACH"}))
        .await;
    assert_eq!(status, StatusCode::OK, "{added}");
    assert_eq!(
        names(added["friends"].as_array().unwrap()),
        ["Luigi", "Peach"]
    );
    sqlx::query(
        "UPDATE players SET heartbeat_at = now() - interval '3 hours'
         WHERE player_id = (SELECT player_id FROM player_identities WHERE lower(username) = 'luigi')",
    )
    .execute(&world.pool)
    .await
    .unwrap();
    let marios = friends(mario.clone()).await;
    assert_eq!(names(&marios), ["Peach", "Luigi"]);
    assert_eq!(marios[1]["online"], json!(false));
    let (_, lists) = world
        .send("GET", "/api/v1/players", &mario, Value::Null)
        .await;
    assert_eq!(lists["online"][0]["name"], "Peach");
    assert_eq!(lists["online"][0]["friend"], json!(true));
    let (_, lists) = world
        .send("GET", "/api/v1/players", &peach, Value::Null)
        .await;
    assert_eq!(lists["online"][0]["name"], "Mario");
    assert_eq!(lists["online"][0]["friend"], json!(false));

    // Adding again changes nothing; nobody adds themselves or an unknown name.
    let peach_id = marios[0]["id"].as_i64().unwrap();
    let (status, again) = world
        .send(
            "POST",
            "/api/v1/friends",
            &mario,
            json!({"player_id": peach_id}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["friends"].as_array().unwrap().len(), 2);
    for (body, expected) in [
        (json!({"name": "mario"}), StatusCode::BAD_REQUEST),
        (json!({"name": "wario"}), StatusCode::NOT_FOUND),
        (json!({}), StatusCode::BAD_REQUEST),
    ] {
        let (status, answer) = world.send("POST", "/api/v1/friends", &mario, body).await;
        assert_eq!(status, expected, "{answer}");
    }

    // Removing: gone, and gone again is no error.
    for _ in 0..2 {
        let (status, removed) = world
            .send(
                "DELETE",
                &format!("/api/v1/friends/{peach_id}"),
                &mario,
                Value::Null,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{removed}");
        assert_eq!(names(removed["friends"].as_array().unwrap()), ["Luigi"]);
    }
    let (status, _) = world
        .send(
            "POST",
            "/api/v1/friends",
            "",
            json!({"player_id": peach_id}),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
