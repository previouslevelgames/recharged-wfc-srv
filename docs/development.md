# Development

Rust 1.95+: `cargo run --locked`. Logs go to stderr, filtered by `RUST_LOG`
(default `info`): `RUST_LOG=info,tower_http=debug` adds one line per HTTP request,
`RUST_LOG=info,raw=trace` adds raw protocol dumps. Check with `cargo fmt --check`,
`cargo clippy --locked --all-targets -- -D warnings`, and `cargo test --locked`
(`just check` runs all three). The standalone Rust process serves status without
starting PostgreSQL; the image entrypoint manages the database. The container
supplies `PGHOST`, `PGPORT`, `PGUSER`, `PGPASSWORD`, and `PGDATABASE` to the
Rust process. Routes live in `src/lib.rs`.

The `justfile` runs the server against a development database from `.env`:
`just run` applies the migrations and starts it, and `just prepare` refreshes
`.sqlx/` after a query or the schema changes, because CI and the Docker build
check the query macros without a database.

The Git revision is embedded at build time, automatically by the image workflow
and build helper. Native builds use Git HEAD, or `unknown` without Git metadata.

For `/admin` locally, register `http://127.0.0.1:8080/admin/callback` as another
redirect URI of the OAuth client, then run with `PUBLIC_URL=http://127.0.0.1:8080`
and the other three variables, and open `http://127.0.0.1:8080/admin` (not
`localhost`, so the address matches). The routes, sign-in and queries live in
`src/admin/`; the page, its script and its styles in `src/admin/assets/`, built
into the binary. The tests sign in through a fake Google, so they need no
credentials.

## Releases

Releases follow the same process as Recharged:

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

Removing test images needs this repository's **Admin** role under the GHCR
package's **Manage Actions access**.
