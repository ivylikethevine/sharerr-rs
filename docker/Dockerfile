# sharerr and sharerr-lighthouse — one file, two runtime images.
#
# `docker/Dockerfile.lighthouse` used to be a near-identical second file: same
# builder base, same MSRV pin, same cross-compile machinery, same runtime split —
# differing only in which crate it built and the shape of its runtime layer. That
# duplication is why the MSRV pin below had two copies to keep in sync (see
# CLAUDE.md's history on this); merging removes the second copy rather than
# relying on a comment to keep both current.
#
# `--target runtime-sharerr` and `--target runtime-lighthouse` select the two
# published images. `runtime-sharerr` is also the last stage in the file, so a
# bare `docker build -f docker/Dockerfile .` — the MSRV check documented in
# docs/CONTRIBUTING.md and .github/pull_request_template.md, and what every
# compose `build:` block in this tree already runs with no `target:` — keeps
# building the sharerr image with no flag.
#
# No TLS system dependency: reqwest is built against rustls, and sqlx compiles
# SQLite from source, so the sharerr runtime layer needs nothing but CA
# certificates. sharerr-lighthouse depends on neither aws-lc-rs (its
# ed25519-dalek dependency, curve25519-dalek, is pure Rust) nor sqlx, so it
# has nothing to compile from source at all beyond the cross linker.
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
#
# Every stage that compiles project code (`toolchain` and everything built from
# it) descends from this pin — `cargo chef` is a driver that shells out to the
# ambient `cargo`, it carries no toolchain of its own, so cooking dependencies
# below proves nothing about the MSRV that building them directly wouldn't.
FROM --platform=$BUILDPLATFORM rust:1.98-bookworm@sha256:82150a52ec202c1b14d7817e14516c392bb7f5cfebd88f1ed531cb37ebd39922 AS rust-base
WORKDIR /src

# --- cargo-chef: dependency caching, computed once for both architectures ---
#
# `rust-base` is `--platform=$BUILDPLATFORM`, so every stage below that does not
# read $TARGETARCH is architecture-invariant and BuildKit builds it once for a
# `linux/amd64,linux/arm64` build, not once per arch. `chef` and `planner` both
# qualify — installing cargo-chef and computing the recipe do not depend on the
# target architecture, only the toolchain stage below them does.
FROM rust-base AS chef
# Installed from crates.io rather than the `lukemathwalker/cargo-chef` base image:
# that image would replace this Dockerfile's own MSRV pin as the build's Rust
# toolchain, and would sit outside every digest-pinning and dependabot rule this
# tree applies to `rust:*`. `--locked` so a cargo-chef point release can never
# silently resolve different transitive deps than the ones this pin was tested
# against. Bumping the version is a hand edit here; see
# .github/scripts/check_tool_versions.sh, which tracks it the same way it tracks
# zizmor and actionlint.
ARG CARGO_CHEF_VERSION=0.1.78
RUN cargo install cargo-chef --locked --version "$CARGO_CHEF_VERSION"

FROM chef AS planner
# The whole workspace. `cargo chef prepare` derives its recipe from `cargo
# metadata`, not from anything hand-listed here, so it picks up a crate added to
# `crates/*` automatically and never rots the way a hand-split
# manifests-then-sources COPY would.
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# --- the cross-compile toolchain, one leg per $TARGETARCH ---
#
# Everything that varies by architecture, in one case statement — adding a third
# target means editing one branch rather than hunting for the others. An
# unrecognised TARGETARCH fails here, before any work, rather than silently
# producing an x86 binary.
#
# cmake and the cross toolchain are installed unconditionally, even though only
# sharerr needs cmake (aws-lc-rs builds AWS-LC through it; sqlx compiles SQLite
# from source) and only an arm64 build needs the cross packages. This stage is
# shared by both `builder-sharerr` and `builder-lighthouse` below, and is never
# shipped — the shared apt layer being slightly larger than the lighthouse build
# strictly needs is a smaller cost than a second conditional branch to save it.
#
# The resolved triple goes to a file because the build steps below run in their
# own layers, and a Dockerfile ENV cannot hold a shell-computed value.
FROM chef AS toolchain
ARG TARGETARCH
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

