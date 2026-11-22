//! The message the launcher shows on its Play page, such as "Server restart
//! tonight at 22:00" (docs/profiles.md, "Launcher message"). Owners post it
//! on the admin dashboard's Maintenance page. Every post is a row of its own
//! with a new id, so the launcher shows an edited message again to players
//! who closed the last one. Only the newest post shows, until it ends or is
//! removed; the older ones stay as history.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use time::OffsetDateTime;

/// The longest message, in characters.
pub const MAX_LENGTH: usize = 2000;

/// How the launcher shows it.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tone {
    Info,
    Warning,
}

impl Tone {
    fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
        }
    }

    /// The table's column, which allows only these two.
    fn parse(tone: &str) -> Self {
        match tone {
            "warning" => Self::Warning,
            _ => Self::Info,
        }
    }
}

/// A message as the launcher gets it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Message {
    pub id: i64,
    pub markdown: String,
    pub tone: Tone,
    #[serde(with = "time::serde::rfc3339")]
    pub posted_at: OffsetDateTime,
    /// None: until it is removed.
    #[serde(with = "time::serde::rfc3339::option")]
    pub ends_at: Option<OffsetDateTime>,
}

/// A message with who posted it, for the Maintenance page. The launcher
/// never gets the owner's address.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Posted {
    #[serde(flatten)]
    pub message: Message,
    pub posted_by: String,
}

/// Why a post was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    Empty,
    TooLong,
    /// Its end is not in the future.
    Ended,
}

impl Refusal {
    pub fn message(&self) -> String {
        match self {
            Self::Empty => "write a message".to_owned(),
            Self::TooLong => format!("a message has at most {MAX_LENGTH} characters"),
            Self::Ended => "\"Show until\" must be in the future".to_owned(),
        }
    }
}

struct Row {
    id: i64,
    markdown: String,
    tone: String,
    posted_at: OffsetDateTime,
    ends_at: Option<OffsetDateTime>,
    posted_by: String,
}

impl From<Row> for Posted {
    fn from(row: Row) -> Self {
        Self {
            message: Message {
                id: row.id,
                markdown: row.markdown,
                tone: Tone::parse(&row.tone),
                posted_at: row.posted_at,
                ends_at: row.ends_at,
            },
            posted_by: row.posted_by,
        }
    }
}

/// The message the launcher shows now: the newest post, unless it ended or
/// was removed.
pub async fn current(pool: &PgPool) -> Result<Option<Posted>, sqlx::Error> {
    let row = sqlx::query_as!(
        Row,
        "SELECT id, markdown, tone, posted_at, ends_at, posted_by FROM launcher_messages
         WHERE id = (SELECT max(id) FROM launcher_messages)
           AND removed_at IS NULL AND (ends_at IS NULL OR ends_at > now())"
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(Posted::from))
}

/// Posts a message for the owner `by`: it replaces the one shown now.
pub async fn post(
    pool: &PgPool,
    markdown: &str,
    tone: Tone,
    ends_at: Option<OffsetDateTime>,
    by: &str,
) -> Result<Result<Posted, Refusal>, sqlx::Error> {
    if markdown.trim().is_empty() {
        return Ok(Err(Refusal::Empty));
    }
    if markdown.chars().count() > MAX_LENGTH {
        return Ok(Err(Refusal::TooLong));
    }
    if ends_at.is_some_and(|ends_at| ends_at <= OffsetDateTime::now_utc()) {
        return Ok(Err(Refusal::Ended));
    }
    let row = sqlx::query_as!(
        Row,
        "INSERT INTO launcher_messages (markdown, tone, ends_at, posted_by)
         VALUES ($1, $2, $3, $4)
         RETURNING id, markdown, tone, posted_at, ends_at, posted_by",
        markdown,
        tone.as_str(),
        ends_at,
        by
    )
    .fetch_one(pool)
    .await?;
    Ok(Ok(row.into()))
}

