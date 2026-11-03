# Profiles: players who sign in from the launcher

Recharged's launcher signs players in to a **Recharged profile** for
competitive play. A profile is a player, not a console: it is made the first
time someone signs in with Discord, or plays with a Mii instead (see "Playing
without Discord"), and it is no part of WFC. The game's own
logins stay NAS and GPCM's ([login.md](login.md)). Signed in, a player
challenges another to a set by a rule set the server's admins keep; both press
**Ready to play!**, their launchers start the games, the port proves which
friend code its game plays as, the two games find only each other in RANKED,
and both report the result.

The service is off unless it is configured, and the routes below then answer
404.

| Variable | Value |
|---|---|
| `PUBLIC_URL` | The address launchers and browsers use, without a path, e.g. `https://wfc.example.com`, shared with `/admin`. Plain `http://` works only on a local network, for a test server. |
| `DISCORD_CLIENT_ID` | The Discord app's client ID (Discord Developer Portal → the app → **OAuth2**). |
| `DISCORD_CLIENT_SECRET` | Its client secret, from the same page. Set **Password Mask: Yes**. |
| `PROFILES_TEST_SIGN_IN` | `true` lets anyone sign in with a name alone, so a second game on one PC has a player of its own. **Only on a local or test server.** An owner can switch it on or off on the admin dashboard's **Maintenance** page instead; that switch wins from then on. |

Discord, test sign-in, or both turn the service on, and then `PUBLIC_URL` is
needed too. Playing with a Mii needs no setting of its own: it works on every
server with the service on. In the Discord app, add the redirect
`<PUBLIC_URL>/sign-in/discord/callback` under **OAuth2 → Redirects**, e.g.
`https://wfc.example.com/sign-in/discord/callback`, or
`http://127.0.0.1:8080/sign-in/discord/callback` for a server on your own
computer. The sign-in asks for `identify` (the Discord user's id, username,
display name and avatar) and for the Discord Social SDK's
`openid sdk.social_layer_presence`, for the launcher's Discord friends (see
"Discord friends"), so the Discord app needs the Social SDK switched on in
the Developer Portal.

## Signing in

The launcher never sees a password and needs no local web server: it opens a
page in the browser and asks the server until the sign-in ends.

