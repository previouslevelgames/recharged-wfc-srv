//! Tournaments the server runs (docs/tournaments.md): brackets of single or
//! double elimination whose sets are the server's own matches, for players
//! who signed in with Discord.

// The store (next) is what uses it.
#[allow(dead_code)]
pub mod bracket;
