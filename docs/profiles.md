# Profiles: players who sign in from the launcher

Recharged's launcher signs players in to a **Recharged profile** for
competitive play. A profile is a player, not a console: it is made the first
time someone signs in with Discord, and it is no part of WFC. The game's own
logins stay NAS and GPCM's ([login.md](login.md)). This first part is the
sign-in; test matches, their rules and results come next.

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
| `GET /api/v1/me` | the player, as in a sign-in |
| `POST /api/v1/sign-out` | 204; the session is closed |

Errors are JSON, `{"error": "<what went wrong>"}`. Every response is
`no-store`; the pages allow no scripts and no framing.

## Players

`players` has one row per player, with the name to show: the Discord display
name (or the username) at the last sign-in. `player_identities` holds the
accounts a player signs in with, one per provider (`discord`, `test`), with
the username other players look them up by and the avatar.
`player_sessions` holds the sessions.