# --- sharerr ---
#
# `cook` and the `cargo build` immediately below it must be invoked with
# byte-identical package/profile/target flags, or cargo treats them as two
# different builds and the cook layer buys nothing while still looking cached.
# `--locked` on both, `--target "$TRIPLE"` on both, `--package sharerr` on both.
#
# No `--mount=type=cache` here, deliberately — a cache mount's contents do not
# survive into the image layer (the same reason `type=gha` never helped this
# build: see the note below on `cache-to`), and cargo-chef's entire mechanism is
# that `cook` bakes the compiled dependency tree into the layer so BuildKit can
# cache and export it. A cache mount at the same path would shadow that
# directory for the RUN's duration, cargo would see it empty, and the cook step
# would recompile every dependency from scratch while still reporting a cache
# hit on the *step* — correct output, zero speedup, nothing fails loudly.
FROM toolchain AS builder-sharerr
COPY --from=planner /src/recipe.json recipe.json
RUN TRIPLE="$(cat /triple)" \
 && cargo chef cook --release --locked --target "$TRIPLE" --package sharerr --recipe-path recipe.json
COPY . .
RUN TRIPLE="$(cat /triple)" \
 && cargo build --release --locked --package sharerr --target "$TRIPLE" \
 && install -D "target/$TRIPLE/release/sharerr" /out/sharerr

# --- sharerr-lighthouse ---
#
# A second `cook` against the same recipe, scoped to a different package: `cargo
# chef cook -p sharerr-lighthouse` only builds that package's own dependency
# subtree (sharerr-lighthouse depends on none of sqlx, reqwest/rustls, aws-lc or
# argon2 — see its own Cargo.toml), not the ~400-crate tree the sharerr image
# needs. Cooking the whole workspace once and filtering per package here is what
# keeps this image's build proportional to what it actually ships, rather than
# regressing to compiling everything sharerr needs to build sharerr-lighthouse.
FROM toolchain AS builder-lighthouse
COPY --from=planner /src/recipe.json recipe.json
RUN TRIPLE="$(cat /triple)" \
 && cargo chef cook --release --locked --target "$TRIPLE" --package sharerr-lighthouse --recipe-path recipe.json
COPY . .
RUN TRIPLE="$(cat /triple)" \
 && cargo build --release --locked --package sharerr-lighthouse --target "$TRIPLE" \
 && install -D "target/$TRIPLE/release/sharerr-lighthouse" /out/sharerr-lighthouse

# No --platform override, deliberately: this stage must be pulled for the *target*
# architecture. That is what makes the arm64 image genuinely arm64.
FROM debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171 AS runtime-base

RUN apt-get update \
 && apt-get install --yes --no-install-recommends ca-certificates curl \
 && rm --recursive --force /var/lib/apt/lists/*

# --- the lighthouse image ---
FROM runtime-base AS runtime-lighthouse

# Runs unprivileged. /data holds only the decoy secret — see
# `sharerr_lighthouse::main`'s `load_or_create_secret` — which is worth
# protecting from a casual read on the host but is not the credential vault
# the sharerr image carries; losing it just reshuffles decoys after a restart.
RUN groupadd --gid 1000 lighthouse \
 && useradd --uid 1000 --gid 1000 --create-home --home-dir /home/lighthouse lighthouse \
 && mkdir --parents /data \
 && chown lighthouse:lighthouse /data

COPY --from=builder-lighthouse /out/sharerr-lighthouse /usr/local/bin/sharerr-lighthouse

USER lighthouse
WORKDIR /home/lighthouse

VOLUME ["/data"]
EXPOSE 7878

ENV LIGHTHOUSE_BIND=0.0.0.0:7878 \
    LIGHTHOUSE_SECRET_FILE=/data/lighthouse.secret

# `/lighthouse/v1/health` rather than a bare `/health`: see
# `sharerr_lighthouse::routes`'s comment on why every route here lives under
# the versioned prefix.
HEALTHCHECK --interval=60s --timeout=10s --start-period=15s --retries=3 \
    CMD curl --fail --silent --show-error http://localhost:7878/lighthouse/v1/health || exit 1

ENTRYPOINT ["/usr/local/bin/sharerr-lighthouse"]

# --- the sharerr image ---
#
# Last stage in the file, deliberately: it is what `docker build -f
# docker/Dockerfile .` resolves to with no `--target`, which is the form every
# compose `build:` block in this tree and the MSRV check both already use.
FROM runtime-base AS runtime-sharerr

# Runs unprivileged. sharerr only ever needs to *read* the media library, so the
# library should be mounted read-only wherever the deployment allows it.
RUN groupadd --gid 1000 sharerr \
 && useradd --uid 1000 --gid 1000 --create-home --home-dir /home/sharerr sharerr \
 && mkdir --parents /config /data \
 && chown sharerr:sharerr /config /data

COPY --from=builder-sharerr /out/sharerr /usr/local/bin/sharerr

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
