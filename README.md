# recharged-wfc-srv

Rust backend for a reimplementation of [WiiLink WFC](https://github.com/WiiLink24/wfc-server/).
One container runs the API and PostgreSQL 18. Currently, `GET /status` returns
`{"status":"ok","revision":"<Git commit SHA>"}`. Game APIs are future work.

## Unraid setup

In **Docker → Add Container**, enable **Advanced View** and use these settings:

| Setting | Value |
|---|---|
| Name | `recharged-wfc-srv` |
| Repository | `ghcr.io/previouslevelgames/recharged-wfc-srv:latest` |
| Network | `bridge` |
| Port | Config Type **Port**: Container Port `8080`, Host Port `8080` (any free port), TCP |
| Path (SQL data) | Config Type **Path**: Container Path `/var/lib/postgresql`, Host Path `/mnt/user/appdata/recharged-wfc-srv/postgres`, Access Mode **Read/Write** |
| WebUI | `http://[IP]:[PORT:8080]/status` |
| Extra Parameters (Advanced) | `--stop-timeout=30` |
| Autostart | Enabled |

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
| `PORT` | no | API port inside the container. Default: `8080`; use the Host Port mapping to choose the external port. |

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
the API. Open `http://<unraid-ip>:8080/status` (using your Host Port) to check the
running revision. The container health check checks both services.

## Updates

Enable **CA Auto Update Applications** for this container to deploy new `latest`
images automatically. GitHub Actions builds and tests each push to `master`/`main`;
the default branch publishes `latest`. For rollback, use `sha-<full-commit>`.
Make the [GHCR package public](https://github.com/orgs/previouslevelgames/packages/container/recharged-wfc-srv/settings)
for pulls without credentials, or run `docker login ghcr.io` on Unraid.

## Docker Compose

```bash
cp .env.example .env
# Set POSTGRES_PASSWORD and POSTGRES_DATA_DIR in .env.
docker compose pull
docker compose up -d --no-build --wait
```

For Compose, `POSTGRES_DATA_DIR` sets the **host** folder and `HTTP_PORT` sets the
host HTTP port. In Unraid, use the **Path** and **Port** mappings above.
Repeat the last two commands to update. To build locally instead, run
`./scripts/build.sh` before `docker compose up -d --no-build --wait`.

## Development

Rust 1.95+: `cargo run --locked`. Check with `cargo fmt --check`,
`cargo clippy --locked --all-targets -- -D warnings`, and `cargo test --locked`.
The standalone Rust process serves status without starting PostgreSQL; the image
entrypoint manages the database. Database APIs and a Rust driver are still to be added.
The container supplies `PGHOST`, `PGPORT`, `PGUSER`, `PGPASSWORD`, and `PGDATABASE`
to the Rust process for future connections. Routes live in `src/lib.rs`.

The Git revision is embedded at build time, automatically by the image workflow
and build helper. Native builds use Git HEAD, or `unknown` without Git metadata.
