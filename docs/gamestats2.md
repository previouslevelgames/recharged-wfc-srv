# gamestats2: leaderboards (`get2.asp` / `put2.asp`)

What the client sends and expects, worked out from the port's decomp tree.
Paths below are relative to
`~/work/mscharged-port/build/release/prepared/mscharged-decomp/patched/`.

| File | Role |
|---|---|
| `src/RVL_SDK/dwc/dwc_encsession.c` | transport: token, hash, encoding of `data`, response check |
| `src/RVL_SDK/dwc/dwc_ranksession.c` | put/get payload layout, response parsing |
| `src/RVL_SDK/dwc/dwc_ranking.c` | public API, parameter limits, row access |
| `libs/RVL_SDK/include/dwc/dwc_ranking.h` | constants, modes, regions, `DWCRnkData` |
| `src/Game/NetworkStats.cpp` | the game: init data, what it puts, what it asks for |
| `src/Game/NetworkStatsManager.cpp` | the game: how the score is computed and when it is sent |

wfc-server answers `get2` with an empty list and ignores `put2`: there is no
captured traffic to compare against. The client is the only specification.

## Constants

`NetworkStats.cpp` (`NetworkRanking::InitializeRanking`) passes this init
string to `DWC_RnkInitialize`; `DWCi_EncSessionInitialize` splits it:

```
DbfBialvJznkQWYuOrRa 00022825 00000ef9 00000200 42db60ec mschargedwii
gamestats key (20)   key1     key2     key3     key4     game name
```

| Name | Value |
|---|---|
| gamestats key | `DbfBialvJznkQWYuOrRa` |
| key1 | `0x00022825` |
| key2 | `0x00000ef9` |
| key3 | `0x00000200` |
| key4 | `0x42db60ec` |
| game name | `mschargedwii` |

`DWCi_EncValidateKey` requires `(key1 - 5) % 8 == 0`, `key2` odd and `key3` a
power of two; these values pass.

## Byte order

The port is native little-endian, and `DWCi_RankingByteSwap32` is the identity
there (`dwc_ranksession.c`, `dwc_encsession.c`). **Every 32-bit word of the
ranking protocol is little-endian.** The one exception is the 4-byte checksum
at the start of `data`, which is written big-endian byte by byte.

The game's own `userdata` (see below) is a native struct, so it is
little-endian too.

## Transport

Host `gamestats2.gs.nintendowifi.net`, port 80, plain HTTP `GET`
(`DWCi_ENC_URL_BASE` + game name + path). The game name is in the path, not in
the host:

```
/mschargedwii/web/client/put2.asp
/mschargedwii/web/client/get2.asp
```

Every operation is two requests (`DWCi_EncSessionProcess`):

```
1. GET <path>?pid=<pid>
   <- body: token, exactly 32 bytes

2. GET <path>?pid=<pid>&hash=<H>&data=<D>
   H = lowercase hex of SHA1(gamestats_key || token)
   <- body: payload || lowercase hex of SHA1(gamestats_key || b64(payload) || gamestats_key)
      or a body starting with "error:"
```

- `pid` is the GameSpy profile id, printed with `%d`.
- Request 1 is the request 2 URL cut short before `&hash` (`DWCi_EncSessionGetAsync`
  writes a `'\0'` there).
- Request 1: any body whose length is not 32 puts the client in error
  (`DWCi_EncSessionCompleteCallback`, `GETTING_TOKEN`). The token only goes into
  the SHA-1, so any 32 printable characters work.
- **The client never sends the token back**, only `H`. To check `H` the server
  has to know which token it gave to this `pid`: either store it (random token,
  expiry, used once) or derive it again (HMAC of a server secret, the pid and a
  time window). Storing it also stops a request from being replayed.
- **Nothing bounds the time between the two requests.** The client sends
  request 2 from the first `DWCi_EncSessionProcess` call after the token
  arrives, from `NetworkRanking::Update`, and has no timeout of its own. A
  refused request 2 fails the ranking session for the rest of the login
  (`mRequestFailed`), so the server keeps a token for 10 minutes.
