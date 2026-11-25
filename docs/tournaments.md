# Tournaments

The server runs tournaments: a bracket of single or double elimination (or
single with an ash bracket) whose sets are the server's own matches ([profiles.md](profiles.md), "Matches").
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
| `format` | `single` or `double` elimination, or `single_ash`: single elimination with an ash bracket |
| `state` | `registration` (open for entries), `running`, `finished` or `cancelled` |
| `rule_set`, `rules` | The rules its matches are played by, copied from a rule set when it was made |
| `best_of`, `semifinals_best_of`, `finals_best_of` | Games of a set, of a semifinal and of a final (1, 3, 5 or 7: first to 1, 2, 3 or 4). Finals: in single elimination the last round (and the ash bracket's); in double the winners' final, the losers' final, the grand final and its reset. Semifinals: the round before the final on the winners', the losers' and the ash bracket's side (none on a side of one round) |
| `max_entrants` | The most who can join (2 to 256), or `null` |

When it starts, the entrants get their seeds (in the order they joined, at
random, or as the admin gives them) and the bracket its sets. Byes fill the
first round up to a power of 2, against the best seeds (1 against 8, 4
against 5, …). In double elimination the losers of each winners' round drop
into the losers' bracket crossed, so two players don't meet again soon; the
grand final's reset is played only when the player from the losers' bracket
wins the grand final.

With an ash bracket (`single_ash`), the losers of the first round play a
single elimination of their own, its sets on the side `ash` (with 16
entrants, 8 players from the quarterfinals). Only the sets of the first
round that are played count: a bye is no loss, so with 12 entrants the ash
bracket has 4 players, with 10 it is one final, and with fewer than 2 such
sets there is none. It is seeded by the seed expected to lose each of those
sets (the worse one), byes to the best, and a set of it is ready as soon as
the two sets its players come from are decided, alongside the main bracket.
It gives no places: an entrant's `place` is the one of the main bracket, and
the tournament ends when both are over.

## Sets

| `state` | |
|---|---|
| `pending` | Waiting for its players: the sets they come from aren't decided |
| `ready` | Both players are known: to be played |
| `playing` | Its match is open (`match`) |
| `finished` | `winner`, the games each won (`home_wins`, `away_wins`), and `decided_by`: `match`, `organizer`, or `walkover` (nobody came against the winner, or the other left) |
| `disputed` | The two games reported different series, or a draw: the organizer decides |
| `void` | Nobody plays it: both places are empty, or a reset that isn't needed |

A set is played one game at a time, each a match of one game, so the
players draft again before each: the first player's READY makes the game's
match, accepted at once, by the tournament's rules with one game, and counts
as that player's Ready to play!; the other's READY makes it go. Its winner
has one more game (`home_wins`, `away_wins`), and the set is theirs once
they have more than half of its `games`; until then it is `ready` for the
next game. A game without a winner is played again. A player who forfeits
a game gives that game up and the set goes on.

A game that breaks off (a player closed their game or lost the connection)
is not counted: the set's next game goes on from it. Its `resume`, from the
game's own match report ([reports.md](reports.md): the last game either
player's game reported with `"ending": "disconnect"` since the match was
played), says the stadium, the seconds on its clock, and each team as it
was: its player, captain, sidekicks and score. The next game's match is
played in that stadium, on the same sides (`game_home` is the resumed game's
home player), and the games skip the draft and start from that score and
clock. Without such a report (or with a team's captain or sidekicks
unknown) the game is played again from the start. A game that ends normally
clears it. The players take the game's home side in turn: the lower seed (the higher
number) in the first game, the other in the second, and so on
(`game_home`; the game swaps its sides to follow it). From there it is a
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
| `GET /api/v1/tournaments` | `{"tournaments": [...]}`: open for entries or running, the newest first, then the last 10 that ended. With a session, each says whether the player `joined`, and their `place` once it ended (`null` before). |
| `GET /api/v1/tournaments/<id>` | `{"tournament", "entrants", "sets", "me", "poll_interval"}`: the tournament, its entrants (`player`, `seed`, `dropped`, `place`), its sets in the bracket's order (`id`, `index`, `side`, `round`, `position`, `games`, `state`, `home`, `away`, `winner`, `decided_by`, `home_wins`, `away_wins`, `match`, `game_home` (who plays at home in its next game while it is `ready` or `playing`), `played` (the games it played, in their order: `home_captain`, `away_captain`, `home_score`, `away_score`, `stadium`, `interrupted`, from its home player's report of each finished match, else its away player's), and `from`, where its two players come from: `{"seed": n}` (from 0; past the entrants, a bye), `{"winner": <set id>}` or `{"loser": <set id>}`; players by id), and with a session `me`: `{"player", "entrant", "eligible", "reason", "dropped", "place", "set"}`, `set` being the player's set to play now (else the next one waiting). Ask again after `poll_interval` seconds while the page is open. |
| `POST /api/v1/tournaments/<id>/join` | Join one open for entries: its page as above. 409 when it is full, running or the player is in it. |
| `DELETE /api/v1/tournaments/<id>/join` | Leave: before the start, out of it; once it runs, the player gives up (see "Sets"). |
| `POST /api/v1/tournaments/<id>/sets/<set>/ready` | `{"ready": true}` or `false`: READY for the player's set, or taken back. `{"match": …}`, the set's match as `GET /api/v1/matches/current` shows it, or `null` for a READY taken back before there was one. 404 for a set not the player's, 409 for a set not to be played or a player with another match. |

A match's view has `tournament`, `{"id", "name", "set", "side", "round",
"games", "home_wins", "away_wins", "won", "lost", "state", "winner",
"home"}` for a tournament set's match, `null` otherwise: the set's best of,
the games each place has won so far, the same as the player's (`won`) and
the opponent's (`lost`), the set's `state`, its `winner` once finished
(`"you"` or `"opponent"`), who plays at home in its next game (`"you"`
or `"opponent"`), and `resume`, `null` or the game the next one goes on
from: `{"stadium", "seconds", "home": "you" | "opponent", "you":
{"captain", "sidekicks", "score"}, "opponent": {...}}`.

## The admin page

`/admin/tournaments` lists them like `GET /api/v1/tournaments`, with each
one's entrants and sets. An admin makes one (`POST /admin/api/tournaments`
with `name`, `format`, `rule_set` (an id), `best_of`, `semifinals_best_of`,
`finals_best_of`, `max_entrants`), starts it (`POST …/<id>/start` with `{"seeding":
"joined"}`, `"shuffled"` or the players' ids in seed order), calls it off
(`POST …/<id>/cancel`), disqualifies an entrant (`POST
…/<id>/entrants/<player>/drop`), and decides a set (`POST
…/<id>/sets/<set>/decide` with `{"winner": <player id>}`, or `null` to have
it played again). A finished set can be changed until a set after it has a
result; calling off a tournament ends its open matches, and nobody plays its
sets not played yet.

A player still in a tournament open or running (not left) can't be deleted,
nor merged with a player in the same one: take them out of it first. Merged,
`from`'s places in tournaments become `into`'s; deleted, the entrant stays
without its player (`player_id` null), so the seeds and the bracket stay as
they were.