1. `POST /api/v1/sign-in` starts a sign-in:

   ```json
   {"id": "<43 letters and digits>", "code": "ABCD-EFGH",
    "url": "https://wfc.example.com/sign-in?id=<id>",
    "expires_in": 600, "poll_interval": 2, "link": false}
   ```

   The launcher shows the code and opens `url`; a Steam Deck can show it as a
   QR code instead. 503 when too many sign-ins are waiting (1000). `link` is
   `true` for a sign-in that connects Discord to a Mii player (see "Playing
   without Discord").
2. The page shows the same code, so that someone who got a sign-in link from
   somebody else can tell that it isn't theirs, and **Continue with
   Discord**: `/sign-in/discord` sends the browser to Discord (authorization
   code with PKCE, `state` names the sign-in). On a test server the page also
   takes a test name.
3. Discord returns to `/sign-in/discord/callback`. The server trades the code
   for the user, finds the player by its Discord id or makes one, opens a
   session, and the page says the player is signed in. A refusal (e.g.
   **Cancel** on Discord's page) ends the sign-in with the reason.
4. `GET /api/v1/sign-in/<id>`, every `poll_interval` seconds, answers
   `{"status": "waiting"}` until the sign-in ends, then once:

   ```json
   {"status": "signed_in", "token": "<43 characters>",
    "player": {"id": 7, "name": "Nelly", "avatar_url": "https://cdn.discordapp.com/…",
               "discord": "nelly", "test": false},
    "discord": {"access_token": "…", "refresh_token": "…", "expires_in": 604800,
                "scope": "identify openid sdk.social_layer_presence"}}
   ```

   or `{"status": "refused", "message": "Sign-in was cancelled."}`, or for
   a link that can't be made a 409 (see "Playing without Discord"). After
   that, or 10 minutes after it began, it is 404. `discord` comes with a
   Discord sign-in that Discord granted the SDK's scopes: Discord's tokens for
   the launcher's Discord friends, kept in memory with the sign-in until this
   answer and not after.

Sign-ins wait in the server's memory; a restart drops them, and the launcher
starts again.

### Playing without Discord

Discord is not needed to play: the launcher's **Play with a Mii** makes a
player without an account, under a name of its own.

`POST /api/v1/sign-in/mii` with `{"name": "Nelly"}` makes the player and
signs in at once, answering like a sign-in that ended:
`{"status": "signed_in", "token": "…", "player": {…, "account": "mii",
"friend_code": null}}`. The name is the profile's, which other players see,
as long as a Discord display name may be: 1 to 32 characters (counted as
characters, not bytes), without spaces around it and without control
characters or invisible formatting ones. (The Mii's own name in its record
has 10 at most; the launcher fits it.) Mii players' names need not be unique,
so the launcher shows them with their friend code. 400 for a name that isn't
one; 429 `{"error": "too many new players from here: try again in an hour"}`
after 5 new players from one address within an hour (an IPv6 address counts
with its /64). The address is the caller's, or, from a proxy on the server's
own network (loopback, private or link-local, such as a Cloudflare Tunnel's
connector), the one it names in `CF-Connecting-IP`, else last in
`X-Forwarded-For`. The count lives in memory: a restart forgets it.

A Mii player is the same as any other: friends, chat, challenges and sets,
rematches and the leaderboard. Others find it in their lists and challenge or
add it by its player id; a name looks up Discord usernames (and test names)
only.

**What identifies it.** A Mii player has nothing to sign in with until its
game logs in to WFC: the first WFC profile its port proves (`POST
/api/v1/wfc`, see "Friend codes") becomes its identity, kept in
`player_identities` as provider `wfc` with the profile id. A WFC profile
identifies one player at most, and a player keeps its first: a profile that
identifies another player, proven by a second Mii player (the same Mii on
another computer that made a new player), is only bound to that one, as any
profile is, and still identifies the first. Players with Discord or a test
name get no `wfc` identity.

**Coming back.** `POST /api/v1/sign-in/wfc` with
`{"profile_id": 1000000000, "login_ticket": "<lt>"}` (the proof of `POST
/api/v1/wfc`, sent by the game after its WFC login, without a session)
answers a new session for the player that profile identifies, as a sign-in
that ended, and binds the profile to it again. So a Mii player gets its
profile back on another computer, or after signing out or losing its session,
by logging in to WFC with the same Mii. 403 for a profile not logged in with
that ticket, 404 `{"error": "no Recharged profile signs in with this WFC
profile"}` when it identifies nobody (no player is made). A Mii player that
signs out before its game ever logged in to WFC can't come back.

**Before a new profile.** `POST /api/v1/sign-in/wfc/known` with
`{"profile_ids": [1000000000, 1000000001]}` (at most 32, no session) answers
`{"profiles": [{"profile_id": 1000000000, "sign_in": "game"}]}`: of the WFC
profiles asked about, those that play for a player already, and how that
player signs in again: `game` (the profile identifies it: its WFC login, as
above), otherwise the account of the player it is bound to (`discord`,
`test`). The signed-out launcher asks it for the Miis of its game save, so a
player picking one of them gets their profile back instead of a new one. It
names no player. One address asks it at most 20 times a minute (429 after).

**Connecting Discord.** A Mii player can connect Discord to the same profile,
keeping its friends, chat, sets and WFC profiles: the launcher starts the
sign-in above with the player's session (`Authorization: Bearer <token>` on
`POST /api/v1/sign-in`), whose answer then has `"link": true`. The page says
which profile Discord joins and offers Discord only (no test name). When
Discord returns, the player gets the Discord account; its name and picture
are Discord's from then on, it signs in with Discord, and the poll answers
`signed_in` with a new session for the same player (the old one goes on). A
Discord account that already has a Recharged profile, or a player that got
another Discord account meanwhile, ends the sign-in with 409
`{"error": "This Discord account already has a Recharged profile"}` (or
`"Your Recharged profile has a Discord account already"`), and nothing
changes. A session of any other player, an expired one or none starts an
ordinary sign-in, as before. The `wfc` identity stays: the game's login
still signs the player in too.

### Test sign-in

On a test server, `POST /api/v1/sign-in/test` with `{"name": "Mario"}`
signs in at once and answers like a sign-in that ended. A test name has 1 to
16 letters, digits, `-` or `_`; the same name in other letters is the same
player. 403 while test sign-in is off. The sign-in page shows it in a box of
its own, marked as a test server's, below Discord: a test player has no
Discord account, so no Discord friends, and nobody finds it by Discord name.

## Sessions

The launcher sends the token as `Authorization: Bearer <token>`; a missing,
expired or closed one is 401 `{"error": "signed out"}`. A session lasts 90
days. The database keeps only the token's SHA-256.

| Route | Answer |
|---|---|
| `GET /api/v1/server` | `{"version", "revision", "sign_in": {"discord": true, "test": false, "mii": true}, "discord_application": "<client id>"}`, without a session: what the launcher can offer (`discord_application` is `null` without Discord sign-in; see "Discord friends"; `mii` from 1.2.3) |
| `GET /api/v1/me` | the player, as in a sign-in, with `wfc` (see "Friend codes") and `discord_id`: the Discord account id (a string; `null` for test and Mii players), from which the launcher makes the player's Mii |
| `POST /api/v1/sign-out` | 204; the session is closed. A Mii player comes back by its game's WFC login (see "Playing without Discord") |

Every request with a session is also the player's heartbeat, as is a
sign-in: their Recharged (the launcher, or the game it started) is open. A
player without one for 60 seconds has closed it ("Online now", and the sets
it ends, see "Sets").

### Activity

The game's requests say what it is doing in a `Recharged-Activity` header:
`game` outside a match, `match` from a match's loading until it is unloaded
(offline, classic online or a set's). The launcher sends none, and neither do
builds before 1.2.0's games. `players.activity` keeps the value of the last
heartbeat (`NULL` for the launcher); a changed activity is written at once.
The lists show it, and nobody can challenge a player in a match.

In a match, games from Recharged 1.2.0 (test builds since 2026-11-02) also
say its kind in a `Recharged-Match` header: `ranked` and `friends` (online, or friends on a LAN), `tournament`
(an online tournament), `cpu` (Domination against the computer, or the
computer against itself), `local` (Domination with players on both teams at
one computer), `cup` (Road to the Striker Cup), `challenge` (a Striker
Challenge) or `training` (a Strikers 101 lesson). `players.match_kind`
keeps it with the activity; the lists show it as `match` (`null` outside a
match, for kinds this server doesn't know and from games that don't say).

Launchers and games from Recharged 1.2.0 (builds since 2026-11-02) say which
game they play in a `Recharged-Game` header: the first four characters of its
id (`R4QE`, `R4QP`, `R4QJ`, `R4QK`). `players.game` keeps the last one said (a
request without the header keeps it). A set's games meet in RANKED, which never
pairs games of two versions, so a challenge between players whose Recharged
plays different versions is refused (409, saying which), and a search asks
only players of its game.

Errors are JSON, `{"error": "<what went wrong>"}`. Every response is
`no-store`; the pages allow no scripts and no framing.

## Players

`players` has one row per player, with the name to show: the Discord display
name (or the username) at the last sign-in, a Mii player's own name, and the
time of the last heartbeat. `player_identities` holds the
accounts a player signs in with, one per provider (`discord`, `test`, and
`wfc`, a Mii player's WFC profile), with the username other players look
them up by (for `wfc` the profile's friend code, which looks up nobody) and
the avatar.

A player as the launcher sees it, in a sign-in, `GET /api/v1/me`, the lists
of `GET /api/v1/players` and a match's opponent:

```json
{"id": 7, "name": "Nelly", "avatar_url": null, "discord": null, "test": false,
 "account": "mii", "friend_code": "0868-9934-5920", "country": "CH"}
```

`account` is what the player signs in with: `discord`, `test` (a test name),
or `mii` (neither, see "Playing without Discord"). `friend_code` is the
friend code of the WFC profile that identifies the player (a Mii player's
first proven profile; it stays after Discord is connected), `null` without
one. Both from 1.2.3. `country` is the flag beside the name (see
"Countries"), `null` for none; from 1.2.11.
`player_sessions` holds the sessions, `player_wfc_profiles` the friend codes
proven theirs, `rule_sets` the rules, `matches` and `match_reports` the
matches and what each game reported. The series every Recharged build reports
online are separate ([reports.md](reports.md)).

## Countries

A player's flag: the country their launcher or game connects from, as two
capital letters of ISO 3166-1, found in DB-IP's free "IP to Country Lite"
database (CC BY 4.0, <https://db-ip.com>), which the server image carries
(`GEOIP_COUNTRY_DB` names another; without one, nobody has a flag). The
server looks it up at `GET /api/v1/me` and `GET /api/v1/players`, the lists
launchers and games read every few seconds, and keeps only the letters
(`players.country`), never the address.

The player chooses (`players.country_choice`): `auto` shows that country (the
default), `none` no flag, or two letters another one (for a VPN, or a
country of their heart). `PUT /api/v1/me/country` with `{"country": "auto"}`
(or `"none"`, or the letters) answers `{"country", "country_detected",
"country_choice"}`: the flag others see, the country found, and the choice;
422 for another value. `GET /api/v1/me` has `country_detected` and
`country_choice` too.

## Available for random games

A player can make themselves available for random games
(`players.available`, off by default). `PUT /api/v1/me/available` with
`{"available": true}` (or `false`) switches it and answers the same;
`GET /api/v1/me` has it as `available`. While a player is available,
`available` in `GET /api/v1/players` lists the other available players who
have Recharged open (at most 100, by name, as the other lists show players);
for a player who isn't, it is empty. So only those who take part see each
other. The game calls it **Open to challenges**.

## Random opponents

A player can search for a random opponent by rules they choose: `POST
/api/v1/search` with the rules as a challenge has them (`"rule_set": <id>`, or
`"rules": {…}` and `"name"`) and `"game"`, the first four characters of their
game's id (`R4QE`). It answers `{"search": {"rule_set", "rules", "since",
"asked"}}` (`since` in Unix seconds, `asked` how many players it asked so
far); 409 while the player has a set to play. Searching again starts over with
the new rules; `DELETE /api/v1/search` stops (204), withdrawing the
challenge that waits for an answer (`cancelled`).

The server sends the searcher's rules as a challenge (`"random": true`) to a
player who has Recharged open, is open to challenges or searching too, has
plays the same game (the game their Recharged says it plays, or for one that
doesn't say a WFC profile of the same id), isn't in a match
(nor in the game's own online matchmaking or match: a WFC profile of theirs
with DWC's status 2 to 6) and has no set to play, at random among them; the
oldest search asks first,
so of two searchers the one who searched first decides. The player asked
answers it as any challenge. A decline, or no answer in 30 seconds
(`expired`), and the search asks another player it hasn't asked yet; an
accepted one ends the search, and the set goes on as a challenge's. A search
whose player closed Recharged ends. Searches move on with every request to the
server. `GET /api/v1/matches/current` has the player's `search` (`null` when
they don't search) and asks again every 2 seconds while they search.

## Mii profiles

The game's own online players without a Recharged launcher show in the
launcher by their Mii and friend code: the WFC profiles logged in to GPCM, as
`{"profile_id", "mii", "game", "friend_code", "online", "status", "match",
"last_online", "friend"}`. `mii` is the Mii name of the profile's last login,
`game` the release (`R4QE`, `R4QP`, `R4QJ`, `R4QK`), `friend_code` the code
the game shows for it (`0868-9934-5920`), `status` while logged in what the
game does (`menu`, `matchmaking` or `match`, from DWC's status), `match`
whom that matchmaking or match is with (`ranked`: the game matches with
anybody only for ranked matches; `friends`: in a room one of them hosts,
whose host plays once a friend has joined; `null` otherwise, and for a game
that was already playing when the server started) and `last_online` the last
login in Unix seconds.

`GET /api/v1/players` has them twice: `mii_friends`, the Mii profiles the
player keeps as friends (`friend_profiles`, those logged in first by name,
then the last online first), and, while the player is available for random
games, `game_online`: the profiles logged in that aren't in the lists already
(not the player's own, not a player's whose launcher is open, not a Mii
friend), by Mii name, at most 100.

| Route | |
|---|---|
| `POST /api/v1/friends/miis` | `{"profile_id": <id>}`: adds that Mii profile to the player's friends, and answers `{"mii_friends": [...]}`. 404 for no such profile, 409 with the list full (200, as for players). |
| `DELETE /api/v1/friends/miis/<profile_id>` | Takes it off; the answer as above. |

## Friends

Each player keeps a list of friends on the server, so it follows them to
every computer they sign in on. It is their own list, without requests: the
player adds and removes players by hand in the launcher. Playing someone does
not make them a friend: they show among the players last played with
(`recent` in `GET /api/v1/players`) until the player adds them. Up to 1.2.0 a
challenge added its opponent and an accept the challenger, and the players of
earlier sets came in when the list began; those stay friends until removed. A
list holds at most 200 friends. The launcher shows them online first, by name,
then the others by when they were last online.

| Route | |
|---|---|
| `POST /api/v1/friends` | `{"player_id": <id>}`, or `{"name": "<Discord username>"}` instead (a Mii player only by id): adds them, and answers `{"friends": [...]}` as in `GET /api/v1/players`. One already there changes nothing. 400 for yourself, 404 for nobody of that name, 409 with the list full. |
| `DELETE /api/v1/friends/<id>` | Removes them (one not there is no error) and answers the list the same way |

## Chat

Two players who keep each other as friends can write each other: one line of
at most 300 characters per message (line breaks become spaces), at most 20
messages a minute. `chat_messages` keeps them 30 days, with when the
recipient's launcher showed them. They are not encrypted on the server: its
operator can read them in the database.

In the lists of `GET /api/v1/players`, a player has `chat` (the two keep each
other), `unread` (their messages not shown yet) and, with `chat`, `discord_id`
(for the launcher's link to their Discord profile; `null` for test players).

| Route | |
|---|---|
| `GET /api/v1/chat/<id>` | `{"messages": [{"id", "mine", "body", "sent_at"}], "can_send": true}`: the latest 50 with player `<id>`, oldest first, or with `?after=<message id>` only later ones; theirs count as read now. `can_send` is whether the two keep each other. |
| `POST /api/v1/chat/<id>` | `{"body": "..."}`: a message, 201 with it as above. 400 for an empty or too long one, 403 unless the two keep each other as friends, 429 after 20 in a minute. |

## Discord friends

The launcher can show the player's Discord friends, with who is online and
who plays Recharged, through the Discord Social SDK. Signing in with Discord
already gives the SDK's permission (the scopes
`openid sdk.social_layer_presence`, for the application in
`discord_application`): the sign-in's answer hands the launcher Discord's
tokens, and the SDK then reads the friends list from Discord itself. A player
who signed in earlier connects with the SDK's own authorization instead.
The friends list never reaches the server. The server only does what the SDK
can't do itself, as the confidential client that holds the secret: it trades
the SDK's code (and later its refresh token) for tokens, and it tells which
of the friends play Recharged. It keeps neither the tokens nor the IDs it
was asked about.

| Route | |
|---|---|
| `POST /api/v1/discord/token` | `{"code", "code_verifier", "redirect_uri"}` from the SDK's authorization, or `{"refresh_token"}` from an earlier answer: `{"access_token", "refresh_token", "expires_in", "scope"}` from Discord. The redirect must be the SDK's local web server, `http://127.0.0.1[:port]/callback` (add `http://127.0.0.1/callback` to the Discord app's redirects). 400 when Discord refuses (a used code, a link the user ended: connect again), 403 for another Discord account than the player signed in with, 404 without Discord sign-in, 502 when Discord can't be reached. |
| `POST /api/v1/players/discord` | `{"ids": ["<Discord account id>", ...]}`, at most 200: `{"players": [...]}`, those of them who have signed in to Recharged (not the player), by name, as in `GET /api/v1/players`, each with its `discord_id`. |

## Friend codes

`POST /api/v1/wfc` with `{"profile_id": 1000000000, "login_ticket": "<lt>"}`:
the port's proof that its game is logged in to WFC as that profile. The ticket
is GPCM's `lt`, which the server hands out at every login and the game keeps
(GameSpy's `gpGetLoginTicket`); an address can't prove it, as the launcher may
reach the server through a proxy or over IPv6 while the game uses IPv4. The
profile is then the player's, until another player proves it, and the answer
is `{"profile_id", "game": "R4QE", "friend_code": "0868-9934-5920", "mii"}`.
403 for a profile not logged in with that ticket. `GET /api/v1/me` lists the
player's profiles as `wfc`, the latest first. A Mii player's first proven
profile also identifies it from then on (see "Playing without Discord").

## Leaderboard

`GET /api/v1/leaderboard` gives the launcher the game's own RANKED
leaderboards, as its LEADERBOARD menu shows them: `{"season": {...},
"daily": {...}}`, the season's board (category 0) and the striker of the
day's (category 1) of `docs/gamestats2.md`. Each has `first_day` and
`last_day` of the current season (or the day), `players` on it, the first 20
as `top`, and `you`, the player's row, or `null`. As on the game's boards, a
player's WFC profiles (see "Friend codes": one for each game they play) are
one row, their points, wins and losses added up, with the Mii of the best one
(`docs/gamestats2.md`, "One row per player"). A row is `{"rank", "mii", "player", "wins",
"losses", "points", "region", "you"}`: the rank as the game counts it (equal
points share it), the Mii's name and record from the game's submission, the
Recharged player whose profile it is (`null` when no launcher proved it), and
the game's region (1 JP, 2 US, 4 EU, 8 KR). Rows of an ended season, which
stay in `scores` until their player's next ranked game, are left out.

### One player, two profiles

A person with two rows (two WFC accounts, a Mii of each version, or a Mii
player and a Discord player) is put together on the admin dashboard, on a
profile's page (owners only):

- **Link WFC profile…** puts that WFC profile together with another WFC profile of the
  same person, by its profile number (`PUT
  /admin/api/wfc-profiles/<profile>/player`, `{"profile_id": <other>}`): both
  play for the player either plays for already, or for a new one named after
  the Mii when neither does (a person who never signed in from a launcher);
  refused when they play for two players (merge those). It answers the player
  (`player_id`, `name`, `created`). Or it binds the profile to a player by
  their number (`{"player_id": <id>}`, or `null` to unlink). A launcher that
  proves a profile later binds it to its own player again (merge the made one
  into it then).
- **Keep this player and merge another…**, in two asks as the leaderboard wipe
  (`POST /admin/api/players/merge` with `{"from", "into"}` answers what it
  moves, the phrase and a token; `.../merge/confirm` with them merges): the
  WFC profiles, sets and reports, chats, friends and Mii friends and the
  sessions (their launchers stay signed in) become `into`'s, and so do the
  accounts `from` signs in with that `into` has none of; the sets between the
  two go, and `from` is gone. Refused while either has a set to play, and for
  two players who both sign in with Discord.

## Rules

A match is played by rules: one of the server's rule sets, which its admins
keep on `/admin/competitive`, or rules a challenge brings along (a player's own
preset, perhaps changed for that challenge). `GET /api/v1/rules`, without a
session, lists the rule sets: `{"rule_sets": [{"id", "name", "rules", "hash"}]}`.

| Rule | Values |
|---|---|
| `tweaks` | The port's gameplay tweaks for both teams: `fast_terrain`, `no_mega_strikes`, `thunder_island_walls`, and `mirror_match` (both players may draft the same captain). The others stay off; fair ball contests are always on online. |
| `skill` | `training`, `rookie`, `professional`, `superstar`, `legend`; `rookie` when left out. The CPU's skill: no effect on a match between two games (the game gives both teams the human level) |
| `limit` | `time` or `goals`: what ends a game |
| `time` | A game's length in seconds, 60 to 900 in steps of 30 |
| `goals` | The goals that end a game, 1 to 10 |
| `games` | Games in the series: 1, 3, 5 or 7 |
| `stadium` | `random` (the host picks, as in the game) or one stadium: `battle_dome`, `bowser_stadium`, `crater_field`, `crystal_canyon`, `the_dump`, `galactic_stadium`, `konga_coliseum`, `lava_pit`, `pipeline_central`, `storm_ship`, `the_palace`, `thunder_island`, `the_underground`, `the_vice`, `the_wastelands`, `sand_tomb` |
| `stadium_cheat` | The game's stadium cheat: `none`, `secure_stadiums`, `power_shortage`, `high_voltage`, `field_tilt`, `always_white_ball`; `none` when left out |
| `player_cheat` | `none`, `devastating_hits`, `safe_megastrike`, `always_sidekick_skillshot`, `classic_mode`; `none` when left out |
| `powerup_cheat` | `none`, `explosives`, `freezing`, `shells`, `giant`, `accelerator`, `peelin_out`, `seek_and_destroy`, `bombs_away`, `super_captains`, `infinite`, `no_power_ups`; `none` when left out |

The hash is the SHA-256 of the rules' canonical JSON: the fields in this
order, the tweaks sorted, no spaces, e.g.
`{"tweaks":["no_mega_strikes"],"skill":"rookie","limit":"time","time":180,"goals":5,"games":3,"stadium":"random","stadium_cheat":"none","player_cheat":"none","powerup_cheat":"none"}`.
Both games must play by the very same rules or the match goes out of sync, so
the server pairs them only once both confirmed it. A match keeps the rules it
was made with when its rule set changes.

The server starts with two: **Retail ranked** (the game's own ranked rules:
Rookie, 3 minutes, best of 3, random stadium) and **Competitive** (Rookie,
first to 10 goals, best of 3, random stadium, with no Mega Strikes, fast
terrain and Thunder Island walls; 3 minutes before 1.2.7).

## Matches

| Route | |
|---|---|
| `GET /api/v1/players` | `{"online": [...], "recent": [...], "friends": [...]}`: who has Recharged open (by name, at most 100), the last 10 players played a set with, and the player's friends (see "Friends"). Each is a player as in a sign-in, with `online`, `busy` (has a set to play), `activity` (while online: `launcher`, `game` or `match`, see "Activity"), `match` (in a match: its kind, see "Activity"), `game` (while online: the game their Recharged plays, `R4QE` and so on, see "Activity"), `last_online` (their last heartbeat in Unix seconds, `null` before the first) and `friend` (the player keeps them as one). `busy` counts a challenge waiting for an answer and a set that goes or plays, not a stopped one. |
| `POST /api/v1/matches` | `{"opponent": "<Discord username>", "rule_set": <id>}`, or `"opponent_id": <player id>` instead of the name (a Mii player only by id): a challenge, 201. Instead of a rule set, `"rules": {…}` (see "Rules") and `"name"`, the preset they come from (1 to 48 characters; `Custom` without one): the match's `rule_set` then is that name; 400 for rules the game doesn't have. 404 for nobody of that name, 409 when either already has a match to play, or the opponent doesn't have Recharged open or is playing a match (see "Activity"). |
| `GET /api/v1/matches/current` | `{"match": …, "search": …, "poll_interval": 2, "server_time": <unix seconds>}`: the match still to be played, or the last one that ended within the hour, or `null`; the player's search (see "Random opponents"); `poll_interval` is how soon to ask again (2 while the set waits for its players or the player searches, 10 while they play, 5 otherwise) |
| `POST /api/v1/search`, `DELETE /api/v1/search` | Search for a random opponent, and stop (see "Random opponents") |
| `POST /api/v1/matches/<id>/accept`, `…/decline` | The challenged player's answer |
| `POST /api/v1/matches/<id>/cancel` | Either player, before the result |
| `POST /api/v1/matches/<id>/ready-to-play` | `{"ready": true}` or `false`: the launcher's Ready to play!, see "Sets" |
| `POST /api/v1/matches/<id>/ready` | `{"profile_id", "rules_hash"}`: the game's, see below |
| `POST /api/v1/matches/<id>/report` | `{"games": [...]}`, see below |
| `POST /api/v1/matches/<id>/rematch` | After a set that ended (finished, disputed, or without a result): the same opponent and rules again, see "Sets" |

A match as the player sees it:

```json
{"id": 12, "state": "accepted", "side": "challenger",
 "opponent": {"id": 8, "name": "Sam", "avatar_url": "…", "discord": "sam",
              "account": "discord", "friend_code": null,
              "wfc": [{"profile_id": 1000000001, "game": "R4QE", "friend_code": "…", "mii": "Sam"}]},
 "rule_set": "Competitive", "rules": {…}, "rules_hash": "…",
 "paired": false, "reported": false, "winner": null, "games": [], "created_at": 1792771200,
 "stage": "open", "reason": null, "you_ready": true, "opponent_ready": false,
 "opponent_online": true, "go_at": null, "playing_at": null, "rematch_of": null, "random": false}
```

`random` marks a challenge a search sent (see "Random opponents"). `state` is
`challenged`, `accepted`, `declined`, `cancelled`, `expired` (a challenge
unanswered for 10 minutes, a random one for 30 seconds, a stopped set nobody
is ready for after 10 minutes, or an accepted match unchanged for 2 hours),
`dissolved` (a player closed Recharged, or the set was broken off, see
"Sets"), `finished` or `disputed`. Each player has one match to play at a
time: a challenge waiting for its answer, or a set that goes or plays. A
stopped set (`open`, see "Sets") keeps nobody from another one.

### Sets

A match is a **set**: the series both players play by its rules. `stage`
says where it is, and older launchers and games, which only know `state`,
still work:

| `stage` | `state` | |
|---|---|---|
| `invited` | `challenged` | Waiting for the challenged player's answer; the challenge is the challenger's **Ready to play!** |
| `open` | `accepted` | Both are in, but one took **Ready to play!** back (`you_ready`, `opponent_ready`) or the set waits for it again |
| `go` | `accepted` | Both are ready (`go_at`): both launchers start their games |
| `playing` | `accepted` | The games found each other in RANKED (`playing_at`) |
| `finished`, `disputed` | the same | The result |
| `dissolved` | `declined`, `cancelled`, `expired` or `dissolved` | Ended without a result; `reason` is the state |

- The accept is the challenged player's Ready to play!: with the
  challenger's, the set goes at once. Either may take theirs back before
  the games met (`open`) and press it again; their game, if it said it is
  ready, then counts as not ready until it says so again.
- A set whose games haven't met 5 minutes after the go waits for Ready to
  play! again.
- A set that stopped before its games met (`open`: a player took Ready to
  play! back, left the search or the games never met) keeps neither player
  from another set: a challenge, an accept or a search of either calls it
  off (`cancelled`), and nobody ready for it for 10 minutes ends it
  (`expired`). The launchers and games of 1.2.0 call it off at once when
  their player leaves the search, closes the game or answers **Not now**.
- A player without a heartbeat for 60 seconds has closed Recharged: their
  sets without a report of theirs end (`dissolved`), unless the other
  player's report settles them (see "Results"). So a challenger set
  lasts while both have Recharged open, and only a player with Recharged
  open can be challenged.
- A playing set with a game that stopped saying it is ready (below) 60
  seconds ago, and that neither player reported, was broken off: it ends
  without a result (`dissolved`), even while both have Recharged open. A
  set with one report waits for the other's, 5 minutes at most, or until
  the other player closed Recharged, or (when the report's last game broke
  off) until the other's game has not said it is ready for 60 seconds: then
  the one report settles it (see "Results"), or it ends without a result
  when nothing in it counts. A game that closes during its set's match
  reports the series with that game broken off, so the set is settled at
  once.
- A game saying it is ready (below) counts as its player's Ready to play!,
  for launchers without the button.
- `rematch`: the first player to ask challenges the other with the ended
  set's rules (`rematch_of` names it); the other's **Rematch** accepts it.
  A set that ended without a result (called off, expired, broken off) can
  be played again the same way.

### Meeting in RANKED

Once the match is accepted, the port says every 10 seconds that its game is
ready: logged in to WFC as `profile_id`, one of the player's profiles, and
applying the rules with `rules_hash`. Readiness lasts 30 seconds without being
said again, so a game that went away stops being kept. The answer is
`{"paired", "other_release", "replaced"}`: `paired` once both games are ready,
and the set's stage is then `playing`. A player has one game in a set: when
another of their games (another WFC profile) says it is ready while the first
still is, the newer one takes the set, and the one it replaced is answered
`replaced` and leaves it; until it stops saying it is ready, its search lists
nobody and nobody's lists it. The games go on saying it while they play the
set; `matches.home_game_ready_at` and `away_game_ready_at` keep the last time
each did, even when the answer was a 409 (such as a WFC login the server lost
in a restart, while the match goes on).

While a game is ready, RANKED in WFC Central works differently for it
([login.md](login.md), "Server browser"): its search lists only its
opponent's game once that one is ready too (whatever region group the game
puts it in), and nobody else's search lists either of them. A US and a
European game are never paired: their releases can't play each other. A set's
games don't count on the RANKED boards ([gamestats2.md](gamestats2.md),
"`put2.asp`").

### Results

After the series, each game reports what it saw, game by game:

```json
{"games": [{"my_score": 3, "opponent_score": 1, "my_captain": "mario",
            "opponent_captain": "bowser", "stadium": "the_palace"}]}
```

A report holds 1 to `games` games (the whole series, decided or not, e.g.
when it ended early); captains and stadiums are lowercase names.

A game that broke off (a player left or lost the connection; 1.2.0 games from
2026-11-03 on) is the report's last, with its score then, `"interrupted":
true` and `"played"`, how much of its clock went (0 to 1). It counts at that
score once a third of it was played and a goal was scored: a third of its
clock, or for a goal limit, a third of the goals by the leader (first to 10:
4). A 0-0 that broke off never counts. A server before 1.2.8 refuses such a
report; the games then report the series without it.

The second report settles the match: `finished` with the series' winner (more
of the counting games won; `winner` is `you`, `opponent` or `draw`) when both
saw the same games (a broken-off game's `played` may differ a little), no
result (`dissolved`) when nothing counts, and `disputed` when they saw
different games. A player reports once. The view then shows the player's own
report as `games`. When the other player closed Recharged, their game went
quiet for 60 seconds after a report whose last game broke off, or 5 minutes
pass without their report, the one report settles the set the same way.