- Anyone can ask for a token for any pid, so the server bounds what it keeps:
  an address gets 300 tokens a minute (an IPv6 address with its /64), and at
  most 50,000 tokens wait to be used. Past either, request 1 answers
  `error:too many requests from this address` or
  `error:too many requests, try again later`; the log says so once, when it
  begins. A game asks for a token per operation: a few at login and after a
  match, one per leaderboard page.
- The gamestats key is in every client binary. Anyone can compute `H` for a
  token, so the token proves that a request is fresh, not who sent it.

### `data`

`data` is base64, URL-safe alphabet **with** padding: `A-Z a-z 0-9 - _`, pad
`=` (`B64Encode(..., 2)`, `urlSafeEncodeing` in
`src/RVL_SDK/gamespy/common/gsPlatformUtil.c`). The `=` signs reach the query
string unescaped.

Decoded (`DWCi_EncSessionEncrypt`):

```
offset  size  field
0       4     checksum, big-endian: sum ^ key4
4       4     pid   (i32 LE)        ┐
8       4     size  (u32 LE)        │ obfuscated
12      size  ranking body          ┘
```

- `sum` is the `u32` sum of every plain byte from offset 4 to the end.
- `size` is the length of the ranking body (header + payload, see below).
- The `pid` here should match the query's `pid`.

Obfuscation, applied to every byte from offset 4 on:

```
seed = sum | (sum << 16)                         (u32, wrapping)
for each byte b:
    seed = (key1 * seed + key2) % key3           (u32, wrapping)
    b ^= (seed >> 16) & 0xff
```

To decode: `sum = checksum ^ key4`, run the same XOR, then sum the plain
bytes again and compare.

With these keys `seed % 0x200` is always below 512, so `(seed >> 16) & 0xff`
is always 0: **the obfuscation is a no-op and the data travels as plain
base64.** Implement the general algorithm anyway; a unit test can pin this
property.

### Response

- The body is raw binary: the base64 is used only to compute the trailing hash
  (`DWCi_EncSessionValidateResponse`).
- `len <= 40` fails ("response too short"), so the payload is at least 1 byte.
- A body starting with `error:` puts the client in error.
- The trailing hash is computed over `b64(payload)` with the same URL-safe,
  padded alphabet.

## `put2.asp`

Ranking body (`DWCi_RankingSessionPutAsync`):

```
offset  size  field
0       4     region     u32  (JP=0x01, US=0x02, EU=0x04, KR=0x08)
4       4     category   u32  (<= 100)
8       4     score      i32
12      4     size       u32  (<= 768)
16      size  userdata
```

The game always sends a `NetworkRankingSubmission` of 0x80 bytes as userdata
(`include/Game/NetworkStats.h`):

```
0x00  u8   day
0x01  u8   month
0x02  u16  year
0x04  i32  wins
0x08  i32  losses
0x0C  u16  online_region
0x0E  u16  name[11]      UTF-16LE Mii name
0x24  u8   mii[0x4C]     Mii data
0x70  u8   digest[16]    HMAC-MD5(sRankingHmacKey, bytes 0x00..0x70)
```

`sRankingHmacKey` (32 bytes) is in `NetworkStats.cpp`. Readers check the
digest and blank the rows that fail it (`ProcessLeaderboardResults`). The
server can check it to reject garbage, but the key ships in every binary: it
is integrity, not authenticity.

The client ignores the response content (no callback): any payload of at least
1 byte that does not start with `error:`, plus the hash.

### What `score` means

The client keeps the running total and sends it whole. In
`NetworkStatsManager::ReportGameResult`, for every persistent category:

- `mLocalStats[category].mScore += pointsScored`: 10 for a win, 1 for
  anything else (a draw counts as a loss), plus the goals scored, at most 10;
  a game broken off (`result` 4) gives its goals only, and no win or loss;
- a new day (striker of the day) or a new season (the others) restarts it from
  the points of this game, with wins and losses reset;
- score is clamped to 999999 (99999 on the JP and KR versions), wins and
  losses to 9999;
- then one `put2` per category with `mLocalStats[category]` as score and
  userdata.

At kickoff (`ReportDefaultDisconnectLoss`) the game books a provisional loss
and puts it: up to 5 points off and a loss more. The series' result gives the
points back and turns the loss into the result; a series that breaks off keeps
it.

