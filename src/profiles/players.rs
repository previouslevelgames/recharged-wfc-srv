//! Players, the accounts they sign in with, and the launchers' sessions
//! (docs/profiles.md).

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};

/// How long a launcher stays signed in.
pub const SESSION_TTL: Duration = Duration::days(90);
/// `last_used_at` is written at most this often.
const USE_RESOLUTION: Duration = Duration::hours(1);
/// Session tokens: 32 random bytes in Base64url.
const TOKEN_LEN: usize = 43;

/// An account to sign in with: Discord's, or a test name. (A Mii player's
/// WFC profile is one too, made by [`bind_identity`].)
pub struct Identity<'a> {
    /// `discord` or `test`.
    pub provider: &'static str,
    pub external_id: &'a str,
    pub username: &'a str,
    pub display_name: &'a str,
    pub avatar_url: Option<&'a str>,
}

/// A player as the launcher and other players see them.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Player {
    pub id: i64,
    pub name: String,
    pub avatar_url: Option<String>,
    /// The Discord username they sign in with.
    pub discord: Option<String>,
    /// A test player (PROFILES_TEST_SIGN_IN).
    pub test: bool,
    /// What they sign in with: `discord`, `test`, or `mii` for a player
    /// without either (docs/profiles.md, "Players").
    pub account: &'static str,
    /// The friend code of the WFC profile that identifies them: a Mii
    /// player's first proven profile, once there is one.
    pub friend_code: Option<String>,
}

/// The account of a player with or without a Discord and a test identity.
fn account(discord: bool, test: bool) -> &'static str {
    match (discord, test) {
        (true, _) => "discord",
        (false, true) => "test",
        (false, false) => "mii",
    }
}

/// Signs in with `identity`: its player, made at the first sign-in. The
/// player's name and picture follow the account's, and the sign-in is a
/// heartbeat (the launcher is open).
pub async fn sign_in(pool: &PgPool, identity: &Identity<'_>) -> Result<i64, sqlx::Error> {
    loop {
        let mut transaction = pool.begin().await?;
        if let Some(player_id) = sqlx::query_scalar!(
            "UPDATE player_identities SET username = $3, avatar_url = $4
             WHERE provider = $1 AND external_id = $2
             RETURNING player_id",
            identity.provider,
            identity.external_id,
            identity.username,
            identity.avatar_url,
        )
        .fetch_optional(&mut *transaction)
        .await?
        {
            sqlx::query!(
                "UPDATE players SET display_name = $2, last_seen_at = now(), heartbeat_at = now(), activity = NULL,
                     match_kind = NULL
                 WHERE player_id = $1",
                player_id,
                identity.display_name,
            )
            .execute(&mut *transaction)
            .await?;
            transaction.commit().await?;
            return Ok(player_id);
        }

        let player_id = sqlx::query_scalar!(
            "INSERT INTO players (display_name, heartbeat_at) VALUES ($1, now()) RETURNING player_id",
            identity.display_name,
        )
        .fetch_one(&mut *transaction)
        .await?;
        // A first sign-in of the same account elsewhere may come first: then
        // start again and take its player.
        if sqlx::query_scalar!(
            "INSERT INTO player_identities (provider, external_id, player_id, username, avatar_url)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT DO NOTHING
             RETURNING player_id",
            identity.provider,
            identity.external_id,
            player_id,
            identity.username,
            identity.avatar_url,
        )
        .fetch_optional(&mut *transaction)
        .await?
        .is_some()
        {
            transaction.commit().await?;
            return Ok(player_id);
        }
    }
}

/// A new player without an account to sign in with, under the name it chose:
/// a Mii player (docs/profiles.md, "Signing in"). Its making is a heartbeat.
pub async fn create_mii(pool: &PgPool, name: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(
        "INSERT INTO players (display_name, heartbeat_at) VALUES ($1, now()) RETURNING player_id",
        name,
    )
    .fetch_one(pool)
    .await
}

