//! Messages between friends (docs/profiles.md, "Chat"): two players who keep
//! each other as friends write each other short lines, kept for 30 days.

use serde::Serialize;
use sqlx::PgPool;

/// Characters a message has at most.
pub const MAX_LENGTH: usize = 300;
/// Days a message is kept.
pub const KEPT_DAYS: i32 = 30;
/// Messages a player sends per minute at most.
pub const PER_MINUTE: i64 = 20;
/// Messages one fetch returns at most: the latest.
pub const PAGE: i64 = 50;

#[derive(Debug, PartialEq, Serialize)]
pub struct Message {
    pub id: i64,
    /// Sent by the player who asks.
    pub mine: bool,
    pub body: String,
    /// Unix seconds.
    pub sent_at: i64,
}

/// `body` as kept: control characters (line breaks too) become spaces, and
/// the ends are trimmed. None when that leaves nothing or too much.
pub fn clean(body: &str) -> Option<String> {
    let spaced: String = body
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let trimmed = spaced.trim();
    let length = trimmed.chars().count();
    (length > 0 && length <= MAX_LENGTH).then(|| trimmed.to_owned())
}

/// Whether `a` and `b` keep each other as friends.
pub async fn may_chat(pool: &PgPool, a: i64, b: i64) -> Result<bool, sqlx::Error> {
    let both = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM friends
           WHERE (player_id = $1 AND friend_id = $2) OR (player_id = $2 AND friend_id = $1)"#,
        a,
        b,
    )
    .fetch_one(pool)
    .await?;
    Ok(both == 2)
}

/// The messages `player_id` sent in the last minute.
pub async fn sent_last_minute(pool: &PgPool, player_id: i64) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM chat_messages
           WHERE sender = $1 AND sent_at > now() - interval '1 minute'"#,
        player_id,
    )
    .fetch_one(pool)
    .await
}

/// `sender` writes `body` (already [`clean`]) to `recipient`. Messages older
/// than [`KEPT_DAYS`] go meanwhile.
pub async fn send(
    pool: &PgPool,
    sender: i64,
    recipient: i64,
    body: &str,
) -> Result<Message, sqlx::Error> {
    sqlx::query!(
        "DELETE FROM chat_messages WHERE sent_at < now() - make_interval(days => $1)",
        KEPT_DAYS,
    )
    .execute(pool)
    .await?;
    let row = sqlx::query!(
        r#"INSERT INTO chat_messages (sender, recipient, body) VALUES ($1, $2, $3)
           RETURNING message_id, extract(epoch FROM sent_at)::bigint AS "sent_at!""#,
        sender,
        recipient,
        body,
    )
    .fetch_one(pool)
    .await?;
    Ok(Message {
        id: row.message_id,
        mine: true,
        body: body.to_owned(),
        sent_at: row.sent_at,
    })
}

/// The messages between `player_id` and `other`, oldest first: those after
/// message `after`, or the latest [`PAGE`]. The ones to `player_id` are read
/// now.
pub async fn messages(
    pool: &PgPool,
    player_id: i64,
    other: i64,
    after: Option<i64>,
) -> Result<Vec<Message>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT message_id, sender, body, extract(epoch FROM sent_at)::bigint AS "sent_at!"
           FROM chat_messages
           WHERE least(sender, recipient) = least($1::bigint, $2::bigint)
             AND greatest(sender, recipient) = greatest($1::bigint, $2::bigint)
             AND message_id > $3
             AND sent_at > now() - make_interval(days => $5)
           ORDER BY message_id DESC
           LIMIT $4"#,
        player_id,
        other,
        after.unwrap_or(0),
        PAGE,
        KEPT_DAYS,
    )
    .fetch_all(pool)
    .await?;
    sqlx::query!(
        "UPDATE chat_messages SET read_at = now()
         WHERE recipient = $1 AND sender = $2 AND read_at IS NULL",
        player_id,
        other,
    )
    .execute(pool)
    .await?;
    Ok(rows
        .into_iter()
        .rev()
        .map(|row| Message {
            id: row.message_id,
            mine: row.sender == player_id,
            body: row.body,
            sent_at: row.sent_at,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::clean;

    #[test]
    fn messages_are_one_trimmed_line_of_at_most_300_characters() {
        assert_eq!(clean("  hi\nthere\t!  ").as_deref(), Some("hi there !"));
        assert_eq!(clean(" \n "), None);
        let long = "é".repeat(300);
        assert_eq!(clean(&long).as_deref(), Some(long.as_str()));
        assert_eq!(clean(&"é".repeat(301)), None);
    }
}
