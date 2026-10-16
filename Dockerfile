FROM rust:1.95-bookworm AS build
WORKDIR /app

COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src

ARG GIT_COMMIT=unknown
RUN GIT_COMMIT="$GIT_COMMIT" cargo build --locked --release

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=build /app/target/release/recharged-wfc-srv /usr/local/bin/recharged-wfc-srv

ARG GIT_COMMIT=unknown
LABEL org.opencontainers.image.title="recharged-wfc-srv" \
    org.opencontainers.image.revision="$GIT_COMMIT"

USER 10001:10001
ENV PORT=8080
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD curl --fail --silent --show-error --max-time 3 "http://127.0.0.1:${PORT}/status" || exit 1
ENTRYPOINT ["/usr/local/bin/recharged-wfc-srv"]
