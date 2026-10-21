# recharged-wfc-srv

Rust backend for a reimplementation of [WiiLink WFC](https://github.com/WiiLink24/wfc-server/).
One container runs the server and PostgreSQL 18. It serves the game's
registration and login (NAS and GPCM, [docs/login.md](docs/login.md)) and the
gamestats2 leaderboards ([docs/gamestats2.md](docs/gamestats2.md)). `GET /status`
returns `{"status":"ok","revision":"<Git commit SHA>"}`, and
[`/admin`](#admin-dashboard) shows the server and its database to admins who
sign in with Google.

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
| Path (SQL data) | Config Type **Path**: Container Path `/var/lib/postgresql`, Host Path `/mnt/user/appdata/recharged-wfc-srv/postgres`, Access Mode **Read/Write** |
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
| `PGDATA` | no | SQL data directory inside the container. Default: `/var/lib/postgresql/18/docker`, beneath the mapped `/var/lib/postgresql` path. Leave unset to use this default. |
| `PORT` | no | HTTP port inside the container (NAS, gamestats2, `/status`, `/admin`). Default: `8080`; the Host Port mapping makes it 80. |
| `GPCM_PORT` | no | GPCM port inside the container. Default: `29900`. |
| `MASTER_PORT` | no | UDP port of the master server inside the container. Default: `27900`. |
| `PUBLIC_URL` | for `/admin` | The HTTPS address browsers use for this server, without a path, e.g. `https://wfc.example.com`. See [Admin dashboard](#admin-dashboard). |
| `GOOGLE_CLIENT_ID` | for `/admin` | Client ID of the Google OAuth client. |
| `GOOGLE_CLIENT_SECRET` | for `/admin` | Client secret of the Google OAuth client. Set **Password Mask: Yes**. |
| `ADMIN_GOOGLE_EMAILS` | for `/admin` | Google addresses allowed in, comma-separated, e.g. `you@gmail.com, teammate@gmail.com`. |

With the mapping above, the **SQL data lives on your Unraid server at**
`/mnt/user/appdata/recharged-wfc-srv/postgres/18/docker`.
To store it elsewhere on the server, choose another **Host Path** for
**Path (SQL data)**. Keep this mapping when updating or replacing the container
so the database persists. `PGDATA` refers to a path inside the container;
keep it at its default for this setup.

`POSTGRES_DB`, `POSTGRES_USER`, and `POSTGRES_PASSWORD` initialize an empty data
folder; changing them later does not update an existing database or its credentials.

If replacing the earlier two-container setup, stop its PostgreSQL container before
reusing the same host folder.

Click **Apply**. The container starts PostgreSQL on internal `127.0.0.1:5432`, then
the server. Open `http://<unraid-ip>/status` to check the running revision. The
container health check checks both.

## Connecting the game

The port (Recharged's `online` branch) sends every Nintendo WFC name to one
IPv4 address. Set it in the game's `recharged.ini`:

```ini
[online]
server = 192.168.1.42
```

That address must reach the container on TCP 80, TCP 29900 and UDP 27900;
for players outside your network, forward those three ports on the router. The
traffic is plain HTTP and GameSpy's own protocols, so it cannot go through a
Cloudflare Tunnel. On its first login the game registers the console and keeps
the userid in `data/shared2/DWC_AUTHDATA`.

The game logs in and stays online; matchmaking, friends and the server
browser are not served yet.

## Admin dashboard

`/admin` is organised by what a WFC server manages, and refreshes every 5
seconds:

- **Overview**: every section in a few figures, with its status.
- **Game**: **Leaderboards** (the gamestats2 boards by category and region, and
  the top 10 of each with Mii names, wins and losses). **Players**,
  **Matchmaking** and **Moderation** are planned: their pages say which
  services they need.
- **System**: **Database**, the health of PostgreSQL, its uptime, size,
  connections, cache hit ratio and transactions per second, the open
  connections and their queries, the tables, and the applied migrations.

Each section has its own address, e.g. `/admin/database`. Only the Google accounts in
`ADMIN_GOOGLE_EMAILS` get in. Without the four `/admin` variables, the server
runs as before and `/admin` answers 404. With only some of them, the container
stops at start and its log names the missing ones.

Google signs in only on an HTTPS address (plain HTTP works only for
`localhost`), so `/admin` needs a public hostname, like offbyone.me. In Cloudflare
Zero Trust, open your tunnel → **Public Hostname** → **Add**: for example subdomain
`wfc`, Service Type `HTTP`, URL `<unraid-ip>:80` (the HTTP Host Port). Then sign
in at `https://wfc.example.com/admin`, never at `http://<unraid-ip>/admin`.

1. In [Google Cloud Console → Credentials](https://console.cloud.google.com/apis/credentials),
   **Create credentials → OAuth client ID**, type **Web application**. Under
   **Authorized redirect URIs** add exactly `<PUBLIC_URL>/admin/callback`, e.g.
   `https://wfc.example.com/admin/callback`. Adding the URI to offbyone.me's
   client works too, but Google then shows that client's app name when you sign in.
2. On the **OAuth consent screen**, user type **External** in **Testing** is
   enough: add every admin as a **test user**. The scopes are only `openid`,
   `email` and `profile`.
3. Add the four variables `PUBLIC_URL`, `GOOGLE_CLIENT_ID`, `GOOGLE_CLIENT_SECRET`
   and `ADMIN_GOOGLE_EMAILS` (see [Environment variables](#environment-variables))
   and click **Apply**. The log shows `Admin dashboard at https://wfc.example.com/admin`.

Sessions last 12 hours and live in the server's memory: restarting or updating
the container signs everyone out. To remove an admin, edit `ADMIN_GOOGLE_EMAILS`
and **Apply**. Turn on 2-step verification for the admin Google accounts, because
they are the only key to `/admin`.

## Updates

Enable **CA Auto Update Applications** for this container to deploy new `stable`
images automatically. Releases follow the same process as Recharged:

1. Develop on `main`. Every push and pull request into `main` checks Rust and
   verifies the container; nothing is published.
2. A release is a pull request from `main` into `stable`, the default branch. It
   also publishes the test image `test-pr<N>` (and `test-pr<N>-<commit>` for
   each commit) to try on a server before merging; the images are removed when
   the pull request closes. `test` always points to the newest test image, so a
   test container with the repository
   `ghcr.io/previouslevelgames/recharged-wfc-srv:test` follows every release
   pull request. Only the repository owner's merge team may merge into `stable`.
3. Merging publishes the image `sha-<full-commit>` and drafts the GitHub release
   `v<version>` with the version in `Cargo.toml`. Raise it before the release
   pull request: edit `version`, then run `cargo update --workspace` for
   `Cargo.lock`. A version that is already released or drafted is not drafted
   again.
4. Publishing the draft tags its image `v<version>`, `stable` and `latest`, and
   the auto-update deploys it. For rollback, use an earlier `v<version>` or
   `sha-<full-commit>`.

Give a test container its own **Path (SQL data)** and **Host Port**: the server
applies its database migrations on start, so a test build would upgrade the
release container's database.

Make the [GHCR package public](https://github.com/orgs/previouslevelgames/packages/container/recharged-wfc-srv/settings)
for pulls without credentials, or run `docker login ghcr.io` on Unraid. Removing
test images needs this repository's **Admin** role under the package's
**Manage Actions access**.

## Docker Compose

```bash
cp .env.example .env
# Set POSTGRES_PASSWORD and POSTGRES_DATA_DIR in .env, and the /admin variables
# to enable the admin dashboard.
docker compose pull
docker compose up -d --no-build --wait
```

For Compose, `POSTGRES_DATA_DIR` sets the **host** folder and `HTTP_PORT` sets the
host HTTP port (default 80, which the game needs); GPCM and the master server
use 29900 and 27900/udp. In Unraid, use the **Path** and **Port** mappings above.
Repeat the last two commands to update. To build locally instead, run
`./scripts/build.sh` before `docker compose up -d --no-build --wait`.

## Development

Rust 1.95+: `cargo run --locked`. Logs go to stderr, filtered by `RUST_LOG`
(default `info`): `RUST_LOG=info,tower_http=debug` adds one line per HTTP request,
`RUST_LOG=info,raw=trace` adds raw protocol dumps. Check with `cargo fmt --check`,
`cargo clippy --locked --all-targets -- -D warnings`, and `cargo test --locked`.
The standalone Rust process serves status without starting PostgreSQL; the image
entrypoint manages the database. Database APIs and a Rust driver are still to be added.
The container supplies `PGHOST`, `PGPORT`, `PGUSER`, `PGPASSWORD`, and `PGDATABASE`
to the Rust process for future connections. Routes live in `src/lib.rs`.

The Git revision is embedded at build time, automatically by the image workflow
and build helper. Native builds use Git HEAD, or `unknown` without Git metadata.

For `/admin` locally, register `http://127.0.0.1:8080/admin/callback` as another
redirect URI of the OAuth client, then run with `PUBLIC_URL=http://127.0.0.1:8080`
and the other three variables, and open `http://127.0.0.1:8080/admin` (not
`localhost`, so the address matches). The routes, sign-in and queries live in
`src/admin/`; the page, its script and its styles in `src/admin/assets/`, built
into the binary. The tests sign in through a fake Google, so they need no
credentials.
