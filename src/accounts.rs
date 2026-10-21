//! Accounts and profiles (docs/login.md): who may log in. NAS registers an
//! account per console; GPCM makes a profile per account and game.

use sqlx::PgPool;

/// DWC keeps 43 bits of a userid.
pub const USER_ID_LIMIT: i64 = 1 << 43;
/// The CPU buddy's profile, made by a migration (docs/login.md): a friend to
/// add before a second console is online.
pub const CPU_BUDDY: i32 = 999_999_999;
/// The CPU buddy's account.
pub const CPU_BUDDY_ACCOUNT: i64 = 1;

/// Registers a new account. Returns its userid.
pub async fn register(pool: &PgPool) -> Result<i64, sqlx::Error> {
    loop {
        let user_id = rand::random_range(0..USER_ID_LIMIT);
        let registered = sqlx::query!(
            "INSERT INTO accounts (user_id) VALUES ($1) ON CONFLICT DO NOTHING",
            user_id,
        )
        .execute(pool)
        .await?
        .rows_affected();
        if registered == 1 {
            return Ok(user_id);
        }
    }
}

/// Records a NAS login of `user_id`, registering it if this server has not:
/// a console keeps the userid it got, from this server or another.
pub async fn log_in(pool: &PgPool, user_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO accounts (user_id, last_login_at) VALUES ($1, now())
         ON CONFLICT (user_id) DO UPDATE SET last_login_at = now()",
        user_id,
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub profile_id: i32,
    pub user_id: i64,
    pub gsbrcd: String,
    pub unique_nick: String,
    pub firstname: String,
    pub lastname: String,
}

/// Logs `user_id` in to its profile for the game `gsbrcd`, making the
/// profile on the first login.
pub async fn profile_login(
    pool: &PgPool,
    user_id: i64,
    gsbrcd: &str,
    ingamesn: Option<&str>,
    ip: &str,
) -> Result<Profile, sqlx::Error> {
    // Update first, so a login does not use up a profile id.
    loop {
        if let Some(profile) = sqlx::query_as!(
            Profile,
            "UPDATE profiles
             SET last_login_at = now(), ingamesn = coalesce($3, ingamesn), last_ip = $4
             WHERE user_id = $1 AND gsbrcd = $2
             RETURNING profile_id, user_id, gsbrcd, unique_nick, firstname, lastname",
            user_id,
            gsbrcd,
            ingamesn,
            ip,
        )
        .fetch_optional(pool)
        .await?
        {
            return Ok(profile);
        }
        // Another login of this profile may insert it first: then update it.
        if let Some(profile) = sqlx::query_as!(
            Profile,
            "INSERT INTO profiles (user_id, gsbrcd, unique_nick, ingamesn, last_ip)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (user_id, gsbrcd) DO NOTHING
             RETURNING profile_id, user_id, gsbrcd, unique_nick, firstname, lastname",
            user_id,
            gsbrcd,
            unique_nick(user_id, gsbrcd),
            ingamesn,
            ip,
        )
        .fetch_optional(pool)
        .await?
        {
            return Ok(profile);
        }
    }
}

pub async fn profile(pool: &PgPool, profile_id: i32) -> Result<Option<Profile>, sqlx::Error> {
    sqlx::query_as!(
        Profile,
        "SELECT profile_id, user_id, gsbrcd, unique_nick, firstname, lastname
         FROM profiles WHERE profile_id = $1",
        profile_id,
    )
    .fetch_optional(pool)
    .await
}