/// Makes the WFC profile `profile_id`, whose friend code is `friend_code`,
/// the identity of `player_id` if that is a Mii player without one yet and
/// the profile identifies nobody else. Whether it did.
pub async fn bind_identity(
    pool: &PgPool,
    player_id: i64,
    profile_id: i32,
    friend_code: &str,
) -> Result<bool, sqlx::Error> {
    let bound = sqlx::query!(
        "INSERT INTO player_identities (provider, external_id, player_id, username)
         SELECT 'wfc', $1, $2, $3
         WHERE NOT EXISTS (SELECT 1 FROM player_identities
                           WHERE player_id = $2 AND provider IN ('discord', 'test'))
         ON CONFLICT DO NOTHING",
        profile_id.to_string(),
        player_id,
        friend_code,
    )
    .execute(pool)
    .await?
    .rows_affected();
    Ok(bound == 1)
}

/// The player the WFC profile `profile_id` identifies.
pub async fn by_wfc_identity(pool: &PgPool, profile_id: i32) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar!(
        "SELECT player_id FROM player_identities WHERE provider = 'wfc' AND external_id = $1",
        profile_id.to_string(),
    )
    .fetch_optional(pool)
    .await
}

/// How each of the WFC profiles `profile_ids` that plays for a player brings
/// that player back: `game` when it identifies the player (its WFC login
/// signs in again, [`by_wfc_identity`]), otherwise the account of the
/// player it is bound to (`discord`, `test`). Profiles of no player are left
/// out.
pub async fn sign_in_kinds(
    pool: &PgPool,
    profile_ids: &[i32],
) -> Result<Vec<(i32, String)>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT asked.profile_id AS "profile_id!",
                  CASE WHEN identity.player_id IS NOT NULL THEN 'game' ELSE account.provider END AS "kind"
           FROM unnest($1::int4[]) AS asked(profile_id)
           LEFT JOIN player_identities identity
             ON identity.provider = 'wfc' AND identity.external_id = asked.profile_id::text
           LEFT JOIN player_wfc_profiles bound ON bound.profile_id = asked.profile_id
           LEFT JOIN LATERAL (SELECT provider FROM player_identities
                              WHERE player_identities.player_id = bound.player_id
                                AND provider IN ('discord', 'test')
                              ORDER BY provider LIMIT 1) account ON true"#,
        profile_ids,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| row.kind.map(|kind| (row.profile_id, kind)))
        .collect())
}

/// Why an account was not linked to a Mii player.
#[derive(Debug, PartialEq, Eq)]
pub enum LinkRefusal {
    /// The account signs in to another player.
    Taken,
    /// The player has an account of its own already, or is gone.
    NotMii,
}

/// Links the account `identity` (Discord's) to the Mii player `player_id`,
/// which signs in with it from now on, under its name and picture, and keeps
/// its friends, sets and WFC profiles. The link is a heartbeat.
pub async fn link(
    pool: &PgPool,
    player_id: i64,
    identity: &Identity<'_>,
) -> Result<Result<(), LinkRefusal>, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    // One link at a time: the player's row stays locked until the end.
    let Some(has_account) = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM player_identities
                          WHERE player_identities.player_id = players.player_id
                            AND provider IN ('discord', 'test')) AS "has_account!"
           FROM players WHERE player_id = $1
           FOR UPDATE"#,
        player_id,
    )
    .fetch_optional(&mut *transaction)
    .await?
    else {
        return Ok(Err(LinkRefusal::NotMii));
    };
    let owner = sqlx::query_scalar!(
        "SELECT player_id FROM player_identities WHERE provider = $1 AND external_id = $2",
        identity.provider,
        identity.external_id,
    )
    .fetch_optional(&mut *transaction)
    .await?;
    match owner {
        // Linked already, e.g. by the same link a moment ago.
        Some(owner) if owner == player_id => {}
        Some(_) => return Ok(Err(LinkRefusal::Taken)),
        None if has_account => return Ok(Err(LinkRefusal::NotMii)),
        None => {
            // A first sign-in of the same account elsewhere may come first.
            if sqlx::query_scalar!(
                "INSERT INTO player_identities (provider, external_id, player_id, username, avatar_url)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT DO NOTHING
                 RETURNING player_id",
                identity.provider,
                identity.external_id,
                player_id,
                identity.username,
                identity.avatar_url,
            )
            .fetch_optional(&mut *transaction)
            .await?
            .is_none()
            {
                return Ok(Err(LinkRefusal::Taken));
            }
        }
    }
    sqlx::query!(
        "UPDATE player_identities SET username = $3, avatar_url = $4
         WHERE provider = $1 AND external_id = $2",
        identity.provider,
        identity.external_id,
        identity.username,
        identity.avatar_url,
    )
    .execute(&mut *transaction)
    .await?;
    sqlx::query!(
        "UPDATE players SET display_name = $2, last_seen_at = now(), heartbeat_at = now(), activity = NULL,
                     match_kind = NULL
         WHERE player_id = $1",
        player_id,
        identity.display_name,
    )
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(Ok(()))
}

