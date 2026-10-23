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
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json");
        if !token.is_empty() {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
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
    assert_eq!(current, json!({"match": null}));
}
