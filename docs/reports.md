# Match reports: the series Recharged builds play online

Retail Mario Strikers Charged tells the server little of its online matches:
a ranked match puts the players' new scores to the leaderboards
([gamestats2.md](gamestats2.md)), without the opponent, a captain or the
stadium, and a friend battle sends nothing. Recharged builds report every
series they play online, ranked or with friends. Both games of a match
report; the server pairs their sides into one match and works out its result
from what both saw. `/admin` shows the matches and each player's record
([install.md](install.md#admin-dashboard)).

## The report

`POST /api/v1/wfc/reports` with a JSON body, on the HTTP port the game
already reaches for NAS and gamestats2 (TCP 80; `PORT` in the container). The
port sends it to the address in its `[online] server`, so the route answers
for any host name, e.g. `Host: 2.31.48.136`. A game behind NAT reports from
the public address it logged in to GPCM from.

```json
{
  "profile_id": 1000000000,
  "release": "R4QE",
  "event": "start",
  "series": 1792000000,
  "mode": "ranked",
  "best_of": 3,
  "side": "home",
  "mii": "Anosch",
  "opponent_mii": "Sam",
  "captains": {"home": "mario", "away": "bowser"},
  "sidekicks": {"home": ["toad", "koopa", "boo"], "away": ["birdo", "boo", "dry_bones"]},
  "game": null,
  "reason": null
}
```

| Field | |
|---|---|
| `profile_id` | the reporter's WFC profile (its Mii's profile, DWC's `gs_profile_id`) |
| `release` | the game code: `R4QE`, `R4QP`, `R4QJ` or `R4QK` |
| `event` | `start` (the series' first kickoff), `game` (a game ended) or `end` (the series is over) |
| `series` | the series' start time in Unix seconds, by the game's clock: every report of a series carries it, a later series has a larger one |
| `mode` | `ranked` or `friends` |
| `best_of` | games in the series: 1, 3, 5, 7 or 9 |
| `side` | the reporter's team: `home` or `away` |
| `mii`, `opponent_mii` | the Mii names as the reporter's game shows them: 1 to 10 characters (not bytes), no control characters |
| `captains` | per team, a captain in the game's order (`eTeamID`): `mario`, `bowser`, `daisy`, `donkey_kong`, `luigi`, `peach`, `waluigi`, `wario`, `yoshi`, `bowser_jr`, `diddy_kong`, `petey`; or `unknown` when the game had none drafted for that team |
| `sidekicks` | per team, up to three sidekicks in the game's order (`eSidekickID`): `toad`, `koopa`, `hammer_bro`, `birdo`, `boo`, `dry_bones`, `monty_mole`, `shy_guy`; fewer or none when the game does not know them. May be left out |
| `game` | for `event: "game"` only, below; `null` or left out otherwise |
| `reason` | for `event: "end"` only: `decided` (a side won the series) or `abandoned` (it stopped before: a disconnect, or a return to the menus); `null` or left out otherwise |
| `build`, `cause` | optional, see [Diagnostics](#diagnostics) |

A game:

```json
{"number": 1, "score": {"home": 3, "away": 1}, "captains": {"home": "mario", "away": "bowser"},
 "stadium": "the_palace", "ending": "normal", "seconds": 180}
```

| Field | |
|---|---|
| `number` | from 1 to `best_of` |
| `score` | goals per team, 0 to 99 |
| `captains` | as above, `unknown` allowed |
| `stadium` | one of the rule sets' stadiums ([profiles.md](profiles.md#rules)) or `unknown` |
| `ending` | `normal`, `disconnect` (the connection was lost: the score is what this side saw) or `desync` |
| `seconds` | the game's length on its clock, 0 to 3600 |
| `sync` | optional, for a `desync`: see [Diagnostics](#diagnostics) |

The answers:

| Status | Body | |
|---|---|---|
| 200 | `{"match_id": 12}` | stored, or the same report came before |
| 400 | `{"error": "mode: one of ranked, friends"}` | not JSON, or a field missing or out of range; at most 16 KiB |
| 403 | `{"error": "profile not logged in from this address"}` | see [Trust](#trust) |
| 500 | `{"error": "the server's database failed"}` | |

Fields the server does not know are ignored, so later builds can send more.

## Diagnostics

Builds that know them add what went wrong, for the Monitoring page of
`/admin`. They are optional, and one the server cannot read is dropped
without refusing the report:

| Field | |
|---|---|
| `build` | any report: the port's build, e.g. `1.1.0+g0123456789ab`; up to 64 printable characters |
| `cause` | an end: why the series ended. `decided`; `desync` (out of sync); `disconnect` (the connection was lost); `offline` (the game's online session ended without a result: a disc error, the HOME menu; a network match has no pause menu) |
| `game.sync` | a game that ended in `desync`: `{"check": "checksum", "frame": 4812}`, the first of the game's checks that failed (`frame`, `checksum` or `seed`: both machines on the same frame, with the same state checksum and random seed) and on which frame |

The server sees more itself. A game whose GPCM login ends while one of its
sides has no end yet (within 30 minutes of its last report) left WFC in the
middle of the match: its reports can no longer come. The side keeps when and
how: `logout` (the game left online play), `closed` (the connection was
closed without a logout: the game was closed or crashed), `lost` (it broke
off) or `replaced` (another login of the profile took over). A connection
that goes silent is not noticed until TCP gives up on it. Refused reports are
counted by profile with the last problem and address (`report_refusals`):
those from another address than the profile's login, and invalid ones from
its login's address. A match that shows no reports from a Recharged side is
often explained there.

## Errors

`POST /api/v1/wfc/errors`, on the same port: one body per error code the
game showed its player.

```json
{"release": "R4QE", "build": "1.1.0+g0123456789ab", "code": 86420, "phase": "matchmaking",
 "profile_id": 1000000000}
```

| Field | |
|---|---|
| `release` | the game code, as in a report |
| `code` | the number the error popup showed, 1 to 999999. DWC's codes say where: 6xxxx login, 7xxxx friends, 8xxxx matchmaking, 9xxxx other; the thousands the GameSpy part (1 GPCM, 2 persistent storage, 4 QR2, 5 server browser, 6 NAT negotiation, 7 peer connection); the rest the failure (420 repeated failures, 70 timeout…). 86420 is NAT negotiation failing in matchmaking |
| `phase` | optional: the game's session state, `none`, `login`, `matchmaking`, `prestart`, `loading`, `in_game` or `game_end` |
| `build` | optional, as in a report |
| `profile_id` | optional: the WFC profile, when the game has one |

Errors come before a login, or without one, so anyone may send them: the
server keeps the profile only when it is logged in from the address the error
came from, takes 30 errors per address every 10 minutes (then answers 429),
and keeps 30 days of them (`client_errors`).

| Status | Body | |
|---|---|---|
| 200 | `{}` | stored |
| 400 | `{"error": "code: 1 to 999999"}` | not JSON, or the release or the code wrong |
| 429 | `{"error": "too many errors from this address"}` | |

A server from before 1.0.8 answers 404: the game drops the error.

## Trust

As for gamestats2's puts ([gamestats2.md](gamestats2.md#trust)), a report
counts only from the address its `profile_id` is logged in to GPCM from: the
game reports while it is online, and a profile id is easy to learn. The rest
is the game's word. A result two games agree on is what the server keeps; a
game they saw differently stays disputed.

## Pairing

A reporter's series is its side of a match, found by `profile_id` and
`series`. The first report of a series opens the side:

- It joins an open match whose first side named this report's Mii as its
  opponent and is named by this report's `opponent_mii`, with the same
  captains (an `unknown` one agrees with any). The match has to be in
  progress, started within 3 minutes, of the same release, mode and
  `best_of`, its first side on the other team, without an end yet, and the
  other team's side still free.
- Otherwise it opens a new match. Its other side is known by the Mii name
  only until the other game joins; against a retail game it stays so.
- Both games send their start at the first kickoff, within seconds of each
  other. A series the server does not know yet (its start was lost) is opened
  by its first game or end just the same.

A report that came before, the same profile, series and event (for a game,
the same number), changes nothing: the first one stays, and the answer has
the same match. A side keeps the names and captains of its first report;
sidekicks a later report knows replace a team's empty list.

## Games and states

A game **counts** when both sides saw it the same (score, captains and
stadium; an `unknown` captain or stadium agrees with any), or when only one
side reported it. A game the two sides saw differently is **disputed** and
does not count. Endings and lengths are each side's own.

| State | When |
|---|---|
| `in_progress` | until one of the others |
| `finished` | a side won more than half the series' games (2 of 3), with or without ends; or both sides ended without that: the side that won more games wins, or it is a draw |
| `disputed` | the series is over as above, and a game is disputed |
| `abandoned` | a side ended with `abandoned` before the series was decided, or nothing came for 30 minutes and the games do not decide it |

- While both games report, only the games both saw the same decide the
  series. A game one side alone reported counts once the other's report can
  no longer come: when both sides ended, or nothing came for 30 minutes. A
  match with one reporter (the other is a retail game) is decided by its
  view.
- A game may never send its end (it was closed right after the last whistle;
  a report is tried a few times, then dropped), so the deciding game is
  enough. An end that comes after the decision changes nothing.
- An end with `abandoned` may come without any game: the session was left
  after the first kickoff.
- The 30 minutes are applied when the next report comes in, or when a page
  of `/admin` shows matches. A report that comes even later shows the series
  went on, and the match is worked out again.

## Tables

| Table | |
|---|---|
| `online_matches` | one row per match: release, mode, `best_of`, state, each team's Mii and captain, games won, winner, times |
| `online_match_sides` | each side a game reported: profile, `series`, the names and captains it saw, sidekicks, the end's reason |
| `online_match_games` | each side's view of each game |
| `online_match_events` | each report as it came, for the match's timeline |
| `report_refusals` | the refused reports of each known profile: how many, the last problem and address |
| `client_errors` | the error codes the games showed, 30 days |

These matches are not the profiles' test matches ([profiles.md](profiles.md#matches)),
which two players who signed in from the launcher arrange and report
themselves: every Recharged build reports its online series, signed in or not.