pub async fn player(pool: &PgPool, player_id: i64) -> Result<Option<Player>, sqlx::Error> {
    let row = sqlx::query!(
        r#"SELECT players.player_id, players.display_name,
                  discord.username AS "discord?", discord.avatar_url AS "avatar_url?",
                  test.username AS "test?", wfc.username AS "friend_code?"
           FROM players
           LEFT JOIN player_identities discord
                  ON discord.player_id = players.player_id AND discord.provider = 'discord'
           LEFT JOIN player_identities test
                  ON test.player_id = players.player_id AND test.provider = 'test'
           LEFT JOIN player_identities wfc
                  ON wfc.player_id = players.player_id AND wfc.provider = 'wfc'
           WHERE players.player_id = $1"#,
        player_id,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|row| Player {
        id: row.player_id,
        name: row.display_name,
        avatar_url: row.avatar_url,
        account: account(row.discord.is_some(), row.test.is_some()),
        discord: row.discord,
        test: row.test.is_some(),
        friend_code: row.friend_code,
    }))
}

/// The player's Discord account id (the long number), which never changes;
/// none for test players. The launcher makes the player's Mii from it.
pub async fn discord_id(pool: &PgPool, player_id: i64) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar!(
        "SELECT external_id FROM player_identities WHERE player_id = $1 AND provider = 'discord'",
        player_id,
    )
    .fetch_optional(pool)
    .await
}

/// What a player's Recharged is doing (docs/profiles.md, "Activity").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    Launcher,
    /// The game, outside a match.
    Game,
    /// A match in the game: offline, classic online or a set's.
    Match,
}

impl Activity {
    /// The `Recharged-Activity` header's value; anything else (or none) is
    /// the launcher, which never sends one.
    pub fn from_header(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            Some("game") => Self::Game,
            Some("match") => Self::Match,
            _ => Self::Launcher,
        }
    }

    /// As stored in `players.activity`.
    fn column(self) -> Option<&'static str> {
        match self {
            Self::Launcher => None,
            Self::Game => Some("game"),
            Self::Match => Some("match"),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Launcher => "launcher",
            Self::Game => "game",
            Self::Match => "match",
        }
    }

    /// The `Recharged-Activity` and `Recharged-Match` headers of a request.
    pub fn from_headers(headers: &axum::http::HeaderMap) -> (Self, Option<MatchKind>) {
        let header = |name| headers.get(name).and_then(|value| value.to_str().ok());
        let activity = Self::from_header(header("recharged-activity"));
        let kind = (activity == Self::Match)
            .then(|| MatchKind::from_header(header("recharged-match")))
            .flatten();
        (activity, kind)
    }
}

/// What kind of match a player's game is in (docs/profiles.md, "Activity"),
/// from its `Recharged-Match` header; older games don't say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchKind {
    /// Online, matched with anybody.
    Ranked,
    /// Online (or on a LAN) with friends.
    Friends,
    /// An online tournament.
    Tournament,
    /// Domination against the computer (or the computer against itself).
    Cpu,
    /// Domination with players on both teams at one computer.
    Local,
    /// Road to the Striker Cup.
    Cup,
    /// A Striker Challenge.
    Challenge,
    /// A Strikers 101 lesson.
    Training,
}

impl MatchKind {
    const ALL: [Self; 8] = [
        Self::Ranked,
        Self::Friends,
        Self::Tournament,
        Self::Cpu,
        Self::Local,
        Self::Cup,
        Self::Challenge,
        Self::Training,
    ];

    /// None for no header and anything this server doesn't know.
    pub fn from_header(value: Option<&str>) -> Option<Self> {
        let value = value?.trim();
        Self::ALL.into_iter().find(|kind| kind.name() == value)
    }