/// Removes the message shown now for the owner `by`: the one removed, or
/// None when none showed.
pub async fn remove(pool: &PgPool, by: &str) -> Result<Option<Posted>, sqlx::Error> {
    let row = sqlx::query_as!(
        Row,
        "UPDATE launcher_messages SET removed_at = now(), removed_by = $1
         WHERE id = (SELECT max(id) FROM launcher_messages)
           AND removed_at IS NULL AND (ends_at IS NULL OR ends_at > now())
         RETURNING id, markdown, tone, posted_at, ends_at, posted_by",
        by
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(Posted::from))
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: &str = "owner@example.com";

    #[sqlx::test]
    async fn a_post_shows_until_the_next_one_or_its_removal(pool: PgPool) {
        assert_eq!(current(&pool).await.unwrap(), None);

        let first = post(&pool, "Restart at 22:00", Tone::Info, None, OWNER)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.posted_by, OWNER);
        assert_eq!(current(&pool).await.unwrap().as_ref(), Some(&first));

        let ends_at = OffsetDateTime::now_utc() + time::Duration::hours(1);
        let second = post(
            &pool,
            "Restart at 23:00",
            Tone::Warning,
            Some(ends_at),
            OWNER,
        )
        .await
        .unwrap()
        .unwrap();
        assert!(second.message.id > first.message.id);
        let shown = current(&pool).await.unwrap().unwrap();
        assert_eq!(shown.message.markdown, "Restart at 23:00");
        assert_eq!(shown.message.tone, Tone::Warning);

        // Once the newest one ends, the older one stays history.
        sqlx::query!(
            "UPDATE launcher_messages SET ends_at = now() - interval '1 minute' WHERE id = $1",
            second.message.id
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(current(&pool).await.unwrap(), None);
        assert_eq!(remove(&pool, OWNER).await.unwrap(), None, "nothing shows");

        let third = post(&pool, "Back up", Tone::Info, None, OWNER)
            .await
            .unwrap()
            .unwrap();
        let removed = remove(&pool, "other@example.com").await.unwrap().unwrap();
        assert_eq!(removed.message.id, third.message.id);
        assert_eq!(current(&pool).await.unwrap(), None);
        let removed_by = sqlx::query_scalar!(
            "SELECT removed_by FROM launcher_messages WHERE id = $1",
            third.message.id
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(removed_by.as_deref(), Some("other@example.com"));
    }

    #[sqlx::test]
    async fn a_post_is_refused_empty_too_long_or_ended(pool: PgPool) {
        let refused = |markdown: String, ends_at| {
            let pool = pool.clone();
            async move {
                post(&pool, &markdown, Tone::Info, ends_at, OWNER)
                    .await
                    .unwrap()
                    .err()
            }
        };
        assert_eq!(refused(" \n".into(), None).await, Some(Refusal::Empty));
        assert_eq!(
            refused("é".repeat(MAX_LENGTH + 1), None).await,
            Some(Refusal::TooLong)
        );
        let past = OffsetDateTime::now_utc() - time::Duration::minutes(1);
        assert_eq!(
            refused("Soon".into(), Some(past)).await,
            Some(Refusal::Ended)
        );
        assert_eq!(current(&pool).await.unwrap(), None);

        // Characters, not bytes.
        assert_eq!(refused("é".repeat(MAX_LENGTH), None).await, None);
    }

    #[test]
    fn the_launcher_gets_rfc_3339_times_and_no_address() {
        let posted = Posted {
            message: Message {
                id: 7,
                markdown: "**Hi**".into(),
                tone: Tone::Warning,
                // 2026-11-21 18:00 UTC.
                posted_at: OffsetDateTime::from_unix_timestamp(1_795_284_000).unwrap(),
                ends_at: None,
            },
            posted_by: OWNER.into(),
        };
        assert_eq!(
            serde_json::to_value(&posted.message).unwrap(),
            serde_json::json!({
                "id": 7,
                "markdown": "**Hi**",
                "tone": "warning",
                "posted_at": "2026-11-21T18:00:00Z",
                "ends_at": null,
            })
        );
        assert_eq!(serde_json::to_value(&posted).unwrap()["posted_by"], OWNER);
    }
}
