FROM rust:1.95-bookworm AS build
WORKDIR /app

COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
COPY migrations ./migrations
# Query descriptions from `just prepare`: the macros check against them, as
# there is no database during the build.
COPY .sqlx ./.sqlx
ENV SQLX_OFFLINE=true

ARG GIT_COMMIT=unknown
RUN GIT_COMMIT="$GIT_COMMIT" cargo build --locked --release

FROM postgres:18-bookworm AS runtime
# ca-certificates: /admin's Google sign-in checks Google's TLS against them.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl tini \
    && rm -rf /var/lib/apt/lists/*

COPY --from=build /app/target/release/recharged-wfc-srv /usr/local/bin/recharged-wfc-srv
COPY --chmod=755 docker/entrypoint.sh /usr/local/bin/start-server

ARG GIT_COMMIT=unknown
LABEL org.opencontainers.image.title="recharged-wfc-srv" \
    org.opencontainers.image.revision="$GIT_COMMIT"

ENV PORT=8080 \
    GPCM_PORT=29900 \
    MASTER_PORT=27900 \
    SERVER_BROWSER_PORT=28910 \
    POSTGRES_DB=recharged_wfc \
    POSTGRES_USER=recharged_wfc \
    POSTGRES_INITDB_ARGS="--auth-host=scram-sha-256"
# PostgreSQL's base image provides PGDATA and the /var/lib/postgresql volume.
# HTTP (NAS, gamestats2, /admin), GPCM, the QR2 master server and the server
# browser.
EXPOSE 8080 29900 27900/udp 28910
STOPSIGNAL SIGTERM
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
    CMD pg_isready -q -h 127.0.0.1 -p 5432 -U "$POSTGRES_USER" -d "$POSTGRES_DB" \
        && curl --fail --silent --show-error --max-time 3 "http://127.0.0.1:${PORT}/status" || exit 1
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/start-server"]
CMD []
