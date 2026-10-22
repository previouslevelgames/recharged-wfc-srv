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

- `mLocalStats[category].mScore += pointsScored`;
- a new day (striker of the day) or a new season (the others) restarts it from
  the points of this game, with wins and losses reset;
- score is clamped to 999999, wins and losses to 9999;
- then one `put2` per category with `mLocalStats[category]` as score and
  userdata.

`PostResetMyPlayerStats` sends a score of 0 with zeroed wins and losses.

So **a put replaces the stored row**: the server keeps the last value per
`(pid, category)`. Adding puts up would count every past game again on each
match and could never reset.

### Categories

`NetworkStatsManager.h`, set up in the `NetworkStatsManager` constructor:

| Value | Name | Used by |
|---|---|---|
| 0 | `NETWORK_PERSISTENT_SEASON` | all regions |
| 1 | `NETWORK_PERSISTENT_STRIKER_OF_DAY` | all regions (daily board) |
| 2 | `NETWORK_PERSISTENT_FRIENDS` | EU with European rankings only |
| 3 | `NETWORK_PERSISTENT_ALTERNATE_SEASON` | EU, alternate country group |
| 4 | `NETWORK_PERSISTENT_ALTERNATE_STRIKER_OF_DAY` | EU, alternate country group |

Each category holds a different score for the same player (season total
compared with today's total), so rows are keyed by `(pid, category)` even with a
single, region-less leaderboard.

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
  Only rows updated in the last `since` minutes belong in the answer. The
  client works out the day and season boundaries; the server only needs each
  row's last update time. "Network time" is the NAS-adjusted clock.
- FRIENDS sends the 64 friend slots of the save; empty slots are presumably 0.
  `since = 0` there, and the client filters by season itself
  (`FilterCurrentSeason`).
- The client sorts the rows again after receiving them
  (`SortLeaderboardResults`).

The client does not settle these; the server (`src/gamestats/leaderboard.rs`)
chooses:

- One leaderboard per category for every region: the request's region is
  ignored.
- Rows are sorted by score in the asked order, the earlier update first on a
  tie. Equal scores share a rank ("1, 2, 2, 4"). `total` is the number of rows
  that pass the `since` filter.
- `since = 0` keeps every row, in every mode.
- NEAR: `limit` rows centred on the caller, shifted to stay inside the board;
  the top of the board when the caller has no row.
- FRIENDS: the rows of the listed pids (zero slots skipped) and of the caller,
  best first, at most `limit`, with their rank on the whole board.
- ORDER: the caller's rank (0 without a row) and `total`.
- `lastupdate`: minutes since the row was stored.

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

A put also has to come from the address the pid is logged in to GPCM from
(`error:pid not logged in from this address` otherwise): the game sends its
puts while it is logged in, and a pid is easy to learn, from a friend code or
the server browser. Gets stay open: the leaderboards are public.

Real trust comes later: work scores out from match results reported by both
players. Until then a modified game can still put any score for its own
profile.
