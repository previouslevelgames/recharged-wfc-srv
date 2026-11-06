# Installing the server

One container runs the server and PostgreSQL 18. To run several servers on
one database, PostgreSQL gets a container of its own instead
([Several servers on one database](#several-servers-on-one-database)). The
image is `ghcr.io/previouslevelgames/recharged-wfc-srv`; `stable` is the latest
release.
`GET /status` returns `{"status":"ok","revision":"<Git commit SHA>"}`.

The game reaches the server on five ports:

| Port | Service |
|---|---|
| TCP 80 | NAS (registration and login), gamestats2 (leaderboards) and Recharged's match reports, plus `/status` and `/admin` |
| TCP 29900 | GPCM: logins, friends and their status |
| UDP 27900 | QR2 master: the games' heartbeats |
| TCP 28910 | Server browser: finding a match |
| UDP 27901 | NAT negotiation: connecting two games |

## Unraid setup

In **Docker → Add Container**, enable **Advanced View** and use these settings:

| Setting | Value |
|---|---|
| Name | `recharged-wfc-srv` |
| Repository | `ghcr.io/previouslevelgames/recharged-wfc-srv:stable` |
| Network | `bridge` |
| Port (HTTP) | Config Type **Port**: Container Port `8080`, Host Port `80`, TCP. The game sends NAS and gamestats2 requests to port 80. |
| Port (GPCM) | Config Type **Port**: Container Port `29900`, Host Port `29900`, TCP |
| Port (master) | Config Type **Port**: Container Port `27900`, Host Port `27900`, UDP |
| Port (server browser) | Config Type **Port**: Container Port `28910`, Host Port `28910`, TCP |
| Port (NAT negotiation) | Config Type **Port**: Container Port `27901`, Host Port `27901`, UDP |
| Path (SQL data) | Config Type **Path**: Container Path `/var/lib/postgresql`, Host Path `/mnt/user/appdata/recharged-wfc-srv/postgres`, Access Mode **Read/Write** |
| Path (logs) | Config Type **Path**: Container Path `/var/log/recharged-wfc-srv`, Host Path `/mnt/user/appdata/recharged-wfc-srv/logs`, Access Mode **Read/Write** |
| WebUI | `http://[IP]:[PORT:8080]/status` |
| Extra Parameters (Advanced) | `--stop-timeout=30` |
| Autostart | Enabled |

Unraid's own web UI uses port 80 by default. Move it (**Settings → Management
Access → HTTP port**, e.g. `8088`), or give the container its own address
(**Network** `Custom: br0`, a fixed IP, no port mappings) and set `PORT` to `80`
with the extra parameter `--sysctl net.ipv4.ip_unprivileged_port_start=80`.

## Environment variables

Configure these directly in the Unraid container editor; no `.env` file is needed.
Choose **Add another Path, Port, Variable, Label or Device → Config Type: Variable**,
enter the **Key** and **Value** from the table, and set **Password Mask: Yes** for
`POSTGRES_PASSWORD`. Optional variables already have defaults in the image.

| Variable (Key) | Required | Value |
|---|---|---|
| `POSTGRES_PASSWORD` | yes | Generate a password in the Unraid terminal with `openssl rand -hex 32` and paste the output. Keep it across container updates. |
| `POSTGRES_DB` | no | Database name. Default: `recharged_wfc`. |
| `POSTGRES_USER` | no | Database user. Default: `recharged_wfc`. |
| `POSTGRES_HOST` | no | The name or address of a database container, e.g. `recharged-wfc-db`, for [several servers on one database](#several-servers-on-one-database). The container then starts no PostgreSQL of its own and needs no **Path (SQL data)**. Unset: the bundled PostgreSQL. |
| `POSTGRES_PORT` | no | The database's port. Default: `5432`. |
| `PGDATA` | no | SQL data directory inside the container. Default: `/var/lib/postgresql/18/docker`, beneath the mapped `/var/lib/postgresql` path. Leave unset to use this default. |
| `PORT` | no | HTTP port inside the container (NAS, gamestats2, `/status`, `/admin`). Default: `8080`; the Host Port mapping makes it 80. |
| `GPCM_PORT` | no | GPCM port inside the container. Default: `29900`. |
| `MASTER_PORT` | no | UDP port of the master server inside the container. Default: `27900`. |
| `SERVER_BROWSER_PORT` | no | TCP port of the server browser inside the container. Default: `28910`. |
| `NATNEG_PORT` | no | UDP port of NAT negotiation inside the container. Default: `27901`. |
| `NATNEG_RELAY_ADDRESS` | for the relay | The IPv4 address the players' games reach this server at, as in their `online.server` (a host name works too), e.g. `203.0.113.10`. It switches on the relay for games that cannot reach each other; see [NAT relay](#nat-relay). Unset, there is no relay. |
| `NATNEG_RELAY` | no | Which matches the relay carries: `auto` (default) only those whose games cannot reach each other, `always` every match (to try it out), `off` none. |
| `NATNEG_RELAY_MAX` | no | Relayed matches at once, at most. Default: `16`. Further matches try without the relay. |
| `PUBLIC_URL` | for `/admin` and profiles | The HTTPS address browsers use for this server, without a path, e.g. `https://wfc.example.com`. See [Admin dashboard](#admin-dashboard). |
| `GOOGLE_CLIENT_ID` | for `/admin` | Client ID of the Google OAuth client. |
| `GOOGLE_CLIENT_SECRET` | for `/admin` | Client secret of the Google OAuth client. Set **Password Mask: Yes**. |
| `ADMIN_GOOGLE_EMAILS` | for `/admin` | Google addresses allowed in, comma-separated, e.g. `you@gmail.com, teammate@gmail.com`. |
| `OWNER_GOOGLE_EMAILS` | no | Owners, above admins: Google addresses, comma-separated, e.g. `you@gmail.com`. They sign in too, and only they see **Maintenance**. Unset, no one does. |
| `ADMIN_DIAGNOSTICS_TOKEN` | no | A secret of at least 32 characters without spaces. Sent as `Authorization: Bearer <token>`, it reads the dashboard's data without a Google sign-in, for diagnosing from a script; see [Diagnosing a set](#diagnosing-a-set). It reads every page but **Maintenance** and changes nothing. Set **Password Mask: Yes**. Unset, there is none. |
| `DISCORD_CLIENT_ID` | for profiles | Client ID of the Discord app players sign in with. See [profiles.md](profiles.md). |
| `DISCORD_CLIENT_SECRET` | for profiles | Its client secret. Set **Password Mask: Yes**. |
| `PROFILES_TEST_SIGN_IN` | no | `true` lets anyone sign in to a profile with a name alone, for tests with two games on one PC. Never on a public server. An owner's switch on **Maintenance** wins over it. |
| `GEOIP_COUNTRY_DB` | no | The country database for players' flags ([profiles.md](profiles.md#countries)). Default: the one the image carries, DB-IP's free country database of the month it was built (CC BY 4.0, [db-ip.com](https://db-ip.com)). Unreadable, players have no flags. |
| `SERVER_LABEL` | no | A short name such as `DEV` for a test server: `/admin` and the sign-in pages show it in their header and tab title, with an orange tab icon, so they are not mistaken for the live server's. At most 16 letters, digits, spaces, `-`, `_` or `.`. Unset, no label. |
| `LOG_DIR` | no | Log folder inside the container. Default: `/var/log/recharged-wfc-srv`, the mapped **Path (logs)**. Leave unset to use this default. |
| `LOG_RETENTION_DAYS` | no | Days of log files to keep. Default: `14`. |
| `RUST_LOG` | no | Which messages to log. Default: `info`. `info,tower_http=debug` adds one line per HTTP request. |

With the mapping above, the **SQL data lives on your Unraid server at**
`/mnt/user/appdata/recharged-wfc-srv/postgres/18/docker`.
To store it elsewhere on the server, choose another **Host Path** for
**Path (SQL data)**. Keep this mapping when updating or replacing the container
so the database persists. `PGDATA` refers to a path inside the container;
keep it at its default for this setup.

`POSTGRES_DB`, `POSTGRES_USER`, and `POSTGRES_PASSWORD` initialize an empty data
folder; changing them later does not update an existing database or its credentials.

PostgreSQL must never run in two containers on the same data folder: stop a
database container before giving its folder back to the server container, and
the other way round.

Click **Apply**. The container starts PostgreSQL on internal `127.0.0.1:5432`, then
the server. Open `http://<unraid-ip>/status` to check the running revision. The
container health check checks both.

## Logs

The server writes one log file per day (UTC) to **Path (logs)**, e.g.
`/mnt/user/appdata/recharged-wfc-srv/logs/recharged-wfc-srv.2026-10-23.log`,
and deletes the oldest once there are more than `LOG_RETENTION_DAYS`. The files
survive container updates. Admins can also read them, and download a whole day,
on the dashboard's **Logs** page (`/admin/logs`), without access to the server.
The container log (**Docker → container icon → Logs**) shows the same lines plus
PostgreSQL's, but Docker deletes it whenever Unraid recreates the container,
e.g. on an update.

## Connecting the game

The port (Recharged's `online` branch) sends every Nintendo WFC name to one
IPv4 address. Set it in the game's `recharged.ini`:

```ini
[online]
server = 192.168.1.42
```

That address must reach the container on TCP 80, TCP 29900, UDP 27900, TCP
28910 and UDP 27901; for players outside your network, forward those five ports
on the router, or run the container on a server with its own public address.
The traffic is plain HTTP and GameSpy's own protocols, so it cannot go through a
Cloudflare Tunnel. On its first login the game registers the console and keeps
the userid in `data/shared2/DWC_AUTHDATA`.

## NAT relay

Two games normally reach each other directly: NAT negotiation tells each the
other's public address. Some internet connections give a game a new public
port for every address it sends to, carrier-grade NAT (one public address
shared by many customers) above all. The other game's packets then never
arrive, and the game stops with error 86070 ("Disconnected from network during
data transmission"), whoever hosts.

With `NATNEG_RELAY_ADDRESS` set, NAT negotiation gives such a pair this
server's address instead, and the server passes every packet of their match on
to the other game. It takes a pair when one game's router gives it a new port
per address (its heartbeats and its negotiation come from different ports), or
when the same two public addresses failed without it in the last 24 hours, so
the second try works. It needs no other port: the games send to UDP 27901,
which they already reach. Set the address the games use, exactly: the relay
only recognises a game by the address its packets to that IP come from.

A relayed match costs the server its traffic twice, in and out, and the players
the way through the server, which adds to their lag. The log has one line per
negotiation (`connect`, with each game's `nat` and the `relay` decision) and
one per relayed match when it ends (`relay ended`, with its `packets` and
`kib`); the dashboard's **Matchmaking** shows "through the relay".

## Admin dashboard

`/admin` shows the server to admins who sign in with Google: an **Overview**,
then **Players**, **Matchmaking**, **Matches**, **Leaderboards** and
**Competitive** under Game, and **Monitoring**, **Database** and **Logs** under
System, each at its own address (e.g.
`/admin/database`), refreshed every 5 seconds. **Matches** lists the series
Recharged builds report ([reports.md](reports.md)), in progress first; a match
opens its own page (e.g. `/admin/matches/12`) with both players, each game as
both sides saw it, and the reports as they came. A profile on **Players** opens
its own page too (e.g. `/admin/players/1000000000`): its friend code and
console, its record in the reported matches by mode (series and games won,
lost and drawn, goals, captains), its standing on the leaderboards and its
matches. The search box of **Players** finds profiles by any part of their
Mii, Recharged player, profile number, friend code, console or address
("laso" finds RAULASO, "0868" a friend code), beyond the 200 the list shows;
the one of **Leaderboards** finds a Mii, player, profile or friend code on
every board, with its rank. **Monitoring** starts with the last 48 hours as a timeline that moves
with the clock: the players online (logged in to WFC, and of them with
Recharged open; the server counts them every minute and keeps 30 days), the
matches in progress, and below them what went wrong in each half hour, with a
table of the same numbers. Its by-day table shows each day's most players
online. Then it shows how online matches
go wrong over the last 14 days: games out of sync and lost connections by day, build and stadium, games
that left WFC in the middle of a match, the error codes players saw, and the
reports the server refused ([reports.md](reports.md#diagnostics)).
Only the Google accounts in `ADMIN_GOOGLE_EMAILS` or `OWNER_GOOGLE_EMAILS` get
in. Owners also see **Maintenance** (`/admin/maintenance`), operations that
cannot be undone: **Wipe all leaderboards** deletes every board's scores and the
final standings of past seasons. It asks twice: the button shows what would go,
then the wipe runs only once you type `wipe all leaderboards` within 5 minutes.
A game that is online keeps its totals until it logs in again and puts them
back after its next match, so wipe when no one is online. **Test sign-in**
switches sign-in with a name alone on or off at once
([profiles.md](profiles.md)); the switch stays across restarts and updates
and wins over `PROFILES_TEST_SIGN_IN`. Without the four `/admin`
variables, the server runs as before and `/admin` answers 404. With only some of
them, the container stops at start and its log names the missing ones.

Google signs in only on an HTTPS address (plain HTTP works only for
`localhost`), so `/admin` needs a public hostname. In Cloudflare Zero Trust,
open your tunnel → **Public Hostname** → **Add**: for example subdomain `wfc`,
Service Type `HTTP`, URL `<unraid-ip>:80` (the HTTP Host Port). Then sign in at
`https://wfc.example.com/admin`, never at `http://<unraid-ip>/admin`. The tunnel
carries only the dashboard; the game still needs the five ports above.

1. In [Google Cloud Console → Credentials](https://console.cloud.google.com/apis/credentials),
   **Create credentials → OAuth client ID**, type **Web application**. Under
   **Authorized redirect URIs** add exactly `<PUBLIC_URL>/admin/callback`, e.g.
   `https://wfc.example.com/admin/callback`. Adding the URI to an OAuth client
   you already have works too, but Google then shows that client's app name
   when you sign in.
2. On the **OAuth consent screen**, user type **External** in **Testing** is
   enough: add every admin as a **test user**. The scopes are only `openid`,
   `email` and `profile`.
3. Add the four variables `PUBLIC_URL`, `GOOGLE_CLIENT_ID`, `GOOGLE_CLIENT_SECRET`
   and `ADMIN_GOOGLE_EMAILS` (see [Environment variables](#environment-variables)),
   and `OWNER_GOOGLE_EMAILS` with your own address for **Maintenance**, and click
   **Apply**. The log shows `Admin dashboard at https://wfc.example.com/admin`.

Sessions last 12 hours and live in the server's memory: restarting or updating
the container signs everyone out. To remove an admin, edit `ADMIN_GOOGLE_EMAILS`
and **Apply**. Turn on 2-step verification for the admin Google accounts, because
they are the only key to `/admin`.

### Diagnosing a set

`GET /admin/api/sets/<id>` shows a competitive set (docs/profiles.md) as the
server sees it right now: the set's state and times, how the server keeps its
games for RANKED (each side's ready game, and a game another game of the same
player replaced), each player's last heartbeat, Ready to play! and game call,
their WFC profiles with their logins and master-server sessions (searching,
listed, last heartbeat), the reports, and the matchmaking events of those
profiles since the set began: searches and how many games they found, match
commands and whether they reached the other game, NAT negotiations and
whether the relay carried them. The server keeps the last 1000 events.

`GET /admin/api/leaderboards?top=1000` reads the whole RANKED boards (each
Recharged player's WFC profiles one row, with their `profile_ids`; `top`
defaults to 10), and `?q=<name>` every row of a player.

With `ADMIN_DIAGNOSTICS_TOKEN` set, a script reads these (and the other pages'
`/admin/api/…` data) without signing in:

```sh
curl -H "Authorization: Bearer $TOKEN" https://wfc.example.com/admin/api/sets/22
```

Every request with a bearer token goes to the log with its path. To take the
token away, change or remove the variable and **Apply**.

## Updates

Enable **CA Auto Update Applications** for this container to deploy new `stable`
images automatically. Every release is also tagged `v<version>` and
`sha-<full-commit>`; for a rollback, use an earlier one as the repository tag.

To try a release before it is merged, run a second container with the
repository `ghcr.io/previouslevelgames/recharged-wfc-srv:test`, which follows
the newest release pull request ([how releases work](development.md#releases)).
Give a test container its own **Path (SQL data)** and **Host Port**: the server
applies its database migrations on start, so a test build would upgrade the
release container's database.

With a [database container](#several-servers-on-one-database), give a test
server a database container of its own.

Make the [GHCR package public](https://github.com/orgs/previouslevelgames/packages/container/recharged-wfc-srv/settings)
for pulls without credentials, or run `docker login ghcr.io` on Unraid.

## Several servers on one database

PostgreSQL can run in a container of its own, which several server containers
share: they keep one set of players, profiles, scores and matches. A server
container with `POSTGRES_HOST` uses the database there and starts no
PostgreSQL of its own.

Each server keeps who is online in its own memory: players see each other
online, as friends and in matchmaking, only on the same server. A game's NAS
login and its GPCM connection must reach the same server too. `/admin` shows
the players, matchmaking and activity of the server you are signed in to, and
**Monitoring**'s count of players online takes one server's count per minute.
Until the servers share this, send all of the game's traffic to one of them.

### The network

The servers reach the database container by its name on a Docker network of
their own. Create it once in the Unraid terminal:

```bash
docker network create recharged-wfc
```

Unraid keeps networks made this way across restarts with **Settings → Docker →
Preserve user defined networks: Yes** (Advanced View, with Docker stopped), and
lists it under **Network** as `Custom: recharged-wfc`. A server inside another
container's network, such as a VPN container's, reaches the database when that
container is on `recharged-wfc`.

### The database container

In **Docker → Add Container** (Advanced View):

| Setting | Value |
|---|---|
| Name | `recharged-wfc-db` |
| Repository | `postgres:18-bookworm`. Keep `-bookworm`: the server image is built on it, so a database the bundled PostgreSQL made keeps its collations. |
| Network | `Custom: recharged-wfc` |
| Path (SQL data) | Config Type **Path**: Container Path `/var/lib/postgresql`, Host Path `/mnt/user/appdata/recharged-wfc-srv/postgres`, Access Mode **Read/Write**: the server's **Path (SQL data)** so far, which keeps its data. |
| Variables | `POSTGRES_PASSWORD` (**Password Mask: Yes**), `POSTGRES_USER` and `POSTGRES_DB`, as on the servers. They initialize an empty folder only. |
| Extra Parameters (Advanced) | `--stop-timeout=30` |
| Autostart | Enabled |

It needs no **Port**: only the servers reach it, over the network. Keep it
above the servers in the Docker tab, so it starts first; a server waits for the
database anyway and logs `Waiting for PostgreSQL` until it answers.

On a folder that holds a database, its log says `Skipping initialization`. If
it initializes a new database instead, stop it: **Host Path** does not point at
the old data.

### The server containers

Set them up as in [Unraid setup](#unraid-setup), with these differences:

- **Network** `Custom: recharged-wfc`.
- The variable `POSTGRES_HOST` = `recharged-wfc-db`, and the same
  `POSTGRES_PASSWORD`, `POSTGRES_USER` and `POSTGRES_DB` as the database.
- No **Path (SQL data)**. Without it, a server whose `POSTGRES_HOST` goes
  missing starts an empty database of its own instead of opening the shared
  folder a second time.
- Each server its own **Path (logs)**, e.g. `.../recharged-wfc-srv/logs-2`:
  two servers would write the same daily file.
- Each server its own **Host Ports**, or its own address: the game's ports are
  fixed.

### Switching an existing container

1. Stop `recharged-wfc-srv`, and copy its data folder while it is stopped,
   e.g. `cp -a /mnt/user/appdata/recharged-wfc-srv/postgres /mnt/user/appdata/recharged-wfc-srv/postgres.bak`.
2. Create the network and the database container as above, on the same
   **Host Path**, and start it. Check its log for `Skipping initialization`.
3. Edit `recharged-wfc-srv`: add `POSTGRES_HOST`, remove **Path (SQL data)**,
   and move it to the network. **Apply**.

To go back, stop the server and the database container, give the server its
**Path (SQL data)** again and remove `POSTGRES_HOST`.

### Updates

The first server to start on a new version updates the database, while the
others wait for it. Update every server to the same version together: a server
older than the database refuses to start (`migration … was previously applied
but is missing`). CA Auto Update on every server container does this.

## Docker Compose

```bash
cp .env.example .env
# Set POSTGRES_PASSWORD and POSTGRES_DATA_DIR in .env, and the /admin variables
# to enable the admin dashboard.
docker compose pull
docker compose up -d --no-build --wait
```

Compose runs PostgreSQL in a container of its own, `db`, and the server as
`server`. `POSTGRES_DATA_DIR` and `LOG_DATA_DIR` set the **host** folders for
the SQL data and the logs, and `HTTP_PORT` sets the host HTTP port (default 80,
which the game needs); GPCM, the master server, the server browser and NAT
negotiation use 29900, 27900/udp, 28910 and 27901/udp. In Unraid, use the
**Path** and **Port** mappings above. Repeat the last two commands to update.
To build locally instead, run `./scripts/build.sh` before
`docker compose up -d --no-build --wait`.

The Compose file before this one ran the database inside the server container.
Coming from it, run `docker compose down` once before the first `up`; the
database container then opens the same `POSTGRES_DATA_DIR`.

`COMPOSE_PROFILES=second-server` adds `server-2` on the same database, with its
own log folder `SERVER_2_LOG_DATA_DIR` and the host ports `SERVER_2_HTTP_PORT`
(8081), `SERVER_2_GPCM_PORT` (39900), `SERVER_2_MASTER_PORT` (37900/udp),
`SERVER_2_SERVER_BROWSER_PORT` (38910) and `SERVER_2_NATNEG_PORT` (37901/udp).
`SERVER_2_LABEL` labels its pages, and `SERVER_2_PUBLIC_URL` gives it its own
`PUBLIC_URL`. See [Several servers on one database](#several-servers-on-one-database)
for what the servers share.
