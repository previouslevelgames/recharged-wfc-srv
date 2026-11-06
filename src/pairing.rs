//! Games kept for a match (docs/profiles.md, "Meeting in RANKED"): once a
//! match between two players is accepted, each player's game says it is
//! ready, logged in to WFC as one of the player's profiles. A ranked search
//! by a ready game then lists only the other one, as soon as it is ready
//! too, and nobody else's search lists either of them. Games of different
//! releases can't play each other, so they are never paired.
//!
//! Readiness is kept in the table `pairings`, so that a game's ready call may
//! reach any node of a pool and any node's server browser lists by it
//! (docs/nodes.md). It lasts [`READY_TTL`] by the database's clock: the port
//! says it again while the game is online, so a game that went away stops
//! being kept. One ready call of a match is handled at a time.
//!
//! A player has one game in a match: when another of their games (another
//! WFC profile) says it is ready while the first still is, the newer one
//! takes the side. The one it replaced stays kept, seeing nobody and seen by
//! nobody, and is told so each time it says it is ready, until it stops.

use std::{collections::HashSet, time::Duration};

use serde::Serialize;
use sqlx::{PgConnection, PgPool};

/// How long a game stays ready without saying it again.
pub const READY_TTL: Duration = Duration::from_secs(30);

/// The ready games of each match.
pub struct Pairings {
    pool: PgPool,
}

/// How a match's games stand.
#[derive(Debug, PartialEq)]
pub enum Pairing {
    /// The opponent's game is not ready.
    Waiting,
    /// Both are: they find each other in RANKED.
    Paired,
    /// Both are, but of different releases.
    OtherRelease,
    /// Another game of this player took its side: this one plays no part.
    Replaced,
}

/// A match's games as they stand, for diagnosing (`/admin`).
#[derive(Debug, Serialize)]
pub struct View {
    /// `paired`, `waiting` or `other_release`.
    pub pairing: &'static str,
    /// The challenger's side, then the challenged's.
    pub sides: [SideView; 2],
}

#[derive(Debug, Default, Serialize)]
pub struct SideView {
    pub ready: Option<GameView>,
    /// Games another game of the same player replaced.
    pub replaced: Vec<GameView>,
}

#[derive(Debug, Serialize)]
pub struct GameView {
    pub profile_id: i32,
    pub game: String,
    /// Seconds since it last said it is ready.
    pub said_ready_secs_ago: f64,
    /// Whether that was within [`READY_TTL`].
    pub live: bool,
}

/// Whom a ranked search may find.
#[derive(Debug, PartialEq)]
pub enum Visible {
    /// The searcher is in no match: every game but those kept for one.
    Everyone,
    /// The searcher's opponent, who is ready too.
    Only(i32),
    /// Nobody yet: the opponent is not ready, or can't be played.
    Nobody,
}

/// A side's game in a match.
struct Game {
    profile_id: i32,
    game: String,
    live: bool,
}

fn ttl() -> f64 {
    READY_TTL.as_secs_f64()
}

