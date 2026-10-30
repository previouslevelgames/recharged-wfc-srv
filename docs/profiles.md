# Profiles: players who sign in from the launcher

Recharged's launcher signs players in to a **Recharged profile** for
competitive play. A profile is a player, not a console: it is made the first
time someone signs in with Discord, and it is no part of WFC. The game's own
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
| `PROFILES_TEST_SIGN_IN` | `true` lets anyone sign in with a name alone, so a second game on one PC has a player of its own. **Only on a local or test server.** |

Discord, test sign-in, or both turn the service on, and then `PUBLIC_URL` is
needed too. In the Discord app, add the redirect
`<PUBLIC_URL>/sign-in/discord/callback` under **OAuth2 → Redirects**, e.g.
`https://wfc.example.com/sign-in/discord/callback`, or
`http://127.0.0.1:8080/sign-in/discord/callback` for a server on your own
computer. The sign-in asks only for `identify`: the Discord user's id,
username, display name and avatar.

## Signing in

The launcher never sees a password or a Discord token, and needs no local web
server: it opens a page in the browser and asks the server until the
sign-in ends.

1. `POST /api/v1/sign-in` starts a sign-in:

   ```json
   {"id": "<43 letters and digits>", "code": "ABCD-EFGH",
    "url": "https://wfc.example.com/sign-in?id=<id>",
    "expires_in": 600, "poll_interval": 2}
   ```

   The launcher shows the code and opens `url`; a Steam Deck can show it as a
   QR code instead. 503 when too many sign-ins are waiting (1000).
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
               "discord": "nelly", "test": false}}
   ```

   or `{"status": "refused", "message": "Sign-in was cancelled."}`. After
   that, or 10 minutes after it began, it is 404.

Sign-ins wait in the server's memory; a restart drops them, and the launcher
starts again.

On a test server, `POST /api/v1/sign-in/test` with `{"name": "Mario"}`
signs in at once and answers like a sign-in that ended. A test name has 1 to
16 letters, digits, `-` or `_`; the same name in other letters is the same
player. 403 without `PROFILES_TEST_SIGN_IN`.

## Sessions

The launcher sends the token as `Authorization: Bearer <token>`; a missing,
expired or closed one is 401 `{"error": "signed out"}`. A session lasts 90
days. The database keeps only the token's SHA-256.

| Route | Answer |
|---|---|
| `GET /api/v1/server` | `{"version", "revision", "sign_in": {"discord": true, "test": false}}`, without a session: what the launcher can offer |
| `GET /api/v1/me` | the player, as in a sign-in, with `wfc` (see "Friend codes") and `discord_id`: the Discord account id (a string; `null` for test players), from which the launcher makes the player's Mii |
| `POST /api/v1/sign-out` | 204; the session is closed |

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

Errors are JSON, `{"error": "<what went wrong>"}`. Every response is
`no-store`; the pages allow no scripts and no framing.

## Players

`players` has one row per player, with the name to show: the Discord display
name (or the username) at the last sign-in, and the time of the last
heartbeat. `player_identities` holds the
accounts a player signs in with, one per provider (`discord`, `test`), with
the username other players look them up by and the avatar.
`player_sessions` holds the sessions, `player_wfc_profiles` the friend codes
proven theirs, `rule_sets` the rules, `matches` and `match_reports` the
matches and what each game reported. The series every Recharged build reports
online are separate ([reports.md](reports.md)).

## Friends

Each player keeps a list of friends on the server, so it follows them to
every computer they sign in on. It is their own list, without requests: a
challenge adds its opponent, accepting a challenge adds the challenger, and the
launcher adds and removes players by hand. The players of earlier sets came in
the same way when the list began. A list holds at most 200 friends. The
launcher shows them online first, by name, then the others by when they were
last online.

| Route | |
|---|---|
| `POST /api/v1/friends` | `{"player_id": <id>}`, or `{"name": "<Discord username>"}` instead: adds them, and answers `{"friends": [...]}` as in `GET /api/v1/players`. One already there changes nothing. 400 for yourself, 404 for nobody of that name, 409 with the list full. |
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

## Friend codes

`POST /api/v1/wfc` with `{"profile_id": 1000000000, "login_ticket": "<lt>"}`:
the port's proof that its game is logged in to WFC as that profile. The ticket
is GPCM's `lt`, which the server hands out at every login and the game keeps
(GameSpy's `gpGetLoginTicket`); an address can't prove it, as the launcher may
reach the server through a proxy or over IPv6 while the game uses IPv4. The
profile is then the player's, until another player proves it, and the answer
is `{"profile_id", "game": "R4QE", "friend_code": "0868-9934-5920", "mii"}`.
403 for a profile not logged in with that ticket. `GET /api/v1/me` lists the
player's profiles as `wfc`, the latest first.

## Rules

A rule set is what a match is played by, and the server's admins keep them on
`/admin/competitive`. `GET /api/v1/rules`, without a session, lists them:
`{"rule_sets": [{"id", "name", "rules", "hash"}]}`.

| Rule | Values |
|---|---|
| `tweaks` | The port's gameplay tweaks for both teams: `fast_terrain`, `no_mega_strikes`, `thunder_island_walls`. The others stay off; fair ball contests are always on online. |
| `skill` | `training`, `rookie`, `professional`, `superstar`, `legend` |
| `limit` | `time` or `goals`: what ends a game |
| `time` | A game's length in seconds, 60 to 900 in steps of 30 |
| `goals` | The goals that end a game, 1 to 10 |
| `games` | Games in the series: 1, 3, 5 or 7 |
| `stadium` | `random` (the host picks, as in the game) or one stadium: `battle_dome`, `bowser_stadium`, `crater_field`, `crystal_canyon`, `the_dump`, `galactic_stadium`, `konga_coliseum`, `lava_pit`, `pipeline_central`, `storm_ship`, `the_palace`, `thunder_island`, `the_underground`, `the_vice`, `the_wastelands`, `sand_tomb` |

The hash is the SHA-256 of the rules' canonical JSON: the fields in this
order, the tweaks sorted, no spaces, e.g.
`{"tweaks":["no_mega_strikes"],"skill":"rookie","limit":"time","time":180,"goals":5,"games":3,"stadium":"random"}`.
Both games must play by the very same rules or the match goes out of sync, so
the server pairs them only once both confirmed it. A match keeps the rules it
was made with when its rule set changes.

The server starts with two: **Retail ranked** (the game's own ranked rules:
Rookie, 3 minutes, best of 3, random stadium) and **Competitive** (the same
with no Mega Strikes, fast terrain and Thunder Island walls).

## Matches

| Route | |
|---|---|
| `GET /api/v1/players` | `{"online": [...], "recent": [...], "friends": [...]}`: who has Recharged open (by name, at most 100), the last 10 players played a set with, and the player's friends (see "Friends"). Each is a player as in a sign-in, with `online`, `busy` (has a set to play), `activity` (while online: `launcher`, `game` or `match`, see "Activity"), `last_online` (their last heartbeat in Unix seconds, `null` before the first) and `friend` (the player keeps them as one). |
| `POST /api/v1/matches` | `{"opponent": "<Discord username>", "rule_set": <id>}`, or `"opponent_id": <player id>` instead of the name: a challenge, 201. 404 for nobody of that name, 409 when either already has a match to play, or the opponent doesn't have Recharged open or is playing a match (see "Activity"). |
| `GET /api/v1/matches/current` | `{"match": …, "poll_interval": 2, "server_time": <unix seconds>}`: the match still to be played, or the last one that ended within the hour, or `null`; `poll_interval` is how soon to ask again (2 while the set waits for its players, 10 while they play, 5 otherwise) |
| `POST /api/v1/matches/<id>/accept`, `…/decline` | The challenged player's answer |
| `POST /api/v1/matches/<id>/cancel` | Either player, before the result |
| `POST /api/v1/matches/<id>/ready-to-play` | `{"ready": true}` or `false`: the launcher's Ready to play!, see "Sets" |
| `POST /api/v1/matches/<id>/ready` | `{"profile_id", "rules_hash"}`: the game's, see below |
| `POST /api/v1/matches/<id>/report` | `{"games": [...]}`, see below |
| `POST /api/v1/matches/<id>/rematch` | After a finished or disputed set: the same opponent and rules again, see "Sets" |

A match as the player sees it:

```json
{"id": 12, "state": "accepted", "side": "challenger",
 "opponent": {"id": 8, "name": "Sam", "avatar_url": "…", "discord": "sam",
              "wfc": [{"profile_id": 1000000001, "game": "R4QE", "friend_code": "…", "mii": "Sam"}]},
 "rule_set": "Competitive", "rules": {…}, "rules_hash": "…",
 "paired": false, "reported": false, "winner": null, "games": [], "created_at": 1792771200,
 "stage": "open", "reason": null, "you_ready": true, "opponent_ready": false,
 "opponent_online": true, "go_at": null, "playing_at": null, "rematch_of": null}