/// Sets the names a client gives its profile; `None` keeps a name.
pub async fn set_names(
    pool: &PgPool,
    profile_id: i32,
    firstname: Option<&str>,
    lastname: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE profiles
         SET firstname = coalesce($2, firstname), lastname = coalesce($3, lastname)
         WHERE profile_id = $1",
        profile_id,
        firstname,
        lastname,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// The friend code of `profile_id` for a player of `game` (`R4QE`, `R4QP`):
/// the profile id, and above it 7 bits of a CRC-8 (polynomial 0x07) of the
/// profile id and the game code, both little-endian
/// (`DWC_Acc_CreateFriendKey` in `dwc_account.c`). DWC checks it with the
/// player's own game code, so the same profile has a code per release.
pub fn friend_code(profile_id: i32, game: &str) -> String {
    let profile_id = profile_id as u32;
    let mut data = profile_id.to_le_bytes().to_vec();
    data.extend(game.bytes().rev());
    let crc = data.iter().fold(0u8, |crc, &byte| {
        (0..8).fold(crc ^ byte, |crc, _| {
            if crc & 0x80 == 0 {
                crc << 1
            } else {
                crc << 1 ^ 0x07
            }
        })
    });
    let digits = format!(
        "{:012}",
        u64::from(crc & 0x7F) << 32 | u64::from(profile_id)
    );
    format!("{}-{}-{}", &digits[..4], &digits[4..8], &digits[8..])
}

/// GameSpy's unique nick for a console's profile: its userid in base32
/// (`0-9a-v`, most significant digit first), then `gsbrcd`.
fn unique_nick(user_id: i64, gsbrcd: &str) -> String {
    const DIGITS: &[u8; 32] = b"0123456789abcdefghijklmnopqrstuv";
    let mut digits = Vec::new();
    let mut rest = user_id as u64;
    loop {
        digits.push(DIGITS[(rest % 32) as usize]);
        rest /= 32;
        if rest == 0 {
            break;
        }
    }
    digits.reverse();
    String::from_utf8(digits).unwrap() + gsbrcd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_nicks_are_base32_userids_and_the_game() {
        assert_eq!(unique_nick(0, "R4QEabc1234"), "0R4QEabc1234");
        assert_eq!(unique_nick(32 * 32 + 31, "R4QE"), "10vR4QE");
        assert_eq!(unique_nick(USER_ID_LIMIT - 1, "R4QE"), "7vvvvvvvvR4QE");
    }

    #[test]
    fn friend_codes_are_checked_with_the_game_code() {
        // The port's first profile, as its Friend Roster shows it.
        assert_eq!(friend_code(1_000_000_000, "R4QE"), "0868-9934-5920");
        // The CPU buddy (docs/login.md).
        assert_eq!(friend_code(CPU_BUDDY, "R4QE"), "1770-9365-9135");
        assert_eq!(friend_code(CPU_BUDDY, "R4QP"), "0009-9999-9999");
    }

    #[sqlx::test]
    async fn the_cpu_buddy_is_there(pool: PgPool) {
        let buddy = profile(&pool, CPU_BUDDY).await.unwrap().unwrap();
        assert_eq!(buddy.user_id, 1);
        // New profiles still count from 1,000,000,000.
        log_in(&pool, 42).await.unwrap();
        let first = profile_login(&pool, 42, "R4QEaaaaaaa", None, "10.0.0.1")
            .await
            .unwrap();
        assert_eq!(first.profile_id, 1_000_000_000);
    }

    #[sqlx::test]
    async fn registered_accounts_get_one_profile_per_game(pool: PgPool) {
        let user_id = register(&pool).await.unwrap();
        assert!((0..USER_ID_LIMIT).contains(&user_id));

        let first = profile_login(&pool, user_id, "R4QEaaaaaaa", Some("Mario"), "10.0.0.1")
            .await
            .unwrap();
        let again = profile_login(&pool, user_id, "R4QEaaaaaaa", None, "10.0.0.2")
            .await
            .unwrap();
        let other = profile_login(&pool, user_id, "R4QEbbbbbbb", None, "10.0.0.1")
            .await
            .unwrap();
        assert_eq!(first, again);
        assert_eq!(first.profile_id, 1_000_000_000);
        assert_eq!(other.profile_id, 1_000_000_001);

        set_names(&pool, first.profile_id, None, Some("login-id"))
            .await
            .unwrap();
        let named = profile(&pool, first.profile_id).await.unwrap().unwrap();
        assert_eq!(
            (named.firstname.as_str(), named.lastname.as_str()),
            ("", "login-id")
        );
    }

    #[sqlx::test]
    async fn a_login_registers_an_unknown_userid(pool: PgPool) {
        log_in(&pool, 42).await.unwrap();
        log_in(&pool, 42).await.unwrap();
        let profile = profile_login(&pool, 42, "R4QEaaaaaaa", None, "10.0.0.1").await;
        assert!(profile.is_ok());
    }
}