impl Pairings {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// `profile_id`, a game of `game`, side `side` (0 the challenger, 1 the
    /// challenged) of `match_id`, is ready.
    pub async fn ready(
        &self,
        match_id: i64,
        side: usize,
        profile_id: i32,
        game: &str,
    ) -> Result<Pairing, sqlx::Error> {
        let side = side as i16;
        let mut tx = self.pool.begin().await?;
        // One ready call of the match at a time, on whichever node.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('pairings', $1))")
            .bind(match_id)
            .execute(&mut *tx)
            .await?;
        // Matches none of whose games is ready any more go.
        sqlx::query!(
            "DELETE FROM pairings p WHERE NOT EXISTS (
                 SELECT 1 FROM pairings q
                 WHERE q.match_id = p.match_id AND q.ready_at > now() - make_interval(secs => $1))",
            ttl(),
        )
        .execute(&mut *tx)
        .await?;
        // A profile is kept for one match at a time.
        sqlx::query!(
            "DELETE FROM pairings WHERE profile_id = $1 AND match_id <> $2",
            profile_id,
            match_id,
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!(
            "DELETE FROM pairings
             WHERE match_id = $1 AND side = $2 AND replaced
               AND ready_at <= now() - make_interval(secs => $3)",
            match_id,
            side,
            ttl(),
        )
        .execute(&mut *tx)
        .await?;
        let replaced = sqlx::query!(
            "UPDATE pairings SET ready_at = now(), game = $4
             WHERE match_id = $1 AND side = $2 AND profile_id = $3 AND replaced",
            match_id,
            side,
            profile_id,
            game,
        )
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if replaced {
            tx.commit().await?;
            return Ok(Pairing::Replaced);
        }
        // Another game of the player's that is still ready gives up the
        // side; one that stopped goes.
        sqlx::query!(
            "UPDATE pairings SET replaced = true
             WHERE match_id = $1 AND side = $2 AND profile_id <> $3 AND NOT replaced
               AND ready_at > now() - make_interval(secs => $4)",
            match_id,
            side,
            profile_id,
            ttl(),
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!(
            "DELETE FROM pairings
             WHERE match_id = $1 AND side = $2 AND profile_id <> $3 AND NOT replaced",
            match_id,
            side,
            profile_id,
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!(
            "INSERT INTO pairings (match_id, side, profile_id, game) VALUES ($1, $2, $3, $4)
             ON CONFLICT (match_id, side, profile_id) DO UPDATE
             SET game = EXCLUDED.game, ready_at = now(), replaced = false",
            match_id,
            side,
            profile_id,
            game,
        )
        .execute(&mut *tx)
        .await?;
        let pairing = pairing(&sides(&mut tx, match_id).await?);
        tx.commit().await?;
        Ok(pairing)
    }

    /// The player of side `side` took back Ready to play! for `match_id`:
    /// their game is not ready until it says so again.
    pub async fn unready(&self, match_id: i64, side: usize) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "DELETE FROM pairings WHERE match_id = $1 AND side = $2 AND NOT replaced",
            match_id,
            side as i16,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The match `match_id` ended: its games are no longer kept.
    pub async fn end(&self, match_id: i64) -> Result<(), sqlx::Error> {
        sqlx::query!("DELETE FROM pairings WHERE match_id = $1", match_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// How the games of `match_id` stand.
    pub async fn pairing(&self, match_id: i64) -> Result<Pairing, sqlx::Error> {
        let mut conn = self.pool.acquire().await?;
        Ok(pairing(&sides(&mut conn, match_id).await?))
    }

    /// Whom a ranked search by `profile_id` may find.
    pub async fn visible_to(&self, profile_id: i32) -> Result<Visible, sqlx::Error> {
        let mut conn = self.pool.acquire().await?;
        let kept = sqlx::query!(
            "SELECT match_id, side, replaced FROM pairings
             WHERE profile_id = $1 AND ready_at > now() - make_interval(secs => $2)
             ORDER BY ready_at DESC LIMIT 1",
            profile_id,
            ttl(),
        )
        .fetch_optional(&mut *conn)
        .await?;
        let Some(kept) = kept else {
            return Ok(Visible::Everyone);
        };
        if kept.replaced {
            return Ok(Visible::Nobody);
        }
        let sides = sides(&mut conn, kept.match_id).await?;
        let other = &sides[1 - kept.side as usize];
        Ok(match (other, pairing(&sides)) {
            (Some(other), Pairing::Paired) => Visible::Only(other.profile_id),
            _ => Visible::Nobody,
        })
    }

    /// Whether `profile_id`'s game is kept for a match.
    pub async fn kept(&self, profile_id: i32) -> Result<bool, sqlx::Error> {
        Ok(self.kept_of(&[profile_id]).await?.contains(&profile_id))
    }

    /// Those of `profiles` whose games are kept for a match.
    pub async fn kept_of(&self, profiles: &[i32]) -> Result<HashSet<i32>, sqlx::Error> {
        let kept = sqlx::query_scalar!(
            "SELECT DISTINCT profile_id FROM pairings
             WHERE profile_id = ANY($1) AND ready_at > now() - make_interval(secs => $2)",
            profiles,
            ttl(),
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(kept.into_iter().collect())
    }

    /// How the games of `match_id` stand, for diagnosing.
    pub async fn view(&self, match_id: i64) -> Result<View, sqlx::Error> {
        let rows = sqlx::query!(
            r#"SELECT side, profile_id, game, replaced,
                      EXTRACT(EPOCH FROM now() - ready_at)::FLOAT8 AS "secs_ago!",
                      ready_at > now() - make_interval(secs => $2) AS "live!"
               FROM pairings WHERE match_id = $1 ORDER BY ready_at"#,
            match_id,
            ttl(),
        )
        .fetch_all(&self.pool)
        .await?;
        let mut sides: [SideView; 2] = Default::default();
        let mut ready: [Option<Game>; 2] = Default::default();
        for row in rows {
            let side = row.side as usize;
            let view = GameView {
                profile_id: row.profile_id,
                game: row.game.clone(),
                said_ready_secs_ago: row.secs_ago,
                live: row.live,
            };
            if row.replaced {
                sides[side].replaced.push(view);
            } else {
                ready[side] = Some(Game {
                    profile_id: row.profile_id,
                    game: row.game,
                    live: row.live,
                });
                sides[side].ready = Some(view);
            }
        }
        Ok(View {
            pairing: match pairing(&ready) {
                Pairing::Paired => "paired",
                Pairing::OtherRelease => "other_release",
                Pairing::Waiting | Pairing::Replaced => "waiting",
            },
            sides,
        })
    }
}

/// The game of each side of `match_id`, if it has one.
async fn sides(conn: &mut PgConnection, match_id: i64) -> Result<[Option<Game>; 2], sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT side, profile_id, game,
                  ready_at > now() - make_interval(secs => $2) AS "live!"
           FROM pairings WHERE match_id = $1 AND NOT replaced"#,
        match_id,
        ttl(),
    )
    .fetch_all(conn)
    .await?;
    let mut sides: [Option<Game>; 2] = Default::default();
    for row in rows {
        sides[row.side as usize] = Some(Game {
            profile_id: row.profile_id,
            game: row.game,
            live: row.live,
        });
    }
    Ok(sides)
}

fn pairing(sides: &[Option<Game>; 2]) -> Pairing {
    match sides {
        [Some(home), Some(away)] if home.live && away.live => {
            if home.game == away.game {
                Pairing::Paired
            } else {
                Pairing::OtherRelease
            }
        }
        _ => Pairing::Waiting,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `profile_id` last said it is ready `secs` ago.
    async fn said_ready_ago(pool: &PgPool, profile_id: i32, secs: f64) {
        sqlx::query(
            "UPDATE pairings SET ready_at = now() - make_interval(secs => $2)
             WHERE profile_id = $1",
        )
        .bind(profile_id)
        .bind(secs)
        .execute(pool)
        .await
        .unwrap();
    }

    #[sqlx::test]
    async fn two_ready_games_see_only_each_other(pool: PgPool) {
        let pairings = Pairings::new(pool);
        assert_eq!(pairings.visible_to(1).await.unwrap(), Visible::Everyone);

        assert_eq!(
            pairings.ready(7, 0, 1, "R4QE").await.unwrap(),
            Pairing::Waiting
        );
        assert_eq!(pairings.visible_to(1).await.unwrap(), Visible::Nobody);
        assert!(pairings.kept(1).await.unwrap());
        assert_eq!(pairings.pairing(7).await.unwrap(), Pairing::Waiting);

        assert_eq!(
            pairings.ready(7, 1, 2, "R4QE").await.unwrap(),
            Pairing::Paired
        );
        assert_eq!(pairings.pairing(7).await.unwrap(), Pairing::Paired);
        assert_eq!(pairings.visible_to(1).await.unwrap(), Visible::Only(2));
        assert_eq!(pairings.visible_to(2).await.unwrap(), Visible::Only(1));
        assert_eq!(pairings.visible_to(3).await.unwrap(), Visible::Everyone);
        assert_eq!(
            pairings.kept_of(&[1, 2, 3]).await.unwrap(),
            HashSet::from([1, 2])
        );

        pairings.end(7).await.unwrap();
        assert_eq!(pairings.visible_to(1).await.unwrap(), Visible::Everyone);
        assert!(!pairings.kept(2).await.unwrap());
    }

    #[sqlx::test]
    async fn a_game_taken_back_is_not_ready(pool: PgPool) {
        let pairings = Pairings::new(pool);
        pairings.ready(7, 0, 1, "R4QE").await.unwrap();
        pairings.unready(7, 0).await.unwrap();
        assert_eq!(pairings.visible_to(1).await.unwrap(), Visible::Everyone);
        assert_eq!(
            pairings.ready(7, 1, 2, "R4QE").await.unwrap(),
            Pairing::Waiting
        );
        assert_eq!(
            pairings.ready(7, 0, 1, "R4QE").await.unwrap(),
            Pairing::Paired
        );
        pairings.unready(7, 1).await.unwrap();
        assert_eq!(pairings.pairing(7).await.unwrap(), Pairing::Waiting);
        assert_eq!(pairings.visible_to(1).await.unwrap(), Visible::Nobody);
    }

    #[sqlx::test]
    async fn releases_are_never_paired(pool: PgPool) {
        let pairings = Pairings::new(pool);
        pairings.ready(7, 0, 1, "R4QE").await.unwrap();
        assert_eq!(
            pairings.ready(7, 1, 2, "R4QP").await.unwrap(),
            Pairing::OtherRelease
        );
        assert_eq!(pairings.visible_to(1).await.unwrap(), Visible::Nobody);
        assert_eq!(pairings.visible_to(2).await.unwrap(), Visible::Nobody);
    }

    #[sqlx::test]
    async fn a_players_newer_game_takes_the_side(pool: PgPool) {
        let pairings = Pairings::new(pool);
        // Mario's first game (profile 1) is ready, then his second (3).
        assert_eq!(
            pairings.ready(7, 0, 1, "R4QE").await.unwrap(),
            Pairing::Waiting
        );
        assert_eq!(
            pairings.ready(7, 1, 2, "R4QE").await.unwrap(),
            Pairing::Paired
        );
        assert_eq!(
            pairings.ready(7, 0, 3, "R4QE").await.unwrap(),
            Pairing::Paired
        );
        assert_eq!(pairings.visible_to(2).await.unwrap(), Visible::Only(3));
        assert_eq!(pairings.visible_to(3).await.unwrap(), Visible::Only(2));
        // The first sees nobody, nobody else's search finds it, and it can't
        // take the side back.
        assert_eq!(pairings.visible_to(1).await.unwrap(), Visible::Nobody);
        assert!(pairings.kept(1).await.unwrap());
        assert_eq!(
            pairings.ready(7, 0, 1, "R4QE").await.unwrap(),
            Pairing::Replaced
        );
        assert_eq!(pairings.visible_to(2).await.unwrap(), Visible::Only(3));
        assert_eq!(
            pairings.ready(7, 0, 3, "R4QE").await.unwrap(),
            Pairing::Paired
        );
        let view = pairings.view(7).await.unwrap();
        assert_eq!(view.pairing, "paired");
        assert_eq!(view.sides[0].ready.as_ref().unwrap().profile_id, 3);
        assert_eq!(view.sides[0].replaced[0].profile_id, 1);
        assert_eq!(view.sides[1].ready.as_ref().unwrap().profile_id, 2);
        pairings.end(7).await.unwrap();
        assert_eq!(pairings.visible_to(1).await.unwrap(), Visible::Everyone);
        assert_eq!(pairings.view(7).await.unwrap().pairing, "waiting");
    }

    #[sqlx::test]
    async fn a_profile_is_kept_for_its_latest_match(pool: PgPool) {
        let pairings = Pairings::new(pool);
        pairings.ready(7, 0, 1, "R4QE").await.unwrap();
        pairings.ready(7, 1, 2, "R4QE").await.unwrap();
        pairings.ready(8, 0, 1, "R4QE").await.unwrap();
        assert_eq!(pairings.visible_to(1).await.unwrap(), Visible::Nobody);
        assert_eq!(pairings.visible_to(2).await.unwrap(), Visible::Nobody);
        assert_eq!(pairings.pairing(7).await.unwrap(), Pairing::Waiting);
    }

    #[sqlx::test]
    async fn a_game_that_stops_saying_it_is_ready_is_let_go(pool: PgPool) {
        let pairings = Pairings::new(pool.clone());
        pairings.ready(7, 0, 1, "R4QE").await.unwrap();
        pairings.ready(7, 1, 2, "R4QE").await.unwrap();
        said_ready_ago(&pool, 2, READY_TTL.as_secs_f64() + 1.0).await;
        assert_eq!(pairings.pairing(7).await.unwrap(), Pairing::Waiting);
        assert_eq!(pairings.visible_to(1).await.unwrap(), Visible::Nobody);
        assert_eq!(pairings.visible_to(2).await.unwrap(), Visible::Everyone);
        let view = pairings.view(7).await.unwrap();
        let away = view.sides[1].ready.as_ref().unwrap();
        assert!(!away.live && away.said_ready_secs_ago > READY_TTL.as_secs_f64());

        // A replaced game that stopped is gone, and another game of the
        // player's takes a side whose game stopped without replacing it.
        pairings.ready(7, 0, 3, "R4QE").await.unwrap();
        said_ready_ago(&pool, 1, 60.0).await;
        said_ready_ago(&pool, 3, 60.0).await;
        assert_eq!(
            pairings.ready(7, 0, 1, "R4QE").await.unwrap(),
            Pairing::Waiting
        );
        let view = pairings.view(7).await.unwrap();
        assert_eq!(view.sides[0].ready.as_ref().unwrap().profile_id, 1);
        assert!(view.sides[0].replaced.is_empty());

        // A match whose games all stopped goes at the next ready call.
        said_ready_ago(&pool, 1, 60.0).await;
        pairings.ready(8, 0, 4, "R4QE").await.unwrap();
        assert!(pairings.view(7).await.unwrap().sides[0].ready.is_none());
    }

    #[sqlx::test]
    async fn ready_calls_of_one_match_on_two_nodes_pair(pool: PgPool) {
        // Two nodes' calls at once: one of them finds the other ready.
        let (home, away) = (Pairings::new(pool.clone()), Pairings::new(pool));
        let (home, away) = tokio::join!(home.ready(7, 0, 1, "R4QE"), away.ready(7, 1, 2, "R4QE"));
        let mut pairings = [home.unwrap(), away.unwrap()];
        pairings.sort_by_key(|pairing| *pairing == Pairing::Paired);
        assert_eq!(pairings, [Pairing::Waiting, Pairing::Paired]);
    }
}