```

`state` is `challenged`, `accepted`, `declined`, `cancelled`, `expired` (a
challenge unanswered for 10 minutes, or an accepted match unchanged for 2
hours), `dissolved` (a player closed Recharged, see "Sets"), `finished` or
`disputed`. Each player has one match to play at a time.

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
  the games met (`open`) and press it again.
- A set whose games haven't met 5 minutes after the go waits for Ready to
  play! again.
- A player without a heartbeat for 60 seconds has closed Recharged: their
  sets without a report of theirs end (`dissolved`). So a challenger set
  lasts while both have Recharged open, and only a player with Recharged
  open can be challenged.
- A game saying it is ready (below) counts as its player's Ready to play!,
  for launchers without the button.
- `rematch`: the first player to ask challenges the other with the ended
  set's rules (`rematch_of` names it); the other's **Rematch** accepts it.

### Meeting in RANKED

Once the match is accepted, the port says every 10 seconds that its game is
ready: logged in to WFC as `profile_id`, one of the player's profiles, and
applying the rules with `rules_hash`. Readiness lasts 30 seconds without being
said again, so a game that went away stops being kept. The answer is
`{"paired", "other_release"}`: `paired` once both games are ready, and the
set's stage is then `playing`.

While a game is ready, RANKED in WFC Central works differently for it
([login.md](login.md), "Server browser"): its search lists only its
opponent's game once that one is ready too (whatever region group the game
puts it in), and nobody else's search lists either of them. A US and a
European game are never paired: their releases can't play each other.

### Results

After the series, each game reports what it saw, game by game:

```json
{"games": [{"my_score": 3, "opponent_score": 1, "my_captain": "mario",
            "opponent_captain": "bowser", "stadium": "the_palace"}]}
```

A report holds 1 to `games` games (the whole series, decided or not, e.g.
when it ended early); captains and stadiums are lowercase names.
The second report settles the match: `finished` with the series' winner (more
games won; `winner` is `you`, `opponent` or `draw`) when both saw the same
games, `disputed` otherwise. A player reports once. The view then shows the
player's own report as `games`.
