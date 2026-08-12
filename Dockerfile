# sharerr — multi-stage build producing a slim runtime image.
#
# No TLS system dependency: reqwest is built against rustls, and sqlx compiles
# SQLite from source, so the runtime layer needs nothing but CA certificates.
#
# Cross-compiled rather than emulated. `--platform=$BUILDPLATFORM` keeps the whole
# toolchain running natively on the build host and retargets only codegen and the
# linker. Under QEMU this build takes the better part of an hour: `lto = "thin"`
# with `codegen-units = 1`, plus the C in aws-lc-sys and libsqlite3-sys, is exactly
# the workload emulation is worst at.

# Pinned to the workspace's declared rust-version, so this build fails if the code
# ever reaches past the MSRV. A developer's local toolchain will not catch that.
#
# The two pins drift asymmetrically: raising `rust-version` in Cargo.toml without
# this line fails loudly here, but raising this line without Cargo.toml fails
# nowhere — the MSRV check silently starts testing a newer toolchain. Change both.
FROM --platform=$BUILDPLATFORM rust:1.88-bookworm AS builder

# Supplied by BuildKit. Names the architecture of the *runtime* image, not of this
# stage — which is the whole point of the split.
ARG TARGETARCH

WORKDIR /src

# Everything that varies by architecture, in one case statement — adding a third
# target means editing one branch rather than hunting for the others. An
# unrecognised TARGETARCH fails here, before any work, rather than silently
# producing an x86 binary.
#
# cmake and a C compiler are not optional: aws-lc-rs (rustls' default crypto
# provider) builds AWS-LC through cmake, and libsqlite3-sys compiles SQLite from
# source. The aarch64 cross toolchain is fetched only when it is the target.
#
# The resolved triple goes to a file because the build step below runs in its own
# layer, and a Dockerfile ENV cannot hold a shell-computed value.
RUN set -eu; \
    case "$TARGETARCH" in \
        amd64) TRIPLE=x86_64-unknown-linux-gnu; CROSS= ;; \
        arm64) TRIPLE=aarch64-unknown-linux-gnu; \
               CROSS="gcc-aarch64-linux-gnu g++-aarch64-linux-gnu libc6-dev-arm64-cross" ;; \
        *) echo "unsupported TARGETARCH: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    echo "$TRIPLE" > /triple; \
    apt-get update; \
    apt-get install --yes --no-install-recommends cmake $CROSS; \
    rm --recursive --force /var/lib/apt/lists/*; \
    rustup target add "$TRIPLE"

# Namespaced by triple, so they are simply inert when TARGETARCH=amd64.
#
# Both halves are needed. Cargo needs the linker for the final link; the cc and
# cmake build scripts need CC/CXX/AR for aws-lc-sys and libsqlite3-sys. Setting
# only the former fails deep inside a C build with a confusing "cannot execute
# binary file", because the build scripts quietly fall back to the host compiler.
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
    CXX_aarch64_unknown_linux_gnu=aarch64-linux-gnu-g++ \
    AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar

# The whole workspace is copied at once. Splitting manifests from sources to warm
# the dependency cache is the usual trick, but it duplicates the crate layout in
# the Dockerfile and silently rots when a crate is added. The cache mounts below
# are the answer to the same problem without that hazard.
COPY . .

# `--locked` so an image build can never silently resolve different dependency
# versions than the ones the test suite ran against.
#
# The registry and target directories are cache mounts, which is what makes a local
# rebuild after a source edit cost seconds rather than recompiling ~370 crates. Two
# details are load-bearing:
#
#   * `id=` is per-architecture. Cache mounts default to `sharing=locked`, so one
#     shared id would serialise the amd64 and arm64 legs that currently build
#     concurrently.
#   * The `install` runs *inside this same RUN*. Mount contents do not survive into
#     the layer, so the binary has to be copied out before the mount goes away.
#
# Note this does not help GitHub Actions: the `type=gha` cache backend does not
# persist mount contents, so CI still builds cold. It is for local builds — which
# is where `docker build .` gets run as the MSRV check.
RUN --mount=type=cache,target=/usr/local/cargo/registry,id=registry-$TARGETARCH \
    --mount=type=cache,target=/src/target,id=target-$TARGETARCH \
    TRIPLE="$(cat /triple)" \
 && cargo build --release --locked --package sharerr --target "$TRIPLE" \
 && install -D "target/$TRIPLE/release/sharerr" /out/sharerr

# No --platform override, deliberately: this stage must be pulled for the *target*
# architecture. That is what makes the arm64 image genuinely arm64.
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

COPY --from=builder /out/sharerr /usr/local/bin/sharerr

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
#
# Deliberately not `/ready` either, tempting as it looks. An instance whose vault
# is not populated yet answers 503 there *by design*, and it is repaired by an
# operator running `sharerr vault set` inside this container. Pointing the
# healthcheck at /ready would restart the container out from under them.
HEALTHCHECK --interval=60s --timeout=10s --start-period=15s --retries=3 \
    CMD curl --fail --silent --show-error http://localhost:8477/health || exit 1

ENTRYPOINT ["/usr/local/bin/sharerr"]
CMD ["serve"]