    /// As the header, `players.match_kind` and the lists have it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Ranked => "ranked",
            Self::Friends => "friends",
            Self::Tournament => "tournament",
            Self::Cpu => "cpu",
            Self::Local => "local",
            Self::Cup => "cup",
            Self::Challenge => "challenge",
            Self::Training => "training",
        }
    }
}

/// `player_id`'s launcher or game made a request: they have Recharged open,
/// doing `activity`, in a match of `kind` (written at most every few
/// seconds, and at once when either changes).
pub async fn heartbeat(
    pool: &PgPool,
    player_id: i64,
    (activity, kind): (Activity, Option<MatchKind>),
) -> Result<(), sqlx::Error> {
    let kind = (activity == Activity::Match).then_some(kind).flatten();
    sqlx::query!(
        "UPDATE players SET heartbeat_at = now(), activity = $2, match_kind = $3
         WHERE player_id = $1
           AND (heartbeat_at IS NULL OR heartbeat_at < now() - interval '5 seconds'
                OR activity IS DISTINCT FROM $2 OR match_kind IS DISTINCT FROM $3)",
        player_id,
        activity.column(),
        kind.map(MatchKind::name),
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Whether `player_id` has Recharged open.
pub async fn is_online(pool: &PgPool, player_id: i64) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query_scalar!(
        "SELECT 1 AS \"online!\" FROM players
         WHERE player_id = $1 AND heartbeat_at > now() - make_interval(secs => $2)",
        player_id,
        f64::from(super::matches::HEARTBEAT_TIMEOUT_SECS),
    )
    .fetch_optional(pool)
    .await?
    .is_some())
}

/// What `player_id`'s Recharged is doing; none while it is closed.
pub async fn activity(pool: &PgPool, player_id: i64) -> Result<Option<Activity>, sqlx::Error> {
    Ok(sqlx::query!(
        "SELECT activity FROM players
         WHERE player_id = $1 AND heartbeat_at > now() - make_interval(secs => $2)",
        player_id,
        f64::from(super::matches::HEARTBEAT_TIMEOUT_SECS),
    )
    .fetch_optional(pool)
    .await?
    .map(|row| Activity::from_header(row.activity.as_deref())))
}

/// A player in the launcher's lists: whether they have Recharged open and a
/// set to play, when they last had it open, and whether the player who asks
/// keeps them as a friend.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Listed {
    #[serde(flatten)]
    pub player: Player,
    pub online: bool,
    pub busy: bool,
    /// While online: "launcher", "game" or "match" (see [`Activity`]).
    pub activity: Option<&'static str>,
    /// In a match: its kind, when the game said (see [`MatchKind`]).
    #[serde(rename = "match")]
    pub match_kind: Option<&'static str>,
    /// Their last heartbeat in Unix seconds; none before the first.
    pub last_online: Option<i64>,
    pub friend: bool,
    /// The two keep each other as friends: they can write each other
    /// (docs/profiles.md, "Chat").
    pub chat: bool,
    /// Their messages the player has not read.
    pub unread: i64,
    /// Their Discord account id, for a friend the player can chat with.
    pub discord_id: Option<String>,
}

/// Who has Recharged open, but `player_id`, by name.
pub async fn online(pool: &PgPool, player_id: i64) -> Result<Vec<Listed>, sqlx::Error> {
    let ids = sqlx::query_scalar!(
        "SELECT player_id FROM players
         WHERE heartbeat_at > now() - make_interval(secs => $2) AND player_id <> $1
         ORDER BY lower(display_name), player_id
         LIMIT 100",
        player_id,
        f64::from(super::matches::HEARTBEAT_TIMEOUT_SECS),
    )
    .fetch_all(pool)
    .await?;
    listed(pool, player_id, &ids).await
}

/// Whether `player_id` is available for random games.
pub async fn available(pool: &PgPool, player_id: i64) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar!(
        "SELECT available FROM players WHERE player_id = $1",
        player_id
    )
    .fetch_one(pool)
    .await
}

