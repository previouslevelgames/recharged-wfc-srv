# DATABASE_URL and the rest of .env reach every recipe.
set dotenv-load

# Apply the pending migrations to the development database.
migrate: db
    sqlx migrate run

# Run the server outside Docker against the development database. It applies
# the migrations itself on start.
run: db
    RUST_LOG="${RUST_LOG:-info,raw=trace}" cargo run --locked

# Refresh .sqlx/ after changing a query or the schema: CI and the Docker build
# check the query macros against it, without a database.
prepare: db
    cargo sqlx prepare -- --all-targets

# What CI runs.
check:
    cargo fmt --check
    cargo clippy --locked --all-targets -- -D warnings
    cargo test --locked