`PostResetMyPlayerStats` sends a score of 0 with zeroed wins and losses, or,
after a change of Mii, the total unchanged.

The game starts its total from the row a NEAR answer shows it: at login, and
whenever it asks again (`ApplyLeaderboardToSave` takes the row with its own
pid as `mLocalStats`).

### What a put counts

A player's games add up on one row (below, "One row per player"), whichever
of their games they play, so each game is shown the player's total as its
own row, and starts from it. A put therefore counts **what the game's total
gained since the game last held one**, not the total itself
(`Leaderboard::count` in `src/gamestats/leaderboard.rs`):

- Each row of `scores` is its profile's share of its player's total in the
  season (or day). The row also keeps what the profile's game holds
  (`held_*`): the last total it put, or the row a NEAR answer showed it, with
  the date on it by the game's clock.
- A put adds `put − held` to the share and the game then holds the put. A
  put of 0 points, no wins and no losses (the game starting a season, a day,
  or as a first-time user) adds nothing. A put dated in a later season (or
  day) than what the game held, by its own clock, is its first game there and
  adds all of itself.
- A share never goes below nothing: a provisional loss on a game whose own
  share is small takes the rest from the player's other shares of the season
  (or day). What none of them has (a loss from a total of the season before)
  stays owed in what the game holds, and its next gain pays it back first,
  as the series' result does.
- A game not seen since the server began counting this way holds its row as
  it put it, and its put replaces the share, as before.

So no game counts another game's points again, however often it asks, and
switching games mid-season, linking or unlinking a profile, or two games
playing at once all add up right.

A Recharged set's games (docs/profiles.md, "Sets") meet in RANKED only to find
each other, so their puts stay off the boards: a put of a profile whose game
is kept for a set, or whose player's set is playing or ended less than 2
minutes ago, is answered as stored but counts nothing; the game holds it, so
its next RANKED put counts only what it gained since. Retail RANKED, which a
player chooses (the game's own menu, or Play RANKED in the Recharged Game
Menu), counts; a FRIENDS battle never puts.

When an admin deletes a player's rows (docs/profiles.md, "Deleting a
player"), a game logged in at the time still holds its old totals and would
put them back after its next match: its puts are answered as stored and
change nothing until it logs in again, finds no row and starts from 0.

Every put goes to `score_puts`, with what it counted (`counted`), or why not
(`set`, `deleted`): the boards can be checked, and counted again, without the
logs.

### Seasons

The game resets its own total by its clock; the boards run on UTC.

- Seasons (season, alternate season, EU friends) are weekly, on the fixed
  dates of `sNetworkSeasonDates` (`NetworkSeasonCalendar.cpp`): 1 January,
  8 January, … 24 December, then 1 January again.
- Striker of the day restarts every day.
- The game's clock is the NAS `datetime` (UTC), shifted by `g_nAddHoursTime`
  (`NetworkUpdateTask.cpp`): -8 hours for the US version, +9 for JP and KR,
  none for EU. A US day starts at 08:00 UTC.
- The boards' seasons and days are UTC's, for every game: a put counts on
  the season (or day) it comes in by UTC. Its row is dated by UTC when it is
  written; a row of an ended season (or day) moves to `past_scores` with its
  profile's next put.

At login the game asks for its own rows (NEAR). A reset follows in two
cases:

- no row of its own comes back;
- the row's date (`day`, `month`, `year` in the userdata) lies in an earlier
  season (`IsNewNetworkSeason`) or day (`IsNewNetworkDay`).

The reset is a put of score 0 (`PostResetMyPlayerStats`), which counts
nothing. The server dates every row it shows a game by that game's own
today, so a game takes the boards' season (or day) as its own at any hour:
between 00:00 and 08:00 UTC a US game is shown today's boards though its
clock still says yesterday. When its own clock then begins the day, it
restarts its total from its next game, which counts that game only; asking
again shows it the board's total.

### Categories

`NetworkStatsManager.h`, set up in the `NetworkStatsManager` constructor:

| Value | Name | Used by |
|---|---|---|
| 0 | `NETWORK_PERSISTENT_SEASON` | all regions |
| 1 | `NETWORK_PERSISTENT_STRIKER_OF_DAY` | all regions (daily board) |
| 2 | `NETWORK_PERSISTENT_FRIENDS` | EU with European rankings only |
| 3 | `NETWORK_PERSISTENT_ALTERNATE_SEASON` | EU, alternate country group |
| 4 | `NETWORK_PERSISTENT_ALTERNATE_STRIKER_OF_DAY` | EU, alternate country group |

The server keeps two boards for every game: the season's (0) and the day's
(1). The alternate country group's season (3) and day (4) count on them and
read them. An EU game with European rankings puts every season game to the
friends board (2) too: those puts count nothing more, and its gets read the
season's board. Each board holds a different score for the same player
(season total compared with today's total), so rows are keyed by
`(pid, category)`.

## `get2.asp`

Ranking body (`DWCi_RankingSessionGetAsync`):

```
offset  size  field
0       4     region     u32  (the caller's region)
4       4     category   u32
8       4     mode       u32
12      4     size       u32  (length of the parameters)
16      size  parameters
```

Parameters by mode (`DWCRnkGetParam`, limits from `DWC_RnkGetScoreAsync`):

| Mode | Value | Parameters | Size | `limit` |
|---|---|---|---|---|
| ORDER | 0 | `sort, since` | 8 | — |
| TOPLIST | 1 | `sort, limit, since` | 12 | 1..=10 |
| NEAR | 2 | `sort, limit, since` | 12 | 2..=10 |
| FRIENDS | 3 | `sort, limit, since, friends[64] (i32)` | 268 | 2..=65 |

`sort`: 0 ascending, 1 descending.

What the game asks for (`NetworkRanking::GetLeaderboardStats`):

- TOPLIST, NEAR and FRIENDS, always `sort = 1`. ORDER is never used.
- **`since` is in minutes**: for categories 1 and 4, minutes since the start of
  the network day (`GetMinutesSinceNetworkDayStart`); for the others, since
  the start of the season (`GetMinutesSinceNetworkSeasonStart`); always >= 1.
  "Network time" is the NAS-adjusted clock, so each region's season and day
  start at another hour.
- FRIENDS sends the 64 friend slots of the save; empty slots are presumably 0.
  `since = 0` there, and the client filters by season itself
  (`FilterCurrentSeason`).
- The client sorts the rows again after receiving them
  (`SortLeaderboardResults`).

The client does not settle these; the server (`src/gamestats/leaderboard.rs`)
chooses:

- The same two boards for every region and release (above, "Categories"),
  with the rows of this season (or day) by UTC: `since` is not read. The
  request's region tells the asking game's clock: every row it is shown is
  dated by its own today and signed again, so `FilterCurrentSeason` and
  `IsNewNetworkSeason` keep it; and its score range (99999 for JP and KR).
- One row per player (below): the WFC profiles of one Recharged player count
  as one row.
- Rows are sorted by score in the asked order, the earlier update first on a
  tie. Equal scores share a rank ("1, 2, 2, 4"). `total` is the number of rows
  of the season (or day), a player's counting once.
- NEAR: `limit` rows centred on the caller, shifted to stay inside the board;
  the top of the board when the caller has no row. The caller's row is what
  its game holds from then on (above, "What a put counts").
- FRIENDS: the rows of the listed pids (zero slots skipped) and of the caller,
  best first, at most `limit`, with their rank on the whole board. A player's
  row counts when any of their profiles is listed.
- ORDER: the caller's rank (0 without a row) and `total`.
- `lastupdate`: minutes since the row was stored.

### One row per player

A player who plays the US and the European game has a WFC profile in each
(the profile belongs to the game code), and so a row in each: two rows on
one board for one player. The profiles a launcher proved for one Recharged
player (`player_wfc_profiles`, docs/profiles.md "Friend codes") are that
player's, so their rows, each profile's share (above, "What a put counts"),
are merged before a get is answered (`src/gamestats/players.rs`): the score,
wins and losses (userdata 0x04 and 0x08) added up (at most 999999 and 9999,
as a put and the game allow), and
the row of the player's best profile otherwise (its pid, region, Mii and the
rest of its userdata), last updated when the latest of them was. Only rows
of one season (or, on the daily boards, one day) add up, by the date in
their userdata; a get shows only this season's (or day's). Changing the
wins and losses breaks the userdata's digest
(0x70), and the game blanks a row whose digest fails, so the server makes
the digest again with `sRankingHmacKey` (`src/gamestats/submission.rs`);
userdata whose digest failed already stays as it was. The caller's own
profile stands for its player, so the game
finds itself (NEAR, ORDER) and shows its own Mii; in FRIENDS, a listed
profile stands for its player before the others. Rows of profiles no
launcher proved stay as they are. The launcher's Leaderboard and the admin
site's Leaders merge alike, and they also leave out rows without a game (0
points, no wins, no losses, the empty record of a season or day the game puts
before it has played one): such a row counts no player there. The game's own
boards keep them.