pub async fn set_available(
    pool: &PgPool,
    player_id: i64,
    available: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE players SET available = $2 WHERE player_id = $1",
        player_id,
        available
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// The other players with Recharged open who are available for random games,
/// by name, as `player_id` sees them.
pub async fn available_now(pool: &PgPool, player_id: i64) -> Result<Vec<Listed>, sqlx::Error> {
    let ids = sqlx::query_scalar!(
        "SELECT player_id FROM players
         WHERE available AND heartbeat_at > now() - make_interval(secs => $2) AND player_id <> $1
         ORDER BY lower(display_name), player_id
         LIMIT 100",
        player_id,
        f64::from(super::matches::HEARTBEAT_TIMEOUT_SECS),
    )
    .fetch_all(pool)
    .await?;
    listed(pool, player_id, &ids).await
}

/// The last 10 players `player_id` played a set with, the latest first.
pub async fn recent_opponents(pool: &PgPool, player_id: i64) -> Result<Vec<Listed>, sqlx::Error> {
    let ids = sqlx::query_scalar!(
        r#"SELECT opponent AS "opponent!" FROM (
               SELECT CASE WHEN home = $1 THEN away ELSE home END AS opponent, updated_at
               FROM matches
               WHERE (home = $1 OR away = $1) AND state IN ('finished', 'disputed')
           ) AS played
           GROUP BY opponent
           ORDER BY max(updated_at) DESC
           LIMIT 10"#,
        player_id,
    )
    .fetch_all(pool)
    .await?;
    listed(pool, player_id, &ids).await
}

/// The most Discord accounts one [`by_discord`] looks up.
pub const MAX_DISCORD_LOOKUP: usize = 200;

/// The players among the Discord accounts `discord_ids` (the launcher's
/// Discord friends), but `viewer`, by name, each with its Discord account id.
pub async fn by_discord(
    pool: &PgPool,
    viewer: i64,
    discord_ids: &[String],
) -> Result<Vec<Listed>, sqlx::Error> {
    let rows = sqlx::query!(
        "SELECT player_identities.player_id, player_identities.external_id
         FROM player_identities
         JOIN players ON players.player_id = player_identities.player_id
         WHERE player_identities.provider = 'discord'
           AND player_identities.external_id = ANY($1)
           AND player_identities.player_id <> $2
         ORDER BY lower(players.display_name), players.player_id",
        discord_ids,
        viewer,
    )
    .fetch_all(pool)
    .await?;
    let ids: Vec<i64> = rows.iter().map(|row| row.player_id).collect();
    let mut found = listed(pool, viewer, &ids).await?;
    for entry in &mut found {
        entry.discord_id = rows
            .iter()
            .find(|row| row.player_id == entry.player.id)
            .map(|row| row.external_id.clone());
    }
    Ok(found)
}

/// The players `ids`, in that order, as `viewer` sees them.
pub(super) async fn listed(
    pool: &PgPool,
    viewer: i64,
    ids: &[i64],
) -> Result<Vec<Listed>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT players.player_id, players.display_name,
                  discord.username AS "discord?", discord.avatar_url AS "avatar_url?",
                  test.username AS "test?",
                  coalesce(players.heartbeat_at > now() - make_interval(secs => $2), false)
                      AS "online!",
                  players.activity, players.match_kind,
                  EXISTS (SELECT 1 FROM matches
                          WHERE state IN ('challenged', 'accepted')
                            AND (home = players.player_id OR away = players.player_id))
                      AS "busy!",
                  extract(epoch FROM players.heartbeat_at)::bigint AS last_online,
                  EXISTS (SELECT 1 FROM friends
                          WHERE friends.player_id = $3 AND friends.friend_id = players.player_id)
                      AS "friend!",
                  EXISTS (SELECT 1 FROM friends
                          WHERE friends.player_id = players.player_id AND friends.friend_id = $3)
                      AS "keeps_you!",
                  (SELECT count(*) FROM chat_messages
                   WHERE chat_messages.sender = players.player_id AND chat_messages.recipient = $3
                     AND chat_messages.read_at IS NULL) AS "unread!",
                  discord.external_id AS "discord_id?", wfc.username AS "friend_code?"
           FROM players
           LEFT JOIN player_identities discord
                  ON discord.player_id = players.player_id AND discord.provider = 'discord'
           LEFT JOIN player_identities test
                  ON test.player_id = players.player_id AND test.provider = 'test'
           LEFT JOIN player_identities wfc
                  ON wfc.player_id = players.player_id AND wfc.provider = 'wfc'
           WHERE players.player_id = ANY($1)"#,
        ids,
        f64::from(super::matches::HEARTBEAT_TIMEOUT_SECS),
        viewer,
    )
    .fetch_all(pool)
    .await?;
    let mut listed: Vec<Listed> = rows
        .into_iter()
        .map(|row| Listed {
            player: Player {
                id: row.player_id,
                name: row.display_name,
                avatar_url: row.avatar_url,
                account: account(row.discord.is_some(), row.test.is_some()),
                discord: row.discord,
                test: row.test.is_some(),
                friend_code: row.friend_code,
            },
            online: row.online,
            busy: row.busy,
            activity: row
                .online
                .then(|| Activity::from_header(row.activity.as_deref()).name()),
            match_kind: row
                .online
                .then(|| MatchKind::from_header(row.match_kind.as_deref()))
                .flatten()
                .map(MatchKind::name),
            last_online: row.last_online,
            friend: row.friend,
            chat: row.friend && row.keeps_you,
            unread: row.unread,
            discord_id: (row.friend && row.keeps_you)
                .then_some(row.discord_id)
                .flatten(),
        })
        .collect();
    listed.sort_by_key(|entry| ids.iter().position(|id| *id == entry.player.id));
    Ok(listed)
}

