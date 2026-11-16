# Tournaments

The server runs tournaments: a bracket of single or double elimination whose
sets are the server's own matches ([profiles.md](profiles.md), "Matches").
Players join and play them from the game (its trophy room); admins make,
start and decide them on `/admin/tournaments`. The code is in
`src/profiles/tournaments/`: `bracket.rs` (the bracket, without the
database), `store.rs` (the tables) and `routes.rs` (the routes).

## Who plays

Only a player who signed in with Discord: `account` is `discord` in their
sign-in (a Mii player who connected Discord too, see "Playing without
Discord" in profiles.md). A test player plays too while test sign-in is on,
for tests on one machine. Anybody else is answered 403
`{"error": "discord_required", "message": "…"}` when joining or pressing
READY, and the tournament's page says `"eligible": false, "reason":
"discord_required"`.

## A tournament

| Field | |
|---|---|
| `format` | `single` or `double` elimination |
| `state` | `registration` (open for entries), `running`, `finished` or `cancelled` |
| `rule_set`, `rules` | The rules its matches are played by, copied from a rule set when it was made |
| `best_of`, `finals_best_of` | Games of a set, and of a final: in single elimination the last round; in double the winners' final, the losers' final, the grand final and its reset |
| `max_entrants` | The most who can join (2 to 256), or `null` |

When it starts, the entrants get their seeds (in the order they joined, at
random, or as the admin gives them) and the bracket its sets. Byes fill the
first round up to a power of 2, against the best seeds (1 against 8, 4
against 5, …). In double elimination the losers of each winners' round drop
into the losers' bracket crossed, so two players don't meet again soon; the
grand final's reset is played only when the player from the losers' bracket
wins the grand final.

## Sets

| `state` | |
|---|---|
| `pending` | Waiting for its players: the sets they come from aren't decided |
| `ready` | Both players are known: to be played |
| `playing` | Its match is open (`match`) |
| `finished` | `winner`, the games each won (`home_wins`, `away_wins`), and `decided_by`: `match`, `organizer`, or `walkover` (nobody came against the winner, or the other left) |
| `disputed` | The two games reported different series, or a draw: the organizer decides |
| `void` | Nobody plays it: both places are empty, or a reset that isn't needed |

A set is played as a match: the first player's READY makes it, accepted at
once, by the tournament's rules with the set's games, and counts as that
player's Ready to play!; the other's READY makes it go. From there it is a
set like any other: the games meet in RANKED, report, may forfeit. A READY
while either player has another match to play is refused (409), and a
player's match still waiting for both is called off by another set of
theirs, as for challenges.

Every sweep of the matches (any request about them) moves on the sets whose
match ended: a result counts, a match without one (called off, expired,
broken off) leaves the set `ready` to be played again, and a disputed one or
a draw leaves it `disputed`.

A player who leaves a running tournament, or whom an admin disqualifies,
loses their open sets without playing and goes no further, not into the
losers' bracket either. The tournament ends with its last set; each entrant
then has a `place` (players out in the same round share one).

## Routes

| Route | |
|---|---|
| `GET /api/v1/tournaments` | `{"tournaments": [...]}`: open for entries or running, the newest first, then the last 10 that ended. With a session, each says whether the player `joined`. |
| `GET /api/v1/tournaments/<id>` | `{"tournament", "entrants", "sets", "me", "poll_interval"}`: the tournament, its entrants (`player`, `seed`, `dropped`, `place`), its sets in the bracket's order (`id`, `index`, `side`, `round`, `position`, `games`, `state`, `home`, `away`, `winner`, `decided_by`, `home_wins`, `away_wins`, `match`; players by id), and with a session `me`: `{"player", "entrant", "eligible", "reason", "dropped", "place", "set"}`, `set` being the player's set to play now (else the next one waiting). Ask again after `poll_interval` seconds while the page is open. |
| `POST /api/v1/tournaments/<id>/join` | Join one open for entries: its page as above. 409 when it is full, running or the player is in it. |
| `DELETE /api/v1/tournaments/<id>/join` | Leave: before the start, out of it; once it runs, the player gives up (see "Sets"). |
| `POST /api/v1/tournaments/<id>/sets/<set>/ready` | `{"ready": true}` or `false`: READY for the player's set, or taken back. `{"match": …}`, the set's match as `GET /api/v1/matches/current` shows it, or `null` for a READY taken back before there was one. 404 for a set not the player's, 409 for a set not to be played or a player with another match. |

A match's view has `tournament`, `{"id", "name", "set", "side", "round"}`
for a tournament set's match, `null` otherwise.

## The admin page

`/admin/tournaments` lists them like `GET /api/v1/tournaments`, with each
one's entrants and sets. An admin makes one (`POST /admin/api/tournaments`
with `name`, `format`, `rule_set` (an id), `best_of`, `finals_best_of`,
`max_entrants`), starts it (`POST …/<id>/start` with `{"seeding":
"joined"}`, `"shuffled"` or the players' ids in seed order), calls it off
(`POST …/<id>/cancel`), disqualifies an entrant (`POST
…/<id>/entrants/<player>/drop`), and decides a set (`POST
…/<id>/sets/<set>/decide` with `{"winner": <player id>}`, or `null` to have
it played again). A finished set can be changed until a set after it has a
result; calling off a tournament ends its open matches.