### Response

Words little-endian (`DWCi_RankingSessionVaridateResponse`,
`DWCiRankingResponse` in `dwc_ranking.c`):

```
mode   u32

ORDER:
  order  u32
  total  u32

TOPLIST / NEAR / FRIENDS:
  count  u32
  total  u32
  count × row:
    order       u32   rank, shown as the row's position
    pid         i32
    score       i32
    region      u32
    lastupdate  u32   not read by the game
    size        u32
    userdata    [size]
```

- `total` is the number of entries on the board.
- The validator walks rows with `offset += 6 + size / 4` (integer division)
  while `DWC_RnkResGetRow` walks `size` exact bytes: **`size` has to be a
  multiple of 4** or the two disagree. The game's 0x80 bytes are fine; refuse
  puts whose size is not a multiple of 4.
- Rows whose userdata is not 0x80 bytes, or fails the HMAC, are shown with
  blank name, Mii and win/loss record.

## Worked example

A `get2` captured against wfc-server (2026-10-16, port client, pid
1000000003):

```
GET /mschargedwii/web/client/get2.asp?pid=1000000003
GET /mschargedwii/web/client/get2.asp?pid=1000000003&hash=f844edf1e6ac045ec0e5303c53eeb86af3ea4a95&data=QttitQPKmjscAAAAAgAAAAEAAAACAAAADAAAAAEAAAAKAAAAfgEAAA==
```

`data` decoded:

```
42 db 62 b5   checksum BE 0x42db62b5 = 0x00000259 ^ key4 (0x259: sum of the bytes below)
03 ca 9a 3b   pid       1000000003
1c 00 00 00   size      28 (16-byte header + 12 bytes of parameters)
02 00 00 00   region    US
01 00 00 00   category  1 (striker of the day)
02 00 00 00   mode      NEAR
0c 00 00 00   size      12
01 00 00 00   sort      descending
0a 00 00 00   limit     10
7e 01 00 00   since     382 minutes
```

The bytes after the checksum are the plain values: the obfuscation is indeed
a no-op. The hash cannot be checked here: it was made from wfc-server's
token, which depends on its secret salt.

## Trust

Everything in a request is under the client's control: the pid, the score and
the userdata. The keys that make a request "valid" are in every binary. See
"Leaderboard trust" in `CLAUDE.md`. Cheap checks that hold even now:

- the token was issued to this pid and not used yet;
- the checksum, the sizes and the pid inside `data` agree;
- `category <= 100`, `size` a multiple of 4 and <= 768, the userdata HMAC
  valid;
- score, wins and losses within the game's clamps.

A put also has to come from the pid's login (`error:pid not logged in from
this address` otherwise): from the address its GPCM connection came from, or
from the address of the NAS login whose token GPCM accepted. The game sends
its puts while it is logged in, and a pid is easy to learn, from a friend
code or the server browser. Most games use one address for both; some
networks (a carrier's NAT, a provider's web proxy) send the game's HTTP, NAS
and gamestats2 alike, out by another address than its GPCM connection. The
NAS address is signed into the token ([login.md](login.md#nas-post-ac)), so
only the login's own game brings it. Gets stay open: the leaderboards are
public.

Real trust comes later: work scores out from the match results both players'
games report ([reports.md](reports.md)). Until then a modified game can still
put any score for its own profile.