/// The player who signs in with `name`: a Discord username, or on a test
/// server a test name. Mii players have neither: they are found by id.
pub async fn find(pool: &PgPool, name: &str) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar!(
        "SELECT player_id FROM player_identities
         WHERE lower(username) = lower($1) AND provider IN ('discord', 'test')
         ORDER BY provider = 'discord' DESC
         LIMIT 1",
        name.trim(),
    )
    .fetch_optional(pool)
    .await
}

/// A WFC profile of a player's (docs/profiles.md, "Friend codes").
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WfcProfile {
    pub profile_id: i32,
    /// The release: `R4QE` or `R4QP`, from the profile's `gsbrcd`.
    pub game: String,
    pub friend_code: String,
    /// The Mii name of its last login.
    pub mii: Option<String>,
}

/// Makes `profile_id` `player_id`'s, whoever's it was.
pub async fn bind_wfc(pool: &PgPool, player_id: i64, profile_id: i32) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO player_wfc_profiles (profile_id, player_id) VALUES ($1, $2)
         ON CONFLICT (profile_id) DO UPDATE SET player_id = $2, bound_at = now()",
        profile_id,
        player_id,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// `player_id`'s WFC profiles, the latest bound first.
pub async fn wfc_profiles(pool: &PgPool, player_id: i64) -> Result<Vec<WfcProfile>, sqlx::Error> {
    let rows = sqlx::query!(
        "SELECT profiles.profile_id, profiles.gsbrcd, profiles.ingamesn
         FROM player_wfc_profiles
         JOIN profiles USING (profile_id)
         WHERE player_wfc_profiles.player_id = $1
         ORDER BY player_wfc_profiles.bound_at DESC",
        player_id,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let game: String = row.gsbrcd.chars().take(4).collect();
            WfcProfile {
                friend_code: crate::accounts::friend_code(row.profile_id, &game),
                profile_id: row.profile_id,
                game,
                mii: row.ingamesn,
            }
        })
        .collect())
}

/// Opens a launcher session for `player_id`. Returns its token, which only
/// the launcher keeps.
pub async fn open_session(pool: &PgPool, player_id: i64) -> Result<String, sqlx::Error> {
    let token = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
    let token_hash = hash(&token);
    sqlx::query!(
        "INSERT INTO player_sessions (token_hash, player_id, expires_at) VALUES ($1, $2, $3)",
        token_hash.as_slice(),
        player_id,
        OffsetDateTime::now_utc() + SESSION_TTL,
    )
    .execute(pool)
    .await?;
    Ok(token)
}

/// The player signed in with `token`, unless the session expired or was
/// closed.
pub async fn session(pool: &PgPool, token: &str) -> Result<Option<i64>, sqlx::Error> {
    if token.len() != TOKEN_LEN {
        return Ok(None);
    }
    let token_hash = hash(token);
    let Some(row) = sqlx::query!(
        "SELECT player_id, last_used_at FROM player_sessions
         WHERE token_hash = $1 AND expires_at > now()",
        token_hash.as_slice(),
    )
    .fetch_optional(pool)
    .await?
    else {
        return Ok(None);
    };
    if OffsetDateTime::now_utc() - row.last_used_at > USE_RESOLUTION {
        sqlx::query!(
            "UPDATE player_sessions SET last_used_at = now() WHERE token_hash = $1",
            token_hash.as_slice(),
        )
        .execute(pool)
        .await?;
        sqlx::query!(
            "UPDATE players SET last_seen_at = now() WHERE player_id = $1",
            row.player_id,
        )
        .execute(pool)
        .await?;
    }
    Ok(Some(row.player_id))
}

