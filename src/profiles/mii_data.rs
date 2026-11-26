//! The players' Miis (docs/profiles.md, "Miis"): each WFC profile's
//! RFLCharData, which only the launcher of the player the profile is bound to
//! sends, and which the launchers and games show players by. A Wii never
//! sends one: its profiles have none.

use std::{collections::HashMap, sync::Arc};

use axum::{
    Json,
    extract::{State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::json;
use sqlx::PgPool;
use tracing::info;

use super::{ProfilesState, database_error, json_error};

/// RFLCharData: a Mii as the game keeps it, without the CRC of its store
/// data.
pub const MII_DATA_LEN: usize = 74;
/// The most Miis one upload sends: a game save holds fewer.
pub const MAX_UPLOAD: usize = 32;
/// A Mii's slot of the game's online save (GameInfoSaveSlot), as the port
/// writes it.
pub const SLOT_LEN: usize = 0xB10;

#[derive(Deserialize)]
pub(super) struct Upload {
    miis: Vec<Uploaded>,
}

#[derive(Deserialize)]
struct Uploaded {
    profile_id: i64,
    /// Standard Base64.
    data: String,
    /// The Mii's slot of the online save, standard Base64 (from 1.3.1).
    #[serde(default)]
    slot: Option<String>,
}

/// `POST /api/v1/me/miis`: the player's launcher sends the Miis of their WFC
/// profiles. Only those of profiles bound to the player are stored, each
/// replacing the one before; the others are left out without an error.
pub(super) async fn upload(
    State(state): State<Arc<ProfilesState>>,
    headers: HeaderMap,
    request: Result<Json<Upload>, JsonRejection>,
) -> Response {
    let player = match state.signed_in(&headers).await {
        Ok(player) => player,
        Err(response) => return response,
    };
    let Upload { miis } = match request {
        Ok(Json(upload)) => upload,
        Err(rejection) => return json_error(StatusCode::BAD_REQUEST, &rejection.body_text()),
    };
    if miis.len() > MAX_UPLOAD {
        return json_error(
            StatusCode::BAD_REQUEST,
            &format!("at most {MAX_UPLOAD} Miis at once"),
        );
    }
    // One per profile: the last good one sent.
    let slots: HashMap<i32, Vec<u8>> = miis
        .iter()
        .filter_map(|mii| {
            Some((
                i32::try_from(mii.profile_id).ok()?,
                decode_slot(mii.slot.as_deref()?)?,
            ))
        })
        .collect();
    let sent: HashMap<i32, Vec<u8>> = miis
        .into_iter()
        .filter_map(|mii| Some((i32::try_from(mii.profile_id).ok()?, decode(&mii.data)?)))
        .collect();
    let sent: Vec<(i32, Vec<u8>)> = sent.into_iter().collect();
    let slots: Vec<(i32, Vec<u8>)> = slots.into_iter().collect();
    let stored = match store(&state.pool, player.id, &sent).await {
        Ok(stored) => stored,
        Err(error) => return database_error(error),
    };
    match store_slots(&state.pool, player.id, &slots).await {
        Ok(slots_stored) => {
            info!(target: "profiles", player_id = player.id, sent = sent.len(), stored, slots_stored, "Miis stored");
            Json(json!({ "stored": stored, "slots": slots_stored })).into_response()
        }
        Err(error) => database_error(error),
    }
}

/// `data` as a Mii: Base64 of [`MII_DATA_LEN`] bytes, not all zero (an
/// empty slot).
pub fn decode(data: &str) -> Option<Vec<u8>> {
    let bytes = STANDARD.decode(data.trim()).ok()?;
    (bytes.len() == MII_DATA_LEN && bytes.iter().any(|&byte| byte != 0)).then_some(bytes)
}

/// `data` as a slot: Base64 of [`SLOT_LEN`] bytes, not all zero.
pub fn decode_slot(data: &str) -> Option<Vec<u8>> {
    let bytes = STANDARD.decode(data.trim()).ok()?;
    (bytes.len() == SLOT_LEN && bytes.iter().any(|&byte| byte != 0)).then_some(bytes)
}

/// A Mii as the API shows it: standard Base64.
pub fn encode(data: &[u8]) -> String {
    STANDARD.encode(data)
}

/// Stores the Miis `miis` (profile, data) of those profiles that are bound to
/// `player_id`, replacing what they had. How many it stored.
pub async fn store(
    pool: &PgPool,
    player_id: i64,
    miis: &[(i32, Vec<u8>)],
) -> Result<u64, sqlx::Error> {
    let (profile_ids, data): (Vec<i32>, Vec<Vec<u8>>) = miis.iter().cloned().unzip();
    let stored = sqlx::query!(
        "UPDATE profiles SET mii_data = sent.data, mii_stored_at = now()
         FROM unnest($2::int4[], $3::bytea[]) AS sent(profile_id, data)
         JOIN player_wfc_profiles bound ON bound.profile_id = sent.profile_id
         WHERE profiles.profile_id = sent.profile_id AND bound.player_id = $1",
        player_id,
        &profile_ids,
        &data,
    )
    .execute(pool)
    .await?
    .rows_affected();
    Ok(stored)
}

/// Stores the slots `slots` (profile, data) of those profiles that are bound
/// to `player_id`, replacing what they had. How many it stored.
pub async fn store_slots(
    pool: &PgPool,
    player_id: i64,
    slots: &[(i32, Vec<u8>)],
) -> Result<u64, sqlx::Error> {
    if slots.is_empty() {
        return Ok(0);
    }
    let (profile_ids, data): (Vec<i32>, Vec<Vec<u8>>) = slots.iter().cloned().unzip();
    let stored = sqlx::query!(
        "UPDATE profiles SET slot_data = sent.data, slot_stored_at = now()
         FROM unnest($2::int4[], $3::bytea[]) AS sent(profile_id, data)
         JOIN player_wfc_profiles bound ON bound.profile_id = sent.profile_id
         WHERE profiles.profile_id = sent.profile_id AND bound.player_id = $1",
        player_id,
        &profile_ids,
        &data,
    )
    .execute(pool)
    .await?
    .rows_affected();
    Ok(stored)
}

/// The Miis of the players `player_ids`, in Base64: for each, their profile's
/// own face, else the Mii of their bound WFC profile that logged in last, of
/// those with one. Players without one are left out.
pub async fn of_players(
    pool: &PgPool,
    player_ids: &[i64],
) -> Result<HashMap<i64, String>, sqlx::Error> {
    if player_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = sqlx::query!(
        r#"SELECT players.player_id, COALESCE(players.mii_data, latest.mii_data) AS "mii_data?"
           FROM players
           LEFT JOIN LATERAL (
               SELECT profiles.mii_data
               FROM player_wfc_profiles bound
               JOIN profiles ON profiles.profile_id = bound.profile_id
               WHERE bound.player_id = players.player_id AND profiles.mii_data IS NOT NULL
               ORDER BY profiles.last_login_at DESC, profiles.profile_id DESC
               LIMIT 1) latest ON true
           WHERE players.player_id = ANY($1)"#,
        player_ids,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| Some((row.player_id, encode(&row.mii_data?))))
        .collect())
}

/// `player_id`'s profile shows by `data` from now on (docs/profiles.md,
/// "Miis").
pub async fn choose(pool: &PgPool, player_id: i64, data: &[u8]) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE players SET mii_data = $2, mii_stored_at = now() WHERE player_id = $1",
        player_id,
        data,
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[derive(Deserialize)]
pub(super) struct Chosen {
    /// Standard Base64 of the RFLCharData.
    data: String,
}

/// `PUT /api/v1/me/mii`: the profile's own face, which the player chose or
/// edited in the launcher. The answer is the player.
pub(super) async fn choose_mii(
    State(state): State<Arc<ProfilesState>>,
    headers: HeaderMap,
    request: Result<Json<Chosen>, JsonRejection>,
) -> Response {
    let player = match state.signed_in(&headers).await {
        Ok(player) => player,
        Err(response) => return response,
    };
    let Chosen { data } = match request {
        Ok(Json(chosen)) => chosen,
        Err(rejection) => return json_error(StatusCode::BAD_REQUEST, &rejection.body_text()),
    };
    let Some(data) = decode(&data) else {
        return json_error(
            StatusCode::BAD_REQUEST,
            &format!("a Mii is Base64 of {MII_DATA_LEN} bytes, not all zero"),
        );
    };
    if let Err(error) = choose(&state.pool, player.id, &data).await {
        return database_error(error);
    }
    info!(target: "profiles", player_id = player.id, "profile's Mii chosen");
    match super::players::player(&state.pool, player.id).await {
        Ok(Some(player)) => Json(player).into_response(),
        Ok(None) => json_error(StatusCode::UNAUTHORIZED, "signed out"),
        Err(error) => database_error(error),
    }
}

/// `GET /api/v1/me/restore`: what a fresh install needs to be the player's
/// again (docs/profiles.md, "Restoring"): the profile's face and the slots of
/// their bound WFC profiles, the latest bound first, each with its game and
/// friend code.
pub(super) async fn restore(
    State(state): State<Arc<ProfilesState>>,
    headers: HeaderMap,
) -> Response {
    let player = match state.signed_in(&headers).await {
        Ok(player) => player,
        Err(response) => return response,
    };
    let face = match sqlx::query_scalar!(
        "SELECT mii_data FROM players WHERE player_id = $1",
        player.id
    )
    .fetch_one(&state.pool)
    .await
    {
        Ok(face) => face,
        Err(error) => return database_error(error),
    };
    let rows = match sqlx::query!(
        r#"SELECT profiles.profile_id, profiles.gsbrcd, profiles.slot_data AS "slot_data!"
           FROM player_wfc_profiles bound
           JOIN profiles USING (profile_id)
           WHERE bound.player_id = $1 AND profiles.slot_data IS NOT NULL
           ORDER BY bound.bound_at DESC, profiles.profile_id DESC"#,
        player.id
    )
    .fetch_all(&state.pool)
    .await
    {
        Ok(rows) => rows,
        Err(error) => return database_error(error),
    };
    let slots: Vec<_> = rows
        .into_iter()
        .map(|row| {
            let game: String = row.gsbrcd.chars().take(4).collect();
            json!({
                "profile_id": row.profile_id,
                "friend_code": crate::accounts::friend_code(row.profile_id, &game),
                "game": game,
                "data": encode(&row.slot_data),
            })
        })
        .collect();
    Json(json!({ "mii": face.as_deref().map(encode), "slots": slots })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::players::{self, Identity};

    async fn player(pool: &PgPool, name: &str) -> i64 {
        players::sign_in(
            pool,
            &Identity {
                provider: "test",
                external_id: name,
                username: name,
                display_name: name,
                avatar_url: None,
            },
        )
        .await
        .unwrap()
    }

    async fn profile(pool: &PgPool, user_id: i64, gsbrcd: &str) -> i32 {
        crate::accounts::log_in(pool, user_id).await.unwrap();
        crate::accounts::profile_login(pool, user_id, gsbrcd, Some("Mii"), "10.0.0.1")
            .await
            .unwrap()
            .profile_id
    }

    #[sqlx::test]
    async fn a_players_mii_is_the_one_of_their_last_login(pool: PgPool) {
        let (nelly, sam, kiki) = (
            player(&pool, "nelly").await,
            player(&pool, "sam").await,
            player(&pool, "kiki").await,
        );
        let first = profile(&pool, 1, "R4QEaaaaaaa").await;
        let second = profile(&pool, 2, "R4QPaaaaaaa").await;
        let without = profile(&pool, 3, "R4QEbbbbbbb").await;
        let sams = profile(&pool, 4, "R4QEccccccc").await;
        for (owner, pid) in [
            (nelly, first),
            (nelly, second),
            (nelly, without),
            (sam, sams),
        ] {
            players::bind_wfc(&pool, owner, pid).await.unwrap();
        }
        let miis = [(first, vec![1; 74]), (second, vec![2; 74])];
        assert_eq!(store(&pool, nelly, &miis).await.unwrap(), 2);
        // Sam's own profile, but not Nelly's.
        let sent = [(sams, vec![3; 74]), (first, vec![4; 74])];
        assert_eq!(store(&pool, sam, &sent).await.unwrap(), 1);

        // `without` logged in last, but has no Mii: `second` did before it.
        let shown = of_players(&pool, &[nelly, sam, kiki]).await.unwrap();
        assert_eq!(shown.len(), 2);
        assert_eq!(shown[&nelly], encode(&[2; 74]));
        assert_eq!(shown[&sam], encode(&[3; 74]));
        profile(&pool, 1, "R4QEaaaaaaa").await;
        let shown = of_players(&pool, &[nelly]).await.unwrap();
        assert_eq!(shown[&nelly], encode(&[1; 74]));

        // The Mii goes with its profile to the player who proves it next.
        players::bind_wfc(&pool, kiki, first).await.unwrap();
        let shown = of_players(&pool, &[nelly, kiki]).await.unwrap();
        assert_eq!(
            (&shown[&nelly], &shown[&kiki]),
            (&encode(&[2; 74]), &encode(&[1; 74]))
        );
        assert!(of_players(&pool, &[]).await.unwrap().is_empty());

        // The profile's own face comes first, whatever logged in last.
        choose(&pool, nelly, &[5; 74]).await.unwrap();
        let shown = of_players(&pool, &[nelly, kiki]).await.unwrap();
        assert_eq!(
            (&shown[&nelly], &shown[&kiki]),
            (&encode(&[5; 74]), &encode(&[1; 74]))
        );
        choose(&pool, sam, &[6; 74]).await.unwrap();
        assert_eq!(
            of_players(&pool, &[sam]).await.unwrap()[&sam],
            encode(&[6; 74])
        );
    }

    #[test]
    fn a_mii_is_74_bytes_that_are_not_all_zero() {
        let mii: Vec<u8> = (1..=74).collect();
        let sent = encode(&mii);
        assert_eq!(sent.len(), 100);
        assert_eq!(decode(&sent), Some(mii));
        for refused in [
            encode(&[0; 74]),
            encode(&[1; 73]),
            encode(&[1; 75]),
            "not base64!".into(),
            String::new(),
        ] {
            assert_eq!(decode(&refused), None, "{refused}");
        }
    }
}
