# DATABASE_URL and the rest of .env reach every recipe.
set dotenv-load

# The development database, Postgres 18 in Docker on the address and with the
# name, user and password of DATABASE_URL: started when it isn't running, and
# made the first time.
db:
    #!/usr/bin/env bash
    set -euo pipefail
    name=recharged-wfc-dev-db
    if [ "$(docker inspect -f '{{{{.State.Running}}}}' "$name" 2>/dev/null)" = true ]; then exit 0; fi
    if docker inspect "$name" >/dev/null 2>&1; then
        docker start "$name" >/dev/null
    else
        # postgres://user:password@host:port/database
        rest=${DATABASE_URL#*://}
        credentials=${rest%%@*}; address=${rest#*@}
        user=${credentials%%:*}; password=${credentials#*:}
        hostport=${address%%/*}; database=${address#*/}; database=${database%%\?*}
        host=${hostport%%:*}; port=${hostport##*:}
        docker run -d --name "$name" --network host \
            -e POSTGRES_USER="$user" -e POSTGRES_PASSWORD="$password" -e POSTGRES_DB="$database" \
            postgres:18-bookworm -c listen_addresses="$host" -c port="$port" >/dev/null
    fi
    until docker exec "$name" sh -c 'pg_isready -q -h "$1" -p "$2"' sh \
        "$(docker inspect -f '{{{{index .Config.Cmd 1}}}}' "$name" | sed 's/listen_addresses=//')" \
        "$(docker inspect -f '{{{{index .Config.Cmd 3}}}}' "$name" | sed 's/port=//')"; do
        sleep 0.5
    done

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
