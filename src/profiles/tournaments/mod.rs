//! Tournaments the server runs (docs/tournaments.md): brackets of single or
//! double elimination whose sets are the server's own matches, for players
//! who signed in with Discord.

pub mod bracket;
pub mod routes;
// The admin page (next) makes, starts and decides tournaments.
#[allow(dead_code)]
pub mod store;
