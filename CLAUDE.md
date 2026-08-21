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

**No tier-1 fixture opens a real vault.** `state::fixtures::unconfigured()` has no
master key, so `ServeState::open_vault`/`tracker_token`/`gossip_identity` all
resolve to "unavailable" there — fine for testing the not-yet-configured path, a
real gap for anything that only behaves differently once a vault-backed secret
*is* set (e.g. a magnet's announce token once `tracker.token` exists). Standing
one up would mean setting `SHARERR_MASTER_KEY` on the real process env, which
nothing in this suite does because a parallel test runner does not scope env
vars per test. Prefer testing the store-backed logic directly (pass `Store` and
the resolved secret as plain parameters, the way `tracker::authenticate_token`
does) over reaching for a live vault.

## Traps

**Torrent name vs release title are two different strings.** Conflating them
stalls seeding at 0%. The torrent's name must describe the file where it already
sits; the release title is what the feed advertises. See `sharerr-torrent`.

**Never move, rename, or re-link media.** This is the project's central constraint,
not a preference. `skip_checking = true` is the default, so qBittorrent seeds a
newly-added torrent immediately rather than re-verifying it — sharerr never wrote
anything qBittorrent's own hash check would need to catch. Set it `false` while
still confirming a path mapping is correct: with checking on, a wrong mapping
seeds mismatched data instead of qBittorrent refusing it.

**Secrets never go in `sharerr.toml`.** They live in the encrypted vault, keyed by
the constants in `sharerr_core::config::secret_keys`. Adding a secret means adding
it to `secret_keys::ALL` (or `commands/vault.rs`'s `vault set`/`vault list` silently
reject it) *and* wiring the constant into the relevant handler in `web/settings.rs`
(which does not consult `ALL` at all — each settings section names its secret
explicitly), or the web UI silently will not manage it.

**The config file is rewritten in place by the web UI**, comments and all, via
`toml_edit`. A settings path is a hand-typed string in more than one place; check
`web/settings.rs` and `web/templates/settings.html` agree.

**Every `web/settings.rs` form field must be `#[serde(default)]`**, even ones the
handler goes on to treat as required. An `<input>` can render `disabled` — no
master key yet, or its config path pinned by a `SHARERR_*` env var
(`lock_attr`/`locks` in `settings.html`) — and a disabled input submits nothing at
all. A `Form` field with no default then fails to deserialize *before* the handler
runs, so `reject()`'s own styled error page never renders; the caller gets a bare
`Failed to deserialize form body: missing field` instead, and whatever they typed
in every other field on that form is discarded. Apply it once, at the struct
level (`#[serde(default)]` above the struct, `#[derive(Default)]` on it) rather
than per field — see any struct in `web/settings.rs`'s Forms section — so a field
added later inherits the tolerance instead of needing to remember it.

## Repository

Publishing to GHCR happens on a `v*` tag only. A push to `main` builds both
architectures — that build is load-bearing as the MSRV check — but ships nothing.

The roadmap is `docs/ROADMAP.md`; the original design brief and the two premises
the implementation disproved are in `docs/design.md`.
