# sharerr — multi-stage build producing a slim runtime image.
#
# No TLS system dependency: reqwest is built against rustls, and sqlx compiles
# SQLite from source, so the runtime layer needs nothing but CA certificates.

# Pinned to the workspace's declared rust-version, so this build fails if the code
# ever reaches past the MSRV. A developer's local toolchain will not catch that.
FROM rust:1.88-bookworm AS builder

WORKDIR /src

# The whole workspace is copied at once. Splitting manifests from sources to warm
# the dependency cache is the usual trick, but it duplicates the crate layout in
# the Dockerfile and silently rots when a crate is added — which has more cost
# than a cold build here.
COPY . .

# `--locked` so an image build can never silently resolve different dependency
# versions than the ones the test suite ran against.
RUN cargo build --release --locked --package sharerr

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
 && apt-get install --yes --no-install-recommends ca-certificates curl \
 && rm --recursive --force /var/lib/apt/lists/*

# Runs unprivileged. sharerr only ever needs to *read* the media library, so the
# library should be mounted read-only wherever the deployment allows it.
RUN groupadd --gid 1000 sharerr \
 && useradd --uid 1000 --gid 1000 --create-home --home-dir /home/sharerr sharerr \
 && mkdir --parents /config /data \
 && chown sharerr:sharerr /config /data

COPY --from=builder /src/target/release/sharerr /usr/local/bin/sharerr

USER sharerr
WORKDIR /home/sharerr

# /config holds sharerr.toml; /data holds the vault, the database, and generated
# .torrent files. Both must persist across restarts — losing /data means losing
# the credential vault.
VOLUME ["/config", "/data"]
EXPOSE 8477

ENV SHARERR_CONFIG=/config/sharerr.toml

# Deliberately *not* `sharerr doctor`: doctor fails when Sonarr or qBittorrent is
# down, which says nothing about whether this container is healthy. `/health`
# answers the question the orchestrator is actually asking.
HEALTHCHECK --interval=60s --timeout=10s --start-period=15s --retries=3 \
    CMD curl --fail --silent --show-error http://localhost:8477/health || exit 1

ENTRYPOINT ["/usr/local/bin/sharerr"]
CMD ["serve"]