/// Closes the session of `token`. Whether there was one.
pub async fn close_session(pool: &PgPool, token: &str) -> Result<bool, sqlx::Error> {
    let token_hash = hash(token);
    let closed = sqlx::query!(
        "DELETE FROM player_sessions WHERE token_hash = $1",
        token_hash.as_slice(),
    )
    .execute(pool)
    .await?
    .rows_affected();
    Ok(closed == 1)
}

fn hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn discord<'a>(id: &'a str, username: &'a str, name: &'a str) -> Identity<'a> {
        Identity {
            provider: "discord",
            external_id: id,
            username,
            display_name: name,
            avatar_url: Some("https://cdn.discordapp.com/embed/avatars/0.png"),
        }
    }

    #[sqlx::test]
    async fn an_account_keeps_its_player(pool: PgPool) {
        let first = sign_in(&pool, &discord("1", "nelly", "Nelly"))
            .await
            .unwrap();
        let again = sign_in(&pool, &discord("1", "nelly2", "Nelly 2"))
            .await
            .unwrap();
        let other = sign_in(&pool, &discord("2", "sam", "Sam")).await.unwrap();
        assert_eq!(first, again);
        assert_ne!(first, other);

        let player = player(&pool, first).await.unwrap().unwrap();
        assert_eq!(player.name, "Nelly 2");
        assert_eq!(player.discord.as_deref(), Some("nelly2"));
        assert!(!player.test);
    }

    #[sqlx::test]
    async fn players_are_found_by_their_username(pool: PgPool) {
        let nelly = sign_in(&pool, &discord("1", "nelly", "Nelly"))
            .await
            .unwrap();
        assert_eq!(find(&pool, " Nelly ").await.unwrap(), Some(nelly));
        assert_eq!(find(&pool, "nobody").await.unwrap(), None);
    }

    #[sqlx::test]
    async fn a_wfc_profile_is_its_last_players(pool: PgPool) {
        crate::accounts::log_in(&pool, 42).await.unwrap();
        let profile =
            crate::accounts::profile_login(&pool, 42, "R4QEabcdefg", Some("Nelly"), "10.0.0.1")
                .await
                .unwrap();
        let nelly = sign_in(&pool, &discord("1", "nelly", "Nelly"))
            .await
            .unwrap();
        let sam = sign_in(&pool, &discord("2", "sam", "Sam")).await.unwrap();

        bind_wfc(&pool, nelly, profile.profile_id).await.unwrap();
        let bound = wfc_profiles(&pool, nelly).await.unwrap();
        assert_eq!(
            bound,
            [WfcProfile {
                profile_id: profile.profile_id,
                game: "R4QE".into(),
                friend_code: "0868-9934-5920".into(),
                mii: Some("Nelly".into()),
            }]
        );
        bind_wfc(&pool, sam, profile.profile_id).await.unwrap();
        assert!(wfc_profiles(&pool, nelly).await.unwrap().is_empty());
        assert_eq!(wfc_profiles(&pool, sam).await.unwrap().len(), 1);
    }

    #[sqlx::test]
    async fn a_mii_players_first_proven_profile_identifies_it(pool: PgPool) {
        let mut profiles = Vec::new();
        for (user_id, gsbrcd) in [
            (42, "R4QEabcdefg"),
            (43, "R4QPabcdefg"),
            (44, "R4QEhijklmn"),
        ] {
            crate::accounts::log_in(&pool, user_id).await.unwrap();
            let profile =
                crate::accounts::profile_login(&pool, user_id, gsbrcd, Some("Mii"), "10.0.0.1")
                    .await
                    .unwrap();
            let game = &gsbrcd[..4];
            profiles.push((
                profile.profile_id,
                crate::accounts::friend_code(profile.profile_id, game),
            ));
        }
        let [
            (first, first_code),
            (second, second_code),
            (third, third_code),
        ] = profiles.try_into().unwrap();

        let mii = create_mii(&pool, "Nelly").await.unwrap();
        let other_mii = create_mii(&pool, "Nelly").await.unwrap();
        assert_ne!(mii, other_mii);
        let nelly = player(&pool, mii).await.unwrap().unwrap();
        assert_eq!(
            (
                nelly.name.as_str(),
                nelly.account,
                nelly.discord,
                nelly.test
            ),
            ("Nelly", "mii", None, false)
        );
        assert_eq!(nelly.friend_code, None);

        // The first profile only, and a profile identifies one player.
        assert!(bind_identity(&pool, mii, first, &first_code).await.unwrap());
        assert!(
            !bind_identity(&pool, mii, second, &second_code)
                .await
                .unwrap()
        );
        assert!(
            !bind_identity(&pool, other_mii, first, &first_code)
                .await
                .unwrap()
        );
        assert_eq!(by_wfc_identity(&pool, first).await.unwrap(), Some(mii));
        assert_eq!(by_wfc_identity(&pool, second).await.unwrap(), None);
        let nelly = player(&pool, mii).await.unwrap().unwrap();
        assert_eq!(nelly.friend_code.as_deref(), Some(first_code.as_str()));
        assert_eq!(
            listed(&pool, other_mii, &[mii]).await.unwrap()[0].player,
            nelly
        );

        // Players with an account of their own get none.
        let sam = sign_in(&pool, &discord("2", "sam", "Sam")).await.unwrap();
        assert!(!bind_identity(&pool, sam, third, &third_code).await.unwrap());
        assert_eq!(
            player(&pool, sam).await.unwrap().unwrap().account,
            "discord"
        );

        // Neither the Mii player's name nor its friend code finds it.
        assert_eq!(find(&pool, "Nelly").await.unwrap(), None);
        assert_eq!(find(&pool, &first_code).await.unwrap(), None);
        assert_eq!(find(&pool, "sam").await.unwrap(), Some(sam));
    }

    #[sqlx::test]
    async fn discord_links_to_a_mii_player_once(pool: PgPool) {
        let mii = create_mii(&pool, "Nelly").await.unwrap();
        let other_mii = create_mii(&pool, "Kiki").await.unwrap();
        let nelly = discord("1", "nelly", "Nelly D.");

        assert_eq!(link(&pool, mii, &nelly).await.unwrap(), Ok(()));
        let linked = player(&pool, mii).await.unwrap().unwrap();
        assert_eq!(
            (
                linked.name.as_str(),
                linked.account,
                linked.discord.as_deref()
            ),
            ("Nelly D.", "discord", Some("nelly"))
        );
        // Signing in with Discord is that player now; the same link again
        // changes nothing.
        assert_eq!(sign_in(&pool, &nelly).await.unwrap(), mii);
        assert_eq!(link(&pool, mii, &nelly).await.unwrap(), Ok(()));

        // Another player's Discord account, or a second account: refused.
        assert_eq!(
            link(&pool, other_mii, &nelly).await.unwrap(),
            Err(LinkRefusal::Taken)
        );
        assert_eq!(
            link(&pool, mii, &discord("2", "sam", "Sam")).await.unwrap(),
            Err(LinkRefusal::NotMii)
        );
        assert_eq!(
            link(&pool, 999_999, &discord("3", "bo", "Bo"))
                .await
                .unwrap(),
            Err(LinkRefusal::NotMii)
        );
        let kiki = player(&pool, other_mii).await.unwrap().unwrap();
        assert_eq!((kiki.name.as_str(), kiki.account), ("Kiki", "mii"));
        assert_eq!(find(&pool, "sam").await.unwrap(), None);
    }

    #[sqlx::test]
    async fn a_session_lasts_until_it_is_closed(pool: PgPool) {
        let player_id = sign_in(&pool, &discord("1", "nelly", "Nelly"))
            .await
            .unwrap();
        let token = open_session(&pool, player_id).await.unwrap();
        assert_eq!(token.len(), TOKEN_LEN);
        assert_eq!(session(&pool, &token).await.unwrap(), Some(player_id));
        assert_eq!(session(&pool, "another token").await.unwrap(), None);
        assert!(close_session(&pool, &token).await.unwrap());
        assert_eq!(session(&pool, &token).await.unwrap(), None);
        assert!(!close_session(&pool, &token).await.unwrap());
    }
}
