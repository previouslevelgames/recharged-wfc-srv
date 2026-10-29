//! Each player's friends (docs/profiles.md, "Friends"): one list per player,
//! without requests, kept on the server so it follows the player to every
//! computer.

use sqlx::PgPool;

use super::players::{self, Listed};

/// Friends a player keeps at most; adding one more is refused.
pub const MAX_FRIENDS: i64 = 200;

/// Adds `friend_id` to `player_id`'s friends. Whether it was added: false
/// for one already there, or with the list full.
pub async fn add(pool: &PgPool, player_id: i64, friend_id: i64) -> Result<bool, sqlx::Error> {
    if player_id == friend_id {
        return Ok(false);
    }
    let added = sqlx::query!(
        "INSERT INTO friends (player_id, friend_id)
         SELECT $1, $2
         WHERE (SELECT count(*) FROM friends WHERE player_id = $1) < $3
         ON CONFLICT DO NOTHING",
        player_id,
        friend_id,
        MAX_FRIENDS,
    )
    .execute(pool)
    .await?
    .rows_affected();
    Ok(added == 1)
}

/// Whether `friend_id` was among `player_id`'s friends.
pub async fn remove(pool: &PgPool, player_id: i64, friend_id: i64) -> Result<bool, sqlx::Error> {
    let removed = sqlx::query!(
        "DELETE FROM friends WHERE player_id = $1 AND friend_id = $2",
        player_id,
        friend_id,
    )
    .execute(pool)
    .await?
    .rows_affected();
    Ok(removed == 1)
}

/// `player_id`'s friends: those with Recharged open by name, then the others,
/// the last online first.
pub async fn list(pool: &PgPool, player_id: i64) -> Result<Vec<Listed>, sqlx::Error> {
    let ids = sqlx::query_scalar!(
        r#"SELECT friends.friend_id AS "friend_id!"
           FROM friends JOIN players ON players.player_id = friends.friend_id
           WHERE friends.player_id = $1
           ORDER BY coalesce(players.heartbeat_at > now() - make_interval(secs => $2), false) DESC,
                    CASE WHEN players.heartbeat_at > now() - make_interval(secs => $2)
                         THEN lower(players.display_name) END,
                    players.heartbeat_at DESC NULLS LAST, friends.added_at DESC"#,
        player_id,
        f64::from(super::matches::HEARTBEAT_TIMEOUT_SECS),
    )
    .fetch_all(pool)
    .await?;
    players::listed(pool, player_id, &ids).await
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    /// The migration that made the table keeps the players of the sets so
    /// far: the challenger keeps the challenged, who keeps the challenger
    /// only after accepting.
    #[sqlx::test(migrations = false)]
    async fn earlier_sets_make_friends(pool: PgPool) {
        let migrator = sqlx::migrate!();
        migrator.run_to(20261029100000, &pool).await.unwrap();
        sqlx::query("INSERT INTO players (display_name) VALUES ('A'), ('B'), ('C'), ('D')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO matches (kind, state, home, away, rule_set, rules, rules_hash) VALUES
                 ('test', 'finished', 1, 2, 'x', '{}', 'h'),
                 ('test', 'declined', 1, 3, 'x', '{}', 'h'),
                 ('test', 'challenged', 4, 1, 'x', '{}', 'h'),
                 ('test', 'finished', 2, 1, 'x', '{}', 'h')",
        )
        .execute(&pool)
        .await
        .unwrap();
        migrator.run(&pool).await.unwrap();
        let pairs: Vec<(i64, i64)> =
            sqlx::query_as("SELECT player_id, friend_id FROM friends ORDER BY 1, 2")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(pairs, [(1, 2), (1, 3), (2, 1), (4, 1)]);
    }
}
