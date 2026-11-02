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
        let activity = activity.map(|activity| ("recharged-activity", activity));
        self.send_with(method, uri, token, activity.as_slice(), body)
            .await
    }

    /// A request with the extra `headers`.
    async fn send_with(
        &self,
        method: &str,
        uri: &str,
        token: &str,
        headers: &[(&str, &str)],
        body: Value,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json");
        if !token.is_empty() {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        for (name, value) in headers {
            request = request.header(*name, *value);
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

    /// **Play with a Mii** from the launcher at `address`, which the
    /// server's proxy names.
    async fn mii_sign_in(&self, name: &str, address: &str) -> (StatusCode, Value) {
        self.send_with(
            "POST",
            "/api/v1/sign-in/mii",
            "",
            &[("cf-connecting-ip", address)],
            json!({ "name": name }),
        )
        .await
    }

    /// The port proves its game's WFC login for the player of `token`.
    async fn prove(&self, token: &str, profile_id: i32) -> (StatusCode, Value) {
        self.send(
            "POST",
            "/api/v1/wfc",
            token,
            json!({"profile_id": profile_id, "login_ticket": format!("ticket-{profile_id}")}),
        )
        .await
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
async fn a_challenge_plays_by_rules_of_its_own(pool: PgPool) {
    let world = World::new(pool);
    let mario = world.sign_in("Mario").await;
    let peach = world.sign_in("Peach").await;
    world.sign_in("Luigi").await;
    world.sign_in("Daisy").await;
    let own = json!({"tweaks": ["fast_terrain"], "limit": "goals", "time": 180, "goals": 7, "games": 5,
                     "stadium": "the_palace", "stadium_cheat": "field_tilt", "powerup_cheat": "shells"});
    // No rules, and rules the game doesn't have, are refused.
    let mut wrong = own.clone();
    wrong["powerup_cheat"] = json!("butterfingers");
    for body in [
        json!({"opponent": "luigi"}),
        json!({"opponent": "luigi", "rules": wrong}),
        json!({"opponent": "luigi", "rules": own, "name": "x".repeat(49)}),
    ] {
        let (status, answer) = world.send("POST", "/api/v1/matches", &mario, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
    }
    // The preset's name goes with them; the skill and the cheats left out
    // are the defaults.
    let (status, challenge) = world
        .send(
            "POST",
            "/api/v1/matches",
            &mario,
            json!({"opponent": "luigi", "rules": own, "name": " Palace tilt "}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{challenge}");
    assert_eq!(challenge["rule_set"], "Palace tilt");
    assert_eq!(challenge["rules"]["stadium_cheat"], "field_tilt");
    assert_eq!(challenge["rules"]["powerup_cheat"], "shells");
    assert_eq!(challenge["rules"]["player_cheat"], "none");
    assert_eq!(challenge["rules"]["skill"], "rookie");
    assert_eq!(challenge["rules_hash"].as_str().unwrap().len(), 64);
    // Without a name: Custom.
    let (status, challenge) = world
        .send(
            "POST",
            "/api/v1/matches",
            &peach,
            json!({"opponent": "daisy", "rules": own}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{challenge}");
    assert_eq!(challenge["rule_set"], "Custom");
}

#[sqlx::test]
async fn a_search_leaves_out_players_in_the_games_online_mode(pool: PgPool) {
    let world = World::new(pool);
    // Mario searches; Peach is open to challenges, but her game searches
    // RANKED (DWC's matchmaking), then plays its match.
    let mario = world.sign_in("Mario").await;
    let peach = world.sign_in("Peach").await;
    let mine = world.wfc_login(1, "R4QEaaaaaaa").await;
    let hers = world.wfc_login(2, "R4QEbbbbbbb").await;
    assert_eq!(world.prove(&mario, mine).await.0, StatusCode::OK);
    assert_eq!(world.prove(&peach, hers).await.0, StatusCode::OK);
    let (status, _) = world
        .send(
            "PUT",
            "/api/v1/me/available",
            &peach,
            json!({"available": true}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    world.gpcm.fake_status(hers, 3);
    let rules = json!({"tweaks": [], "limit": "time", "time": 120, "goals": 5, "games": 1,
                       "stadium": "random"});
    let (status, body) = world
        .send(
            "POST",
            "/api/v1/search",
            &mario,
            json!({"rules": rules, "name": "Mine", "game": "R4QE"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["search"]["asked"], 0);
    world.gpcm.fake_status(hers, 2);
    let (_, now) = world
        .send("GET", "/api/v1/matches/current", &mario, Value::Null)
        .await;
    assert_eq!(now["search"]["asked"], 0);

    // Back in the online menu: she is asked.
    world.gpcm.fake_status(hers, 1);
    let (_, now) = world
        .send("GET", "/api/v1/matches/current", &mario, Value::Null)
        .await;
    assert_eq!(now["search"]["asked"], 1);
    assert_eq!(now["match"]["opponent"]["name"], "Peach");
}

#[sqlx::test]
async fn a_search_asks_players_open_to_challenges(pool: PgPool) {
    let world = World::new(pool);
    // Mario searches; Peach and Luigi are open to challenges and play the
    // US game; Daisy isn't open; Wario is, with the European game.
    let names = ["Mario", "Peach", "Luigi", "Daisy", "Wario"];
    let mut tokens = Vec::new();
    for (n, name) in names.iter().enumerate() {
        let token = world.sign_in(name).await;
        let game = if *name == "Wario" {
            "R4QPwwwwwww"
        } else {
            "R4QEaaaaaaa"
        };
        let pid = world.wfc_login(n as i64 + 1, game).await;
        let (status, body) = world
            .send(
                "POST",
                "/api/v1/wfc",
                &token,
                json!({"profile_id": pid, "login_ticket": format!("ticket-{pid}")}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        if *name != "Mario" && *name != "Daisy" {
            let (status, _) = world
                .send(
                    "PUT",
                    "/api/v1/me/available",
                    &token,
                    json!({"available": true}),
                )
                .await;
            assert_eq!(status, StatusCode::OK);
        }
        tokens.push(token);
    }
    let mario = &tokens[0];
    let token_of = |name: &str| tokens[names.iter().position(|n| *n == name).unwrap()].clone();
    let own = json!({"tweaks": [], "limit": "time", "time": 120, "goals": 5, "games": 1,
                     "stadium": "the_palace", "powerup_cheat": "shells"});

    // A game id that isn't one is refused.
    let (status, _) = world
        .send(
            "POST",
            "/api/v1/search",
            mario,
            json!({"rules": own, "game": "R4"}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, body) = world
        .send(
            "POST",
            "/api/v1/search",
            mario,
            json!({"rules": own, "name": "Palace", "game": "R4QE"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["search"]["rule_set"], "Palace");
    assert_eq!(body["search"]["asked"], 1);

    // The first player asked: a random challenge by Mario's rules.
    let current = |token: String| {
        let world = &world;
        async move {
            world
                .send("GET", "/api/v1/matches/current", &token, Value::Null)
                .await
                .1
        }
    };
    let now = current(mario.clone()).await;
    assert_eq!(now["search"]["asked"], 1);
    assert_eq!(now["match"]["random"], true);
    assert_eq!(now["match"]["state"], "challenged");
    assert_eq!(now["match"]["rule_set"], "Palace");
    assert_eq!(now["match"]["rules"]["powerup_cheat"], "shells");
    let first = now["match"]["opponent"]["name"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(first == "Peach" || first == "Luigi", "{first}");
    let other = if first == "Peach" { "Luigi" } else { "Peach" };

    // They decline: the other one is asked.
    let id = now["match"]["id"].as_i64().unwrap();
    let (status, _) = world
        .send(
            "POST",
            &format!("/api/v1/matches/{id}/decline"),
            &token_of(&first),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let now = current(mario.clone()).await;
    assert_eq!(now["search"]["asked"], 2);
    assert_eq!(now["match"]["opponent"]["name"], other);
    let id = now["match"]["id"].as_i64().unwrap();

    // No answer in time: nobody left to ask (Daisy isn't open, Wario plays
    // another game), the search waits.
    sqlx::query!(
        "UPDATE matches SET created_at = now() - interval '1 minute' WHERE match_id = $1",
        id
    )
    .execute(&world.pool)
    .await
    .unwrap();
    let now = current(mario.clone()).await;
    let state = sqlx::query_scalar!("SELECT state FROM matches WHERE match_id = $1", id)
        .fetch_one(&world.pool)
        .await
        .unwrap();
    assert_eq!(state, "expired");
    assert_eq!(now["search"]["asked"], 2);
    assert_ne!(now["match"]["state"], "challenged");

    // Stop, and search again: a challenge again, which its player accepts:
    // the set goes on, the search ends.
    let (status, _) = world
        .send("DELETE", "/api/v1/search", mario, Value::Null)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(current(mario.clone()).await["search"], Value::Null);
    world
        .send(
            "POST",
            "/api/v1/search",
            mario,
            json!({"rules": own, "name": "Palace", "game": "R4QE"}),
        )
        .await;
    let now = current(mario.clone()).await;
    let asked = now["match"]["opponent"]["name"]
        .as_str()
        .unwrap()
        .to_owned();
    let id = now["match"]["id"].as_i64().unwrap();
    let (status, _) = world
        .send(
            "POST",
            &format!("/api/v1/matches/{id}/accept"),
            &token_of(&asked),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let now = current(mario.clone()).await;
    assert_eq!(now["match"]["state"], "accepted");
    assert_eq!(now["search"], Value::Null);
    // With a set to play, no search.
    let (status, _) = world
        .send(
            "POST",
            "/api/v1/search",
            mario,
            json!({"rules": own, "game": "R4QE"}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
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
async fn a_set_lasts_while_its_games_call(pool: PgPool) {
    let world = World::new(pool);
    let mario = world.sign_in("Mario").await;
    let bowser = world.sign_in("Bowser").await;
    let (_, challenge) = world
        .send(
            "POST",
            "/api/v1/matches",
            &mario,
            json!({"opponent": "bowser", "rule_set": 2}),
        )
        .await;
    let id = challenge["id"].as_i64().unwrap();
    let (_, go) = world
        .send(
            "POST",
            &format!("/api/v1/matches/{id}/accept"),
            &bowser,
            Value::Null,
        )
        .await;
    assert_eq!(go["stage"], "go");
    let mut pids = Vec::new();
    for (token, user, gsbrcd) in [(&mario, 1, "R4QEaaaaaaa"), (&bowser, 2, "R4QEbbbbbbb")] {
        let pid = world.wfc_login(user, gsbrcd).await;
        world
            .send(
                "POST",
                "/api/v1/wfc",
                token,
                json!({"profile_id": pid, "login_ticket": format!("ticket-{pid}")}),
            )
            .await;
        pids.push(pid);
    }
    let ready = async |world: &World, token: &str, pid: i32| {
        let body = json!({"profile_id": pid, "rules_hash": go["rules_hash"]});
        world
            .send("POST", &format!("/api/v1/matches/{id}/ready"), token, body)
            .await
    };
    let stage = async |world: &World, token: &str| {
        let (_, current) = world
            .send("GET", "/api/v1/matches/current", token, Value::Null)
            .await;
        current["match"]["stage"].clone()
    };

    // Mario's game says it is ready, then his Ready to play! is taken back:
    // Bowser's game doesn't pair with it until it says so again.
    let (_, answer) = ready(&world, &mario, pids[0]).await;
    assert_eq!(answer["paired"], false);
    let (_, open) = world
        .send(
            "POST",
            &format!("/api/v1/matches/{id}/ready-to-play"),
            &mario,
            json!({"ready": false}),
        )
        .await;
    assert_eq!(open["stage"], "open");
    let (_, answer) = ready(&world, &bowser, pids[1]).await;
    assert_eq!(answer["paired"], false);
    assert_eq!(stage(&world, &bowser).await, "open");
    let (_, answer) = ready(&world, &mario, pids[0]).await;
    assert_eq!(answer["paired"], true);
    assert_eq!(stage(&world, &bowser).await, "playing");

    // The server restarts during the match: the games' GPCM logins are gone,
    // so their ready calls are refused, but they still play the set.
    let restarted = World::new(world.pool.clone());
    sqlx::query!(
        "UPDATE matches SET home_game_ready_at = now() - interval '61 seconds',
                            away_game_ready_at = now() - interval '61 seconds'
         WHERE match_id = $1",
        id,
    )
    .execute(&world.pool)
    .await
    .unwrap();
    for (token, pid) in [(&mario, pids[0]), (&bowser, pids[1])] {
        let (status, refused) = ready(&restarted, token, pid).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    }
    assert_eq!(stage(&restarted, &mario).await, "playing");

    // Bowser's game stops calling, Mario's goes on: the set was broken off,
    // though both still have Recharged open.
    sqlx::query!(
        "UPDATE matches SET away_game_ready_at = now() - interval '61 seconds'
         WHERE match_id = $1",
        id,
    )
    .execute(&world.pool)
    .await
    .unwrap();
    ready(&restarted, &mario, pids[0]).await;
    let (_, current) = restarted
        .send("GET", "/api/v1/matches/current", &bowser, Value::Null)
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

    // Each keeps the other, so they can write.
    for (token, name) in [(&mario, "luigi"), (&luigi, "mario")] {
        let (status, added) = world
            .send("POST", "/api/v1/friends", token, json!({ "name": name }))
            .await;
        assert_eq!(status, StatusCode::OK, "{added}");
    }
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
    let match_of_luigi = || async {
        let (_, lists) = world
            .send("GET", "/api/v1/players", &mario, Value::Null)
            .await;
        lists["online"][0]["match"].clone()
    };
    // A game before 1.2.1 doesn't say which kind; a newer one does, and
    // a kind this server doesn't know is none.
    assert_eq!(match_of_luigi().await, Value::Null);
    for (kind, shown) in [
        ("cpu", json!("cpu")),
        ("ranked", json!("ranked")),
        ("golf", Value::Null),
    ] {
        world
            .send_with(
                "GET",
                "/api/v1/matches/current",
                &luigi,
                &[("recharged-activity", "match"), ("recharged-match", kind)],
                Value::Null,
            )
            .await;
        assert_eq!(match_of_luigi().await, shown, "{kind}");
    }
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
    assert_eq!(match_of_luigi().await, Value::Null);
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
async fn friends_are_added_by_hand_and_follow_the_player(pool: PgPool) {
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

    // Playing someone makes no friend: a challenge and its accept leave
    // both lists empty.
    let (status, challenge) = world
        .send(
            "POST",
            "/api/v1/matches",
            &mario,
            json!({"opponent": "luigi", "rule_set": 1}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{challenge}");
    let id = challenge["id"].as_i64().unwrap();
    world
        .send(
            "POST",
            &format!("/api/v1/matches/{id}/accept"),
            &luigi,
            Value::Null,
        )
        .await;
    assert!(friends(mario.clone()).await.is_empty());
    assert!(friends(luigi.clone()).await.is_empty());

    // Each adds by hand.
    for (token, name) in [(&luigi, "mario"), (&mario, "luigi")] {
        let (status, added) = world
            .send("POST", "/api/v1/friends", token, json!({ "name": name }))
            .await;
        assert_eq!(status, StatusCode::OK, "{added}");
    }
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

    // By name, then by id; online friends first, the others by when they
    // were last online; the other lists mark friends.
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

#[sqlx::test]
async fn a_player_without_discord_plays_by_its_mii(pool: PgPool) {
    let world = World::new(pool);
    let (_, server) = world.send("GET", "/api/v1/server", "", Value::Null).await;
    assert_eq!(server["sign_in"]["mii"], true);

    for name in ["", "   ", &"x".repeat(33), "two\nlines"] {
        let (status, body) = world.mii_sign_in(name, "198.51.100.1").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name:?}: {body}");
    }
    let (status, signed_in) = world.mii_sign_in("  Nelly ☆ ", "198.51.100.1").await;
    assert_eq!(status, StatusCode::OK, "{signed_in}");
    assert_eq!(signed_in["status"], "signed_in");
    let player = &signed_in["player"];
    assert_eq!(player["name"], "Nelly ☆");
    assert_eq!(player["account"], "mii");
    assert_eq!(
        (&player["discord"], &player["test"], &player["friend_code"]),
        (&Value::Null, &json!(false), &Value::Null)
    );
    let nelly_id = player["id"].as_i64().unwrap();
    let nelly = signed_in["token"].as_str().unwrap().to_owned();
    let (_, me) = world.send("GET", "/api/v1/me", &nelly, Value::Null).await;
    assert_eq!(
        (&me["account"], &me["discord_id"], &me["wfc"]),
        (&json!("mii"), &Value::Null, &json!([]))
    );

    // Nelly's game logs in to WFC: its first profile is what identifies her
    // from now on, a second one is only hers.
    let first = world.wfc_login(1, "R4QEaaaaaaa").await;
    let second = world.wfc_login(2, "R4QPaaaaaaa").await;
    for pid in [first, second] {
        let (status, body) = world.prove(&nelly, pid).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let first_code = accounts::friend_code(first, "R4QE");
    let (_, me) = world.send("GET", "/api/v1/me", &nelly, Value::Null).await;
    assert_eq!(me["friend_code"], first_code.as_str());
    assert_eq!(me["wfc"].as_array().unwrap().len(), 2);

    // Others see her as a Mii player with that friend code, and challenge
    // her by id: her name is no Discord username.
    let mario = world.sign_in("Mario").await;
    let mario_pid = world.wfc_login(3, "R4QEbbbbbbb").await;
    world.prove(&mario, mario_pid).await;
    let (_, me) = world.send("GET", "/api/v1/me", &mario, Value::Null).await;
    assert_eq!(
        (&me["account"], &me["friend_code"]),
        (&json!("test"), &Value::Null)
    );
    let (_, lists) = world
        .send("GET", "/api/v1/players", &mario, Value::Null)
        .await;
    let online = &lists["online"][0];
    assert_eq!(online["id"], nelly_id);
    assert_eq!(
        (&online["account"], &online["friend_code"]),
        (&json!("mii"), &json!(first_code))
    );
    let (status, _) = world
        .send(
            "POST",
            "/api/v1/matches",
            &mario,
            json!({"opponent": "Nelly ☆", "rule_set": 1}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, challenge) = world
        .send(
            "POST",
            "/api/v1/matches",
            &mario,
            json!({"opponent_id": nelly_id, "rule_set": 1}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{challenge}");
    assert_eq!(challenge["opponent"]["account"], "mii");
    assert_eq!(challenge["opponent"]["friend_code"], first_code.as_str());
    let (_, current) = world
        .send("GET", "/api/v1/matches/current", &nelly, Value::Null)
        .await;
    assert_eq!(current["match"]["opponent"]["account"], "test");
}

#[sqlx::test]
async fn a_mii_player_comes_back_by_its_wfc_login(pool: PgPool) {
    let world = World::new(pool);
    let (_, signed_in) = world.mii_sign_in("Nelly", "198.51.100.1").await;
    let nelly_id = signed_in["player"]["id"].clone();
    let nelly = signed_in["token"].as_str().unwrap().to_owned();
    let pid = world.wfc_login(1, "R4QEaaaaaaa").await;
    world.prove(&nelly, pid).await;
    let (status, _) = world
        .send("POST", "/api/v1/sign-out", &nelly, Value::Null)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let recover = |profile_id: i32, ticket: String| {
        let world = &world;
        async move {
            world
                .send(
                    "POST",
                    "/api/v1/sign-in/wfc",
                    "",
                    json!({"profile_id": profile_id, "login_ticket": ticket}),
                )
                .await
        }
    };
    // The game's login proves it, as for POST /api/v1/wfc.
    assert_eq!(recover(pid, "wrong".into()).await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        recover(1_999_999, "ticket-1999999".into()).await.0,
        StatusCode::FORBIDDEN
    );
    // A profile that identifies nobody signs nobody in: proving it is not
    // enough for a player with an account of its own.
    let mario = world.sign_in("Mario").await;
    let mario_pid = world.wfc_login(2, "R4QEbbbbbbb").await;
    world.prove(&mario, mario_pid).await;
    let (status, body) = recover(mario_pid, format!("ticket-{mario_pid}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    let (status, back) = recover(pid, format!("ticket-{pid}")).await;
    assert_eq!(status, StatusCode::OK, "{back}");
    assert_eq!(back["status"], "signed_in");
    assert_eq!(back["player"]["id"], nelly_id);
    assert_eq!(back["player"]["account"], "mii");
    let token = back["token"].as_str().unwrap();
    assert_ne!(token, nelly);
    let (status, me) = world.send("GET", "/api/v1/me", token, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["wfc"][0]["profile_id"], pid);

    // A second Mii player with the same Mii takes the profile, not what it
    // identifies: the login still signs Nelly in.
    let (_, other) = world.mii_sign_in("Nelly", "198.51.100.2").await;
    let other_token = other["token"].as_str().unwrap().to_owned();
    assert_eq!(world.prove(&other_token, pid).await.0, StatusCode::OK);
    let (_, other_me) = world
        .send("GET", "/api/v1/me", &other_token, Value::Null)
        .await;
    assert_eq!(other_me["friend_code"], Value::Null);
    assert_eq!(other_me["wfc"][0]["profile_id"], pid);
    let (_, again) = recover(pid, format!("ticket-{pid}")).await;
    assert_eq!(again["player"]["id"], nelly_id);
    let (_, me) = world
        .send(
            "GET",
            "/api/v1/me",
            again["token"].as_str().unwrap(),
            Value::Null,
        )
        .await;
    assert_eq!(me["wfc"][0]["profile_id"], pid);
}

#[sqlx::test]
async fn a_signed_out_launcher_learns_which_miis_have_a_profile(pool: PgPool) {
    let world = World::new(pool);
    let (_, signed_in) = world.mii_sign_in("Nelly", "198.51.100.1").await;
    let nelly = signed_in["token"].as_str().unwrap().to_owned();
    let nelly_pid = world.wfc_login(1, "R4QEaaaaaaa").await;
    world.prove(&nelly, nelly_pid).await;
    let mario = world.sign_in("Mario").await;
    let mario_pid = world.wfc_login(2, "R4QEbbbbbbb").await;
    world.prove(&mario, mario_pid).await;
    let nobody_pid = world.wfc_login(3, "R4QEccccccc").await;

    let known = |ids: Value| {
        let world = &world;
        async move {
            world
                .send(
                    "POST",
                    "/api/v1/sign-in/wfc/known",
                    "",
                    json!({ "profile_ids": ids }),
                )
                .await
        }
    };
    // Nelly's Mii signs her in again through the game; Mario's plays for a
    // player with an account; nobody's is left out. No names.
    let (status, body) = known(json!([nelly_pid, mario_pid, nobody_pid, 1_999_999])).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mut profiles = body["profiles"].as_array().unwrap().clone();
    profiles.sort_by_key(|profile| profile["profile_id"].as_i64());
    assert_eq!(
        Value::Array(profiles),
        json!([
            {"profile_id": nelly_pid, "sign_in": "game"},
            {"profile_id": mario_pid, "sign_in": "test"},
        ])
    );
    let too_many: Vec<i32> = (0..=super::mii_sign_in::MAX_KNOWN as i32).collect();
    assert_eq!(known(json!(too_many)).await.0, StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn an_address_makes_a_few_mii_players_an_hour(pool: PgPool) {
    let world = World::new(pool);
    // A refused name doesn't count.
    assert_eq!(
        world.mii_sign_in("", "198.51.100.7").await.0,
        StatusCode::BAD_REQUEST
    );
    for count in 0..super::mii_sign_in::NEW_PLAYERS_PER_WINDOW {
        let (status, body) = world
            .mii_sign_in(&format!("Mii {count}"), "198.51.100.7")
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let (status, body) = world.mii_sign_in("One more", "198.51.100.7").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        body["error"].as_str().unwrap().contains("too many"),
        "{body}"
    );
    assert_eq!(
        world.mii_sign_in("Elsewhere", "198.51.100.8").await.0,
        StatusCode::OK
    );
}
