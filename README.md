# recharged-wfc-srv

Minimal Rust/Axum foundation for a reimplementation of
[WiiLink WFC](https://github.com/WiiLink24/wfc-server/). Currently implements only
`GET /status`; game handshakes, authentication and matchmaking are future work.
PostgreSQL is provisioned for future APIs; the application has no database driver,
schema or queries yet. Status reports application liveness, not database health.

## Run locally

Requires Rust 1.95 or newer:

```bash
cargo run --locked
```

In another terminal:

```bash
curl http://localhost:8080/status
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Response: `{"status":"ok","revision":"<full Git commit SHA>"}`.
The revision is compiled into the binary. Local builds use Git HEAD; builds before
the first commit or without Git metadata report `unknown`. Uncommitted edits are
based on the reported commit. `GIT_COMMIT` can override the revision **at build
time**; changing container environment variables cannot change it. `PORT` sets the
listening port (default `8080`, all interfaces). Routes live in `src/lib.rs`.

## Images and automatic updates

Push this project as the root of `yannicksuter/recharged-wfc-srv` on GitHub.
The **Publish server image** Action checks Rust, builds Linux/AMD64, and tests the
status endpoint before publishing to GHCR. Pushes to `master`/`main` and manual
runs publish `sha-<full-commit>`; the default branch also publishes `latest`.
Pull requests run the checks without publishing. The image name follows the
GitHub repository; adjust `SERVER_IMAGE` if using a different owner or repo name.

After the first successful run, make the GHCR package public in its package
settings for credential-free pulls. For a private package, authenticate on Unraid
with `docker login ghcr.io` using a GitHub token with `read:packages`.
[GHCR authentication](https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry).

As in `data-tracker`, install
[CA Auto Update Applications](https://github.com/Squidly271/ca.update.applications)
in Unraid, enable updates for the server container, and choose a schedule. It pulls
new `latest` images and recreates the container with its saved configuration.
To roll back, set Repository to `ghcr.io/yannicksuter/recharged-wfc-srv:sha-<full-commit>`
and apply. Disable automatic updates for the pinned container.

## Unraid container settings

Create a Docker network once with `docker network create recharged-wfc`, then add
both containers through **Docker → Add Container** with network `recharged-wfc`.
Enable Autostart for both.

| Server setting | Value |
|---|---|
| Name | `recharged-wfc-srv` |
| Repository | `ghcr.io/yannicksuter/recharged-wfc-srv:latest` |
| Network | `recharged-wfc` |
| TCP port | Host `8080` → container `8080` (choose another host port if occupied) |
| Variable `PORT` | `8080` (already the image default) |
| WebUI | `http://[IP]:[PORT:8080]/status` |
| Persistent paths | None; the server is stateless |

| PostgreSQL setting | Value |
|---|---|
| Name | `recharged-wfc-postgres` |
| Repository | `postgres:18-bookworm` |
| Network | `recharged-wfc` |
| Path, Read/Write | Host `/mnt/user/appdata/recharged-wfc-srv/postgres` → container `/var/lib/postgresql` |
| Variable `POSTGRES_DB` | `recharged_wfc` |
| Variable `POSTGRES_USER` | `recharged_wfc` |
| Variable `POSTGRES_PASSWORD` | Generate with `openssl rand -hex 32` |
| TCP port | Internal `5432`; no host mapping needed |

Future API code can connect to `recharged-wfc-postgres:5432` on this network
(`postgres:5432` with Compose), using the database credentials above. PostgreSQL
18 stores its cluster under `/var/lib/postgresql/18/docker`, inside the mapped
directory. These credentials initialize an empty data directory; changing the
environment later does not change existing database credentials. Keep the major
version at 18 until performing a database migration.
[PostgreSQL image documentation](https://hub.docker.com/_/postgres).

Check the deployed revision with `curl http://<unraid-ip>:8080/status`.

## Docker Compose alternative

Copy `docker-compose.yml` and `.env.example` to a directory on Unraid (or clone the
repository). Docker Compose must be installed.

```bash
cp .env.example .env
# Edit .env: set POSTGRES_PASSWORD and, on Unraid,
# POSTGRES_DATA_DIR=/mnt/user/appdata/recharged-wfc-srv/postgres
docker compose pull
docker compose up -d --no-build --wait
```

`HTTP_PORT` controls the host port; `POSTGRES_DATA_DIR` controls the persistent
database folder. Compose keeps PostgreSQL on its internal network. To update a
Compose-managed deployment, run these commands manually or from an Unraid User
Scripts schedule in the directory containing the Compose file:

```bash
docker compose pull server
docker compose up -d --no-build --no-deps --wait server
```

For a local image build, run `./scripts/build.sh`, then
`docker compose up -d --no-build --wait`. If you changed `SERVER_IMAGE` in `.env`,
also export it when running the build helper. The helper embeds the current Git commit;
a direct Docker build should pass `--build-arg GIT_COMMIT="$(git rev-parse HEAD)"`.
Database files survive container replacement in the configured host folder.
