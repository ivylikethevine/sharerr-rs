# Working on sharerr

Conventions that are not derivable from the code, and traps that have each cost
real debugging time.

## The verification loop

```bash
cargo test --workspace && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo build
```

Run it before declaring anything done.

**Clippy must stay at zero warnings.** The workspace sets `unwrap_used` and
`expect_used` to `warn` because the vault and the service clients handle secrets;
CI promotes them with `-D warnings`. When a test needs to panic on failure, opt out
with an inner attribute at the top of the module:

```rust
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
```

Never weaken the workspace lint to make a test compile. A non-test `expect()` that
genuinely cannot fail takes a targeted `#[allow]` **plus a comment saying why** —
`Config::default`'s `Url::parse` on a literal is the one example in the tree.

**`cargo test` does not build `target/debug/sharerr`.** It builds the test harness.
Any CLI smoke test needs an explicit `cargo build` first; running a stale binary has
already produced one confidently wrong conclusion here.

## MSRV

`rust-version` is 1.88, and the Dockerfile pins the same toolchain, which makes
`docker build .` the **de-facto MSRV check** — a local toolchain is invariably
newer and will not catch a breach. Two have shipped unnoticed this way
(`str::split_at_checked` in a const context, then let-chains). Build the image
before claiming the declared MSRV holds.

## Dependencies

Versions live in `[workspace.dependencies]` in the root `Cargo.toml`, one pin for
the whole tree. Crates write `foo = { workspace = true }` and add only their own
`features`, which cargo unions. Do not pin a version in a crate manifest.

## Testing tiers

**Tier 1** is the default `cargo test` and is hermetic: no network, no containers,
no database. Service clients run against wiremock on loopback, sqlx against
`sqlite::memory:`, migrations are embedded by `sqlx::migrate!`. Keep it that way —
it is what makes CI fast and reliable.

**Tier 2** is `./run_docker_tests.sh`, opt-in and local only, behind the `e2e`
feature and `#[ignore]`. It drives a real Sonarr + Radarr + qBittorrent stack. CI
compiles it with `--all-features` so it cannot rot, but never runs it — do not add
`--include-ignored`.

The assertion that justifies tier 2 existing: after a real sync through a real
qBittorrent, every media file has the same inode, mtime, and length it started
with. Mocks cannot prove that.

All fixtures are synthetic — invented titles, seeded pseudo-random bytes, so
torrent info hashes are stable across machines. No real content, ever.

## Traps

**Torrent name vs release title are two different strings.** Conflating them
stalls seeding at 0%. The torrent's name must describe the file where it already
sits; the release title is what the feed advertises. See `sharerr-torrent`.

**Never move, rename, or re-link media.** This is the project's central constraint,
not a preference. `skip_checking = false` is the default so qBittorrent verifies
the existing file and seeds it in place.

**Secrets never go in `sharerr.toml`.** They live in the encrypted vault, keyed by
the constants in `sharerr_core::config::secret_keys`. Adding a secret means adding
it to `secret_keys::ALL` (or `commands/vault.rs`'s `vault set`/`vault list` silently
reject it) *and* wiring the constant into the relevant handler in `web/settings.rs`
(which does not consult `ALL` at all — each settings section names its secret
explicitly), or the web UI silently will not manage it.

**The config file is rewritten in place by the web UI**, comments and all, via
`toml_edit`. A settings path is a hand-typed string in more than one place; check
`web/settings.rs` and `web/templates/settings.html` agree.

## Repository

Publishing to GHCR happens on a `v*` tag only. A push to `main` builds both
architectures — that build is load-bearing as the MSRV check — but ships nothing.

The roadmap is `docs/roadmap.md`; the original design brief and the two premises
the implementation disproved are in `docs/design.md`.
