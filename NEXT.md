# Simplify pass over `crates/` — smallest crate to largest

## Context

`/simplify` normally reviews a diff, but the working tree is clean and the
request is explicitly the **whole `crates/` folder**, walked smallest crate to
largest. This is a quality pass — reuse, simplification, efficiency, altitude —
not a bug hunt. Correctness belongs to `/code-review`.

Agreed with the user: **judgment calls are in scope**, and fixes go in
**one crate at a time, verification loop after each**.

Walk order (Rust LOC): `sharerr-client` 575 · `sharerr-probe` 784 ·
`sharerr-transmission` 995 · `sharerr-lighthouse` 1396 · `sharerr-rtorrent` 1467 ·
`sharerr-testkit` 1796 · `sharerr-qbit` 2003 · `sharerr-arr` 2671 ·
`sharerr-torrent` 2985 · `sharerr-core` 3107 · `sharerr-store` 4147 ·
`sharerr` 39124.

Three surveys covered crates 1–11; I read the `sharerr` crate myself. Findings
below are deduped across all four passes, and the ones I verified personally are
marked **[verified]**.

**Status:**
- ✅ X1–X5 (cross-crate batch) — done, verified, MSRV-checked.
- ✅ Crates 1–2 (`sharerr-client`, `sharerr-probe`): P1, P2 — done, verified.
- ✅ Crate 3 (`sharerr-transmission`): T1, T2, T3, T4 — done, verified.
- ✅ Crate 4 (`sharerr-lighthouse`): L1, L2 — done, verified. (L1 needed
  `#[allow(clippy::unused_async, reason = "...")]` on `report`/`lookup`,
  since making the lock synchronous removed their only `.await` but ~20
  call sites across two crates still call them as async — changing the
  public signature was judged a wider ripple than this refactor calls for.)
- ✅ Crate 5 (`sharerr-rtorrent`): R1, R3, R4, R5 — done, verified.
  **R2 (concurrent tracker inserts) was deliberately skipped** — the
  module's own doc comment states insertion order at group 0 decides the
  new trackers' relative tier priority, and no test guards that order, so
  making the calls concurrent risks a real behavior change with nothing to
  catch it. Left sequential.
  R4 (the file split) landed as `client.rs` (construction + raw
  `call`/`call_str`/`call_multi`), `adapter.rs` (the `TorrentClient` impl +
  `as_str`/`as_bool`/`as_u64` decode helpers), `xmlrpc.rs` (the wire codec:
  `Param`, `XmlValue`, `request_xml`, `parse_response`, `take`, etc.) —
  adapted from qbit's `lib/client/adapter/torrents/models/error` naming
  since rtorrent has no per-endpoint wire structs or local error type to
  justify `models.rs`/`error.rs`. The 43-test module split the same way:
  18 low-level parser/encoder tests that call xmlrpc.rs's private
  functions directly moved to `xmlrpc.rs`'s own `mod tests`; the other 25
  (everything going through `RtorrentClient`/wiremock) stayed together in
  `adapter.rs`'s `mod tests`. Test count verified unchanged (43 before, 43
  after) — nothing was dropped in the split.
- ✅ Crate 6 (`sharerr-testkit`): K1 — done, verified. `TvLibrary`/
  `MovieLibrary`/`MusicLibrary` collapsed into one `Library { root, files }`;
  `tv_library`/`movie_library`/`music_library` (unchanged names/call sites)
  now share a `write_library(root, files, base_seed)` primitive. This also
  fixed the latent gap the finding named: `movie_library` previously seeded
  every file at a flat `2000` instead of `2000 + index`, harmless only
  because it has ever had exactly one file. `TvLibrary` had one external
  consumer (`sharerr/src/sync/tests.rs`), updated to `Library`.
  `gen-fixtures.rs` is now a loop over the three builders. Smoke-tested the
  binary directly (writes 5 files, second run's `TempDir` cleanup proves
  idempotent content).
- ✅ Crate 7 (`sharerr-qbit`): Q1, Q2, Q3 — done, verified.
  Q1: `BuildRequest`'s `&dyn Fn(...) + Send + Sync` indirection replaced with
  `impl FnOnce(RequestBuilder) -> RequestBuilder + Send` taken by value
  across `dispatch`/`send`/`send_checked`/`send_ok`/`send_json`; the dead
  `cookies` reqwest feature (no `auth/login`, key-only auth) dropped from
  Cargo.toml; the stale "rebuilt per attempt" comment removed.
  Q2: `Category` deleted, `categories()` now returns `HashSet<String>`
  (decodes into `HashMap<String, serde::de::IgnoredAny>` and takes the
  keys); `doctor.rs`'s only consumer updated to `.contains(label)`.
  `API_KEY_LEN`/`API_KEY_PREFIX` demoted to private — verified zero
  external callers before removing their `pub use`.
  Q3: **diffed all 17 in-crate tests against the 8 in `tests/adapter.rs`
  pair by pair** (the survey's "9 duplicates" claim did not hold up — real
  count was 5). Deleted the 5 true duplicates (each already a strict subset
  of an existing `tests/adapter.rs` assertion), moved the 12
  genuinely-distinct ones over, deleted the in-crate `mod tests` entirely.
  Verified by test count: 48 before (4 client + 17 adapter + 8 integration
  + 19 webui) → 43 after (4 + 20 + 19) — exactly 5 fewer, nothing else lost.
- ✅ Crate 8 (`sharerr-arr`): A1 — done, verified, with a scope correction.
  On close reading, sonarr's join is genuinely different from lidarr's and
  readarr's — it looks episode numbering up by `file.id` (picking the
  lowest-numbered of possibly several episodes pointing at one file), not a
  single-container-by-id lookup — so forcing all three into one generic
  helper would have fought the actual shapes. Added `join_by_parent` to
  `lib.rs` beside `fetch_tagged` for **lidarr and readarr only** (verified
  byte-for-byte identical shape between those two); sonarr's
  `numbering_by_file` stays untouched and undisturbed. All 46 existing
  tests pass unchanged, confirming behavior preserved.
- ✅ Crate 9 (`sharerr-torrent`): B1, B2, B3 — done, verified, MSRV-checked.
  B1: added `Retargeted` enum + `retarget_announce(data, announce)` to
  `factory.rs` — one bencode parse instead of two (or three, for `adopt`'s
  cache-hit-stale path). Careful re-derivation of `adopt`'s exact
  invariants was needed: cache-hit-and-current short-circuits with zero
  verification (a prior `adopt` already proved it), but cache-miss ALWAYS
  verifies+writes even when the exported torrent's announce happens to
  already be correct — `Retargeted::Current` still carries `info_hash` so
  that path doesn't lose the identity check. Added 2 new direct unit tests
  for `retarget_announce` since it's new production logic with real
  branching. `read_announce`/`read_info_hash`/`rewrite_announce` kept as-is
  — `tracker.rs:660` calls `rewrite_announce` unconditionally with nothing
  to compare, so single-parsing would add complexity for no win there.
  B2: `parse_query` returns `HashMap<&str, Cow<[u8]>>`; `percent_decode`
  borrows when the input has no `%`/`+` (every field except `info_hash`/
  `peer_id`). Required fixing one real borrow-checker trap at the
  `tracker.rs` call site — `parse_query(query.unwrap_or_default().as_bytes())`
  borrowed from a temporary that would be dropped at the end of the `let`
  statement; split into two statements.
  B3: extracted `title::humanize`/`title::join_title` (made `pub`) from
  what `parse()`/`join_title` already did internally; `sharerr::library`'s
  own `humanize`/`display_title` now delegate instead of re-deriving the
  same dot/underscore substitution and join-trim tail. Confirmed
  `RELEASE_TOKENS` and `MediaMeta::scene_video_codec`'s codec list are
  genuinely different vocabularies (source/resolution tokens vs. codec
  names only) before asserting so in a comment — left un-merged.
- 🔶 Crate 10 (`sharerr-core`), in progress. **N1 and N2 — the two riskiest,
  largest items in the whole remaining plan — are done and fully verified.**
  N1: added `crate::str_enum!` (in a new `macros.rs`, `#[macro_export]`) to
  `sharerr-core`, generating `ALL`/`as_str`/`parse` for all nine enums
  across both `sharerr-core` and `sharerr-store`. Three invocation forms:
  plain (strict `Option<Self>`), a `, "reason"` form for a strict `parse`
  whose decode-failure consequence is worth a doc paragraph (used once, on
  `MediaSource`), and a `, lenient = Default, "reason"` form for the two
  widen-instead-of-fail decoders (`PeerScope`, `ObservedVia`) — the reason
  string is mandatory there and gets spliced onto the generated `parse`'s
  doc via `#[doc = concat!(...)]`, so the enum-specific safety reasoning
  ("widening is the safer failure because...") survives instead of being
  deleted with the boilerplate. Enums with extra hand-written methods
  (`TorrentBackend::display_name`, `MediaSource::api_version`/`ARRS`/
  `KIND_SCOPED`/`has_coarse_tagging`) keep a second, ordinary `impl` block
  alongside the macro-generated one — multiple `impl` blocks per type is
  fine in Rust, so nothing needed inventing a way to inject hand-written
  methods into generated code.
  N2: added `config_paths!` and `secret_keys!` (same file), using
  `$(#[$doc:meta])* $name:ident = $value:literal;` capture so a `///`
  comment written directly above an entry in the macro invocation attaches
  to the generated `pub const` exactly as it would without the macro — full
  per-constant doc fidelity preserved across all ~38 config paths and ~18
  secret keys. `secret_keys!` takes two labeled blocks, `editable { }` and
  `generated { }`; only `editable` feeds `ALL`, so "deliberately not
  editable" is now a marker at the declaration site (which block) instead
  of an absence a reader has to notice in a separately-maintained list —
  exactly what the finding asked for. `url_for`/`api_key_for`/`env_var`/
  `validate_value`/`peer_gossip_key` stay hand-written, placed after their
  macro invocation.
  Verified via `every_writable_path_resolves_to_a_real_config_field`,
  `writable_paths_are_unique`, all 73 `web::settings` tests, all 21
  `web::config_io` tests (the env-override/`toml_edit` machinery
  `CLAUDE.md` specifically warns is easy to desync), all 4 `web::wizard`
  tests, plus the `share_state_names_round_trip`/`media_source_names_round_trip`/
  `every_observed_via_round_trips_and_unknown_values_default_to_gossip`/
  `every_scope_round_trips_through_its_stored_value` round-trip tests for
  N1. Full workspace loop green after each.
- ✅ Crate 10 (`sharerr-core`) — **all findings resolved.** Fully verified,
  full workspace loop green after each.
  N3 (gluetun slot dedup) — **partially done, partially deferred.** On
  investigation, the handler-level duplication the finding described was
  already fixed: `web/settings.rs`'s `save_gluetun`/`save_gluetun_client`
  already funnel through one `save_gluetun_section`, and `sharerr::gluetun`
  already has a `GluetunTarget` enum with `.config(&Config)`/
  `.api_key_secret()` accessors — exactly the abstraction N3 asked for,
  just living in the `sharerr` binary crate (correctly — it's an
  orchestration concept, not a config-schema one) rather than
  `sharerr-core`. The one real gap was `GluetunTarget` not also covering
  the three `config_paths` constants; added `GluetunTarget::config_paths()`
  and switched `save_gluetun_section` to take a `GluetunTarget` instead of
  three loose path constants. **Deliberately left alone:** `settings.html`'s
  twelve parallel Askama template fields (six per gluetun section). Fixing
  that means restructuring the page's Rust-side view-model struct AND the
  template markup (an Askama macro parameterized by section), which is a
  materially bigger, riskier change for a purely cosmetic/organizational
  win — the functional duplication this finding was actually worried about
  was already gone. If picked up later: chrome-devtools MCP tools are
  available in this environment for visually verifying the rendered page,
  which this deferral avoided needing.
  N4 (`torrent_client_for`) — **skipped.** On inspection only 2 of the 10
  `TorrentClientConfig` fields (`upload_limit_kib`/`ratio_limit`) are
  genuinely backend-agnostic; the other 8 correctly vary per arm. Every
  extraction shape tried (a helper struct, placeholder-then-mutate since
  the type isn't `Default`) cost more in indirection than the ~4 duplicated
  lines it would remove. Left the three full struct literals as they are.
  N5: added `ARR_WIRING: &[(MediaSource, &str, &str)]` at the top of
  `config.rs`; `secret_keys::api_key_for` and `config_paths::url_for` both
  index it now instead of two independently hand-maintained five-arm
  matches. `Config::service` deliberately keeps its own match (it returns a
  `&ServiceConfig` field reference, which a `(source, &str, &str)` row
  cannot express) — added `service_resolves_every_source_arr_wiring_lists`
  as the test that would now catch a table/match divergence.
  N6: `web/items.rs`'s `KINDS` and a second, previously-unnoticed copy in
  `commands/preview.rs` both deleted; both call sites now use
  `sharerr_core::MediaSpec::KIND_TAGS` directly.
- ✅ **Crate 11, `sharerr-store` (S1–S6) — done.**
  S1: added `confirm_seeding(source, file_id, announce_token_fp, ratio,
  ratio_limit)`, one UPDATE where the sync fast path previously issued two;
  collapsed the two call sites. Kept `set_ratio` standalone — still needed
  for the non-confirmed branches. Added
  `confirm_seeding_touches_the_fingerprint_and_ratio_together`.
  S2: added `peer_endpoints_for(peer_ids) -> HashMap<i64, Vec<PeerEndpoint>>`
  and `peer_gossip_records(peer_ids) -> HashMap<i64, String>`, both one
  `WHERE peer_id IN (…)` query via a shared `row_to_endpoint` decoder.
  `web/peers.rs`, `web/topology.rs`, and `gossip.rs` all replaced their
  per-friend `join_all`/sequential-loop calls with one bulk call each.
  S3: six single-row updaters (`set_state`, `set_seeding`,
  `confirm_seeding`, `set_ratio`, `set_info_hash`, `reset_for_rebuild`) now
  share a private `update_item(query, source, file_id)` that binds the
  trailing `updated_at, source, file_id` and executes. Took two failed
  designs first — a `sql: String` built inside the helper with a
  caller-supplied bind closure doesn't work because the local `String`'s
  lifetime can't be named by the caller, and an HRTB `for<'q> FnOnce(...)`
  closure fails because it demands the closure work at `'static` even
  though it captures a borrowed `Option<&str>`. Landed on taking an
  already-built `SqliteQuery<'q>` *value* instead of a closure — ordinary
  generic lifetime, no HRTB. All six converted; all pre-existing
  column-isolation tests pass unchanged.
  S4: added `counts_by_state() -> Vec<(ShareState, i64)>` via one
  `GROUP BY`, zero-filled in declaration order — same contract
  `metrics::items_by_state` already promised. `metrics::gather` no longer
  calls `all_items()` to decode the whole library just to tally states, and
  its four store calls (`counts_by_state`, `seeding_summary`, `recent_runs`,
  `list_peers`) now run concurrently via `tokio::join!` instead of
  sequentially. Added `counts_by_state_zero_fills_every_declared_state`.
  S5: deleted `count_seeding` and `item_by_info_hash` — zero callers
  workspace-wide, tests included.
  S6: added `scope_filter(scope) -> &'static ScopeFilter` backed by
  `LazyLock<[ScopeFilter; 5]>`, built once instead of per Torznab feed
  request; `seeding_items`/`seeding_summary` call it instead of
  `ScopeFilter::new(scope)`.
- ✅ **Crate 12, `sharerr` itself (W1–W3) — done. Last crate in the walk.**
  W1: `vault_in` was byte-identical (modulo `unwrap`/`expect`) in
  `gossip.rs`, `lighthouse_client.rs`, `sync/tests.rs`, and
  `commands/doctor.rs`. Landed as one `pub(crate) fn vault_in` in a new
  `#[cfg(test)] mod test_support;` (crate-local, not a testkit export — a
  testkit export would drag `sharerr-store`, `secrecy`, `tempfile` in as
  non-dev dependencies for a helper only this crate uses). All four sites
  now `use crate::test_support::vault_in;`; `sync/tests.rs`'s explanatory
  doc comment moved onto the shared function rather than being lost.
  W2: `rotate_/clear_/finalize_tracker_token` in `web/settings.rs` shared a
  five-line tail (`open_vault` → the matching `_in` fn → `tracing::info!` →
  `invalidate` → `legacy_token_status().reset()`). Added
  `mutate_tracker_token(state, log_msg, invalidate_reason, f)`; all three
  callers now pass their `_in` function (or a closure, for `rotate_`, which
  needs to close over `new_value`) through it. The `_in` split stays, since
  tests still drive those directly.
  W3: `web/topology.rs`'s `layout` took eight positional arguments under
  `#[allow(clippy::too_many_arguments)]` and returned a bare
  `(Vec<Node>, Vec<Edge>, i32, i32)`. Added `LayoutInput<'a>` (named fields)
  and `Layout` (named `nodes`/`edges`/`width`/`height`); the `#[allow]` is
  gone. Updated the one production call site (`web/topology.rs`'s page
  handler) and the one previously-unnoticed second call site in
  `commands/preview.rs` (missed by the original survey — its mock topology
  page builds a `Layout` the same way), plus all 7 test call sites.

**All 12 crates in the walk are done, and so are D1 and D2.** X1–X5
cross-crate work, every per-crate finding taken up (P1–P2, T1–T4, L1–L2,
R1–R5 with R2/R4 judgment calls recorded, K1, Q1–Q3, A1, B1–B3, N1–N6 with
N4 skipped, S1–S6, W1–W3), and both deferred decisions (D1: gossip's and
lighthouse's record types are one type now; D2: lighthouse's `now_epoch`
matches core's) — see each crate's section, and the "Two decisions" section
below, for what was skipped and why. **Nothing from this plan remains open.**

None of this is committed — it's all uncommitted working-tree changes,
verified after each crate (and after D1/D2) with the full loop below. Every
"done" crate above has an empty `cargo fmt --all --check` diff and zero
clippy warnings as of the point it was marked done. The full workspace loop
(`cargo test --workspace`, `cargo clippy --workspace --all-targets
--all-features -- -D warnings`, `cargo build`, `cargo fmt --all --check`)
was re-run clean after D1/D2 landed, on top of every crate before them —
nothing broke across the whole pass. The MSRV gate (`docker build .` for the
main image, `docker build -f Dockerfile.lighthouse .` for the lighthouse
image, since D1/D2 touched `sharerr-lighthouse` directly) has also been run
and passes clean under the pinned 1.98 toolchain.
`docs/openapi.json` was regenerated as part of D1 and is committed-clean —
`openapi::tests::the_committed_document_is_current` passes.

---

## Cross-crate work — do this first

Four backends (`transmission`, `rtorrent`, `qbit`, `arr`) each hand-roll the
same plumbing. These land in `sharerr-client` and then unlock the per-crate
cleanups, so they come before the walk.

### X1 — the HTTP status ladder, written four times · REUSE / ALTITUDE

`transmission/src/lib.rs:126-169` and `rtorrent/src/lib.rs:137-161` are
line-for-line the same transport→auth→status→body ladder.
`qbit/src/client.rs:189-215` and `arr/src/client.rs:132-154` are the same
ladder with a clamped body added.

`sharerr-client` already owns every piece — `clamp_body:39`, `error_chain:48`,
`is_auth_rejection:82`, `unreachable:115` — but not the ladder composing them.

**Fix:** add `check_status(kind, status, what) -> Result<()>` and
`malformed(kind, what, err)` beside them. Transmission and rtorrent adopt both
wholesale (**[verified]** both already use `sharerr_client::ClientError` —
`transmission:34`, `rtorrent:86`).

> **Scope limit — qbit and arr keep their own error types.** A survey proposed
> routing all four through one ladder. They must not be: `QbitError::ApiKeyRejected`
> (`qbit/src/error.rs`) carries a long qBittorrent-specific diagnostic about key
> rotation and Host-header port mismatch that `ClientError::AuthRejected` would
> discard. Those two keep reusing the *individual* helpers, which is the right
> amount of sharing for them.

### X2 — password-redacting `Debug`, written four times · REUSE

`transmission:61-73`, `rtorrent:105-117`, `qbit/src/client.rs:67-77`,
`arr/src/client.rs:51-62` — **[verified]** verbatim identical in the first two,
comment included. The three-line justification comment (`finish_non_exhaustive`
rather than `finish`…) is copy-pasted four times, so a security-relevant
rationale is stated four times and can drift.

**Fix:** `sharerr_client::debug_redacted(f, "RtorrentClient", &[("endpoint", …)])`.
All four impls' fields are `&str` or the literal `"<redacted>"`, so one helper
covers them and the comment lives once.

### X3 — `shutdown_signal` duplicated verbatim · REUSE

`lighthouse/src/main.rs:52` and `sharerr/src/commands/serve.rs:190`.
**[verified] byte-identical**, including the `#[cfg(unix)]` split and both
`std::future::pending` fallbacks — the fiddliest part — differing only in the
final log string.

**Fix:** `pub async fn shutdown_signal()` in `lighthouse/src/lib.rs`, called by
both. `sharerr` already depends on `sharerr-lighthouse` (**[verified]**
`crates/sharerr/Cargo.toml:40`). `preview.rs:80` follows.

### X4 — every crate hand-builds its `reqwest::Client` · REUSE

`arr/src/client.rs:71-74` and `qbit/src/client.rs:101-104` both bypass
`sharerr_client::http_client_with_timeout` (`sharerr-client/src/lib.rs:104`),
whose own doc says it exists so "a client cannot forget the timeout". Both
bypass for the same reason: keeping the typed `reqwest::Error` source.

**Fix:** change the helper to return `Result<reqwest::Client, reqwest::Error>`
and let each crate map into its own error. Then all four go through it.

### X5 — test helpers every backend re-hand-rolls · REUSE

`Url::parse(&server.uri()).unwrap()` where `sharerr_testkit::mock::base_url`
exists: `rtorrent:787`, `transmission:560,618`, `qbit/src/adapter.rs:142`.
`qbit/src/adapter.rs:140` also redeclares the literal value of
`mock::QBIT_API_KEY`, again inline at `client.rs:275,292`.
`lighthouse/tests/binary.rs:30` duplicates `testkit::net::closed_port`.

**Fix:** call the helpers — testkit is already a dev-dep everywhere except
lighthouse. Additionally lift `body_text(&Request)` and
`requests_to(&server, suffix)` out of `qbit/tests/webui.rs:22-34` into
`testkit::mock`; rtorrent hand-decodes request bodies six times
(`rtorrent:974,997,1027,1056,1097,1290`) for want of them.

---

## Per-crate findings

### 1–2. `sharerr-client`, `sharerr-probe`

`sharerr-client` is clean as a contract crate — X1/X2/X4 *add* to it.

- **P1** `probe/src/lib.rs:141,205` — identical "open file, debug-log, return
  `None`" blocks → `fn open_logged(path) -> Option<File>`.
- **P2** `probe/src/lib.rs:215` — recomputes the extension the dispatch at `:57`
  already lowercased, so `.FLAC` hints as `FLAC` while dispatch used `flac`.
  Pass the lowercased value in.

### 3. `sharerr-transmission`

- **T1** `:203-278`, `:380`, `:512` — three `{ torrents: Vec<T> }` wrappers plus
  two copy-pasted "first torrent" chains → one generic `Torrents<T>` +
  `first_torrent<T>` over the existing `decode:281`.
- **T2** `:487`, `:523` — `set_trackers`/`add_trackers` each spell the `\n\n`
  tier join and its own `torrent-set` → extract `write_tracker_list`.
- **T3** `:363` — `category.unwrap_or_default().to_owned()` allocates per
  torrent from a loop-invariant value → hoist above the loop at `:330`.
- **T4** `:784,901,945` — the 409-handshake mock mounted verbatim 3×, bypassing
  `mount_handshake_then:569` → split into `mount_handshake` + `…_then`.

### 4. `sharerr-lighthouse`

- **L1** `:174-178` — one logical state guarded by two primitives: an async
  `RwLock<HashMap>` plus `last_sweep_at: AtomicI64` whose own doc says it is
  "only touched under the `records` write lock", so the atomic ordering is
  ceremony. No `.await` is held under either lock. → `std::sync::RwLock<Records>`.
  `transmission:56-58` already documents this exact reasoning for its plain
  `Mutex` — in-tree precedent.
- **L2** `:302,323` — `report` hashes and probes the same key three times
  (`get`, `contains_key`, `insert`) → restructure around `entry()`.

### 5. `sharerr-rtorrent`

- **R1** `:528-539`, `:724-743` — `escape_into` and a 5-arm XML entity table
  reimplement `quick_xml::escape::{escape, resolve_xml_entity}`, and quick-xml
  is already a direct dependency. 28 hand-written lines plus their test surface.
- **R2** `:421-427` — `set_trackers` awaits one full HTTP round trip **per URL,
  sequentially**, across the whole seeded set after every VPN reconnect → one
  `system.multicall` (the crate already parses arrays of arrays via `call_multi`),
  or at minimum `buffered(N)` as `arr/src/lib.rs:81` already does.
- **R3** `:964-1130` — eight tests mount an identical scalar-response stub →
  local `mount_scalar` helper (pairs with X5).
- **R4** — the crate is one 1467-line `lib.rs` (778 production + 689 test).
  `sharerr-qbit`, the same kind of crate, is split `lib/client/adapter/torrents/models/error`.
  Adopt that layout — it is already the house pattern. *(Judgment call, in scope
  per the user. `sharerr-transmission` at 552 production lines stays as one file.)*
- **R5** `:1316` — `take7_rejects_a_row_of_the_wrong_length` guards an arity no
  call site uses (`list()` destructures 8, `files()` 2) → retarget to `take::<8>`
  or drop it; `take2_…:1311` already covers the error path.

### 6. `sharerr-testkit`

- **K1** `src/library.rs:106-122,206-243` — `TvLibrary`/`MovieLibrary`/`MusicLibrary`
  are three structurally identical `{ root, files }` structs, and their three
  builders are one five-line body differing only in seed base (1000/2000/3000)
  and whether the index is added — `movie_library` silently omits it.
  `src/bin/gen-fixtures.rs:23-45` then repeats the same write-and-report block
  three more times. → one `Library` + `write_library(root, files, base_seed)`;
  `gen-fixtures` becomes a loop.

### 7. `sharerr-qbit`

- **Q1** `src/client.rs:45-47` — `BuildRequest = &dyn Fn(…)` exists "because a
  retry needs a fresh Form", but **[verified] nothing in the crate retries**;
  `client.rs:187` explicitly documents that it never does, and
  `torrents.rs:79`'s "rebuilt per attempt" comment describes a path that no
  longer exists. The trait-object indirection is threaded through five
  signatures and ten call sites. → take `impl FnOnce(…)` by value or drop the
  closure entirely. The `cookies` reqwest feature is dead for the same reason.
- **Q2** `src/models.rs:66-72` — `Category`'s only field is already the map key,
  and `lib.rs:34` never re-exports the type, so no external caller can name it;
  the sole consumer (`doctor.rs:669`) only calls `.contains_key`. → return
  `HashSet<String>`, delete `Category`, demote `API_KEY_LEN`/`API_KEY_PREFIX`
  to private.
- **Q3** `src/adapter.rs:130-512` — a 382-line in-crate `mod tests` against a
  183-line `tests/adapter.rs`. → consolidate.
  > **Verify before deleting.** The survey claimed "9 of 15 are near-verbatim
  > duplicates". **[verified] only one name actually collides**
  > (`a_rejected_key_translates_to_auth_rejected`). The rest are differently
  > named and must be diffed pair-by-pair before anything is removed — deleting
  > tests on a survey's say-so is how coverage quietly disappears.

### 8. `sharerr-arr`

- **A1** `src/lidarr.rs:101-155`, `src/readarr.rs:98-146` — the second half of
  every discovery walk (skip-if-no-files → index containers by id → join each
  file → warn-and-skip orphans) is copy-pasted with only type names changed;
  `sonarr.rs:49-99` is the same shape. A fifth *arr means writing it a fourth
  time. → the crate already owns the *first* half as `lib.rs:55 fetch_tagged`;
  add its counterpart `join_by_parent(...)` beside it.

### 9. `sharerr-torrent`

- **B1** `src/factory.rs:172-209` — `read_announce`, `read_info_hash`, and
  `rewrite_announce` each fully bencode-parse the `.torrent`, and all three call
  sites do read→compare→rewrite. `Seeder::refresh_announce` (`sync/seed.rs:379,390`)
  runs per seeding item per pass and parses the whole file — every piece hash,
  hundreds of KB — once to compare one URL, then again to rewrite it. `adopt`
  parses three times for one file. → `retarget_announce(data, announce) -> Retargeted`
  from a single `read_from_bytes`; keep the narrow readers for read-only uses.
- **B2** `src/announce.rs:502-521` — `parse_query` allocates an owned `String`
  key and `Vec<u8>` value for every parameter on every announce (~14), when only
  `info_hash`/`peer_id` need decoding. The rest of the module is carefully
  allocation-conscious. → `HashMap<&str, Cow<[u8]>>`.
- **B3** `src/title.rs:207,271` — `sharerr/src/library.rs:355-386` re-derives
  this module's normalisation internals (`humanize` is `parse`'s first line
  verbatim). → export `title::humanize` / `title::strip_release_cruft`. Ranked
  last: the token *sets* differ legitimately, so this is a move, not a merge.

### 10. `sharerr-core`

- **N1** `config.rs:642,777,1035,1067`, `model.rs:24,484`, plus `store/peers.rs:48`
  and `store/endpoints.rs:34,70` — **nine** enums hand-repeat the identical
  `ALL` / `as_str` / `parse = ALL.iter().find(…)` triple, ~200 lines. Every one
  carries its own copy of the "derived from `as_str` so the two cannot drift"
  comment — the tell that the *mechanism*, not the comment, should be shared. →
  one `str_enum!` macro in `sharerr-core`, with a `parse_or(default)` form for
  the two lenient decoders. Variant-specific extras stay hand-written.
- **N2** `config.rs:164-306` — `config_paths` writes each of 39 dotted paths
  twice (a `pub const` and an `ALL` entry), with drift explicitly unguarded:
  the comment at `:263` concedes "a path missing from here is simply unverified".
  Since `ALL` is what the settings UI enumerates, an omission is a field the UI
  silently will not manage — the exact trap `CLAUDE.md` documents. →
  a `config_paths! { … }` macro emitting constants *and* `ALL` from one list;
  for `secret_keys`, an `ALL`/`GENERATED` split so "deliberately not editable"
  is a marker rather than an absence reconstructed from prose.
- **N3** `config.rs:224-234,345-351,885-923` — the second gluetun poller is a
  special case layered on infrastructure already generalized for the first:
  `GluetunConfig` is correctly one type used twice, but six `config_paths`
  constants, two `secret_keys`, two near-identical handlers
  (`web/settings.rs:667-703`) and twelve parallel template fields are
  duplicated per slot. This crate already owns the right pattern one section
  over — `Config::torrent_client_for:484`. → `enum GluetunSlot` +
  `Config::gluetun_for(slot)` + `config_paths::gluetun(slot)`.
- **N4** `config.rs:484-535` — `torrent_client_for`'s three arms restate all ten
  fields each; `upload_limit_kib`/`ratio_limit` are copied verbatim into all
  three with a comment at `:513` saying they are backend-agnostic. → match on
  the seven varying fields, attach the two seeding fields outside the match.
- **N5** `config.rs:25-35,179-189,421-432` — three parallel six-arm matches over
  `MediaSource`, each promising "a sixth app should mean editing one function".
  It means three, and nothing fails to compile if only two are made. → one
  `ARR_WIRING` table that `api_key_for` and `url_for` both index.
- **N6** `model.rs:400` — `MediaSpec::KIND_TAGS` exists to stop kind tags being
  retyped, and **[verified]** `web/items.rs:74` retypes them anyway, with a doc
  comment admitting it is a copy. `store/peers.rs:381` uses the real one.
  `KIND_TAGS` is round-trip-tested (`model.rs:885`); the copy is not, so a
  `#[serde(rename)]` leaves the filter dropdown offering a dead tag. One-line fix.

### 11. `sharerr-store`

- **S1** `db.rs:390-433` — the sync fast path writes the same row twice per item
  per pass (`set_announce_token_fp` then `set_ratio` as separate UPDATEs). For a
  2,000-item library that is 4,000 WAL commits per sync, contending with feed
  reads. `set_seeding`'s own doc (`:343-349`) gives exactly this reasoning for
  having merged two UPDATEs — the fast path, which runs for *every* item on
  *every* pass, never got the same treatment. → `confirm_seeding(...)`, one
  UPDATE; collapse `sync/mod.rs:562-598`'s two awaits into it.
- **S2** `endpoints.rs:167` — no bulk accessor, so `web/peers.rs:306` and
  `web/topology.rs:745` each `join_all` one query per friend on every page load,
  with a comment at `peers.rs:294` apologising for it; `gossip.rs:456` is worse,
  awaiting sequentially inside a `for`. → `peer_endpoints_for(ids)` with one
  `WHERE peer_id IN (…)`, reusing the placeholder expansion `ScopeFilter`
  already does; add `peer_gossip_records(pubkeys)` alongside.
- **S3** `db.rs:322-478` — six near-identical single-row updaters sharing one
  `UPDATE shared_items SET …, updated_at = ? WHERE source = ? AND file_id = ?`
  skeleton; each re-types the WHERE and the `now_epoch()` bind, so changing the
  natural key is six edits. → a private `update_item(source, file_id, set_clause, bind)`.
  The binds are heterogeneous, so the closure/macro form is what type-checks.
- **S4** `db.rs:131` — `/metrics` decodes the entire library to produce four
  integers (`metrics.rs:96` calls `all_items()`, then scans the Vec four times).
  `seeding_summary`/`count_seeding` exist because "the status page asks on every
  load"; the third aggregate consumer never got one. → `counts_by_state()` over
  `GROUP BY`. Secondary: `metrics::gather` awaits four store calls sequentially
  where `tokio::join!` would overlap, as `web/peers.rs:304` already does.
- **S5** `db.rs:203,229` — **[verified] `count_seeding` and `item_by_info_hash`
  have zero callers workspace-wide**, tests included. Both carry doc comments
  naming a consumer that uses something else. → delete.
- **S6** `db.rs:552-607` — `ScopeFilter` rebuilds its SQL fragment and bind list
  from scratch per feed request for one of only five possible values, on the
  Torznab hot path (every Prowlarr RSS poll from every friend). → a
  `LazyLock<[ScopeFilter; 5]>` indexed by scope; the clause/binds ordering
  contract is preserved exactly, it just gets built once.

### 12. `sharerr` (largest — last)

- **W1** **[verified]** `fn vault_in(dir: &TempDir) -> Vault` copy-pasted
  byte-identical (modulo `unwrap`/`expect`) in four test modules:
  `gossip.rs:702`, `lighthouse_client.rs:652`, `sync/tests.rs:1751`,
  `commands/doctor.rs:1070`. All four are in *this* crate, so the fix is one
  crate-local `#[cfg(test)] pub(crate) fn vault_in` — not a testkit export,
  which would drag `sharerr-store`, `secrecy`, and `tempfile` in as non-dev
  dependencies for a helper only one crate uses.
- **W2** **[verified]** `web/settings.rs:1169,1194,1218` — `rotate_`, `clear_`,
  and `finalize_tracker_token` have the same five-line body (`open_vault` → the
  matching `_in` fn → `tracing::info!` → `invalidate` → `legacy_token_status().reset()`),
  differing only in the inner call and two strings. → one
  `mutate_tracker_token(state, log_msg, reason, f)`. Keeps the `_in` split that
  tests drive directly.
- **W3** **[verified]** `web/topology.rs:913` — `layout` takes eight positional
  args under an `#[allow(clippy::too_many_arguments)]` and returns a bare
  `(Vec<Node>, Vec<Edge>, i32, i32)` whose two `i32`s callers must remember the
  order of. → a `LayoutInput` struct and a named `Layout` return; the `#[allow]`
  then goes away rather than being carried.

---

## Two decisions — settled

### D1 — merge gossip's and lighthouse's record types · **DECIDED: merge, keep the `Lighthouse*` names — done**

`GossipEndpointRecord` is the schema name that disappears from the published
OpenAPI document; `LighthouseEndpointRecord` and `LighthouseRecordEndpoint`
survive. `sharerr/src/gossip.rs` re-exports the types from
`sharerr-lighthouse`; `to_lighthouse_record` (`lighthouse_client.rs:233`) and
the third inline `signable_bytes` copy (`:591`) are deleted.

Background: `lighthouse/src/lib.rs:83-149` (`RecordEndpoint`, `EndpointRecord`,
`signable_bytes`, `verify`) duplicates `sharerr/src/gossip.rs:57-131`.
**[verified] structurally identical**, differing only in doc comments and
utoipa attributes. The module doc justifies the split as "gossip's lives in
the `sharerr` binary crate, which this crate deliberately does not depend
on" — but **[verified]** the arrow already points the other way
(`sharerr/Cargo.toml:40`), so unifying costs lighthouse nothing.

**Done.** `gossip.rs` now `pub use sharerr_lighthouse::{EndpointRecord,
RecordEndpoint, signable_bytes, verify};` instead of redeclaring all four;
its own `RecordBatch` stays local (gossip-specific, no lighthouse
equivalent) with its `records` field now holding the re-exported type.
`lighthouse/src/lib.rs`'s `signable_bytes` was promoted `pub` so gossip
could reuse it. `to_lighthouse_record` (`lighthouse_client.rs`) is deleted —
once the two types are literally the same type, converting between them is
a no-op, so `gossip::self_record(state).await` is now handed straight to
`report()`. Its test (`to_lighthouse_record_copies_every_field`) is deleted
for the same reason: nothing left to test once there is no conversion.
`lighthouse_client.rs`'s test-only `signed_lighthouse_record` helper's
inline `Signable`-struct duplicate of `signable_bytes` is gone too — it
calls `sharerr_lighthouse::signable_bytes` directly now. `docs/openapi.json`
regenerated: `GossipEndpointRecord`/`GossipRecordEndpoint` are gone from the
published document, `LighthouseEndpointRecord`/`LighthouseRecordEndpoint`
stand in their place, confirmed by grep and by
`openapi::tests::the_committed_document_is_current` passing.

### D2 — lighthouse's private `now_epoch` · **DECIDED: keep both, make them match — done**

`lighthouse/src/lib.rs:161` reimplements `sharerr_core::endpoint::now_epoch`
(`core/endpoint.rs:277`), whose doc claims to be "the one place this is
computed". The two **already differ**: core saturates via
`i64::try_from(..).unwrap_or(i64::MAX)`, lighthouse uses a bare `as i64`.

No `sharerr-core` dependency — the crate keeps standing alone. Change
lighthouse's bare `as i64` to core's saturating form so the two cannot
disagree at the `i64` boundary, and note the intentional twin in both doc
comments so `endpoint.rs:277`'s "one place" claim stops being false.

**Done.** `lighthouse/src/lib.rs`'s `now_epoch` now uses
`i64::try_from(d.as_secs()).unwrap_or(i64::MAX)`, matching core's. Both
`now_epoch` doc comments now name the other as an intentional twin rather
than one claiming to be the sole implementation.

Both `docker build .` (main image) and `docker build -f Dockerfile.lighthouse
.` (lighthouse image) were re-run after D1+D2 and pass clean under the
pinned 1.98 toolchain — this batch touched `sharerr-lighthouse`'s public
surface directly, so both images mattered here, not just the main one.

---

## Execution scope for the batch in progress

1. **Write this entire plan out as `NEXT.md`** in the repo, so the deferred
   two-thirds survives the session. *(this file — done)*
2. **Do X1–X5 only** in this pass — the cross-crate work. Everything under
   "Per-crate findings" waits for a later pass.

Order within the batch, chosen so each step compiles on its own:

| Step | Change | Crates touched |
|---|---|---|
| 1 | X2 `debug_redacted` — additive, no call-site churn until adopted | client, then all four backends |
| 2 | X1 `check_status` + `malformed` — transmission and rtorrent only | client, transmission, rtorrent |
| 3 | X4 `http_client_with_timeout` returns `Result<_, reqwest::Error>` | client, arr, qbit |
| 4 | X3 `shutdown_signal` moves to `lighthouse/src/lib.rs` | lighthouse, sharerr |
| 5 | X5 test helpers — `base_url`/`QBIT_API_KEY` call sites, plus `body_text`/`requests_to` lifted into `testkit::mock` | testkit, all four backends |

X2 goes first because it is purely additive and the least likely to cascade.
X5 goes last because it is test-only, so a failure there cannot mask a
production-code problem from steps 1–4.

---

## Checked and deliberately NOT filed

- **Three `poll_loop`s** (`system_stats.rs:55`, `gluetun.rs:327`,
  `swarm_history.rs:29`) share only a three-line `loop { work; sleep }` over
  genuinely different bodies and intervals. Over-abstraction — leave it.
- **`tracker.rs:152,185` `#[allow(dead_code)]`** on `AnnounceParams`/`ScrapeParams`
  are documentation-only utoipa shapes with a stated reason. Correct as written.
- **`doctor.rs` vs `checks.rs`.** The parallel `check_arr`/`check_library`/
  `check_qbit`/`check_paths` names look like duplication. **[verified]** they are
  not: `doctor.rs` delegates into `checks::` at `:386,494,582,824,881` and its own
  functions are thin reporting wrappers.
- **`sharerr-transmission` as one file** — 552 production lines is under the
  threshold where a qbit-style split pays for itself. (rtorrent at 778 is over it;
  see R4.)
- **`probe`'s two metadata loops** (`matroska_meta:100`, `isobmff_meta:168`) look
  similar, but track types, codec accessors, and the `und` case differ enough
  that sharing costs more than the ~15 lines saved.
- **`probe` vs `MediaMeta::scene_*`** — the probe deliberately does *not*
  duplicate core's scene-token mapping; `probe/src/lib.rs:280-286` documents the
  split and its tests at `:648,686` verify it holds.
- **`arr/src/client.rs:24-29` `api_prefix`** restates `MediaSource::api_version`,
  but both sides carry comments arguing for the split deliberately.
- **`TorrentClient` trait** (`client/src/lib.rs:361`) already gives the three
  backends the right shared altitude. No trait to invent.

---

## ETA

Measured inputs: 1,216 test functions, a warm 74 GB `target/`, and **six of the
twelve crates depend on `sharerr-client`** — so every X-series change
invalidates nearly the whole workspace and forces a full rebuild plus a full
test run.

| Group | Findings | Edit effort | Verification | Subtotal |
|---|---|---|---|---|
| **X1–X5** cross-crate | 5 | heavy — touches 5 crates, public surface | 3 full-cascade loops | **60–90 min** |
| Crates 1–2 client/probe | 2 | trivial | 1 loop | 10–15 min |
| 3 transmission | 4 | light | 1 loop | 15–20 min |
| 4 lighthouse | 2 | L1 is a lock-model change | 1 loop | 15–20 min |
| 5 rtorrent | 5 | **R4 is a file split**, R2 rewrites `set_trackers` | 2 loops | 40–60 min |
| 6 testkit | 1 | moderate — 3 types → 1 | 1 loop (cascades) | 15–20 min |
| 7 qbit | 3 | Q1 threads through 10 call sites; **Q3 needs pairwise test diffing** | 2 loops | 40–55 min |
| 8 arr | 1 | one generic helper + 3 call sites | 1 loop | 20–25 min |
| 9 torrent | 3 | B1 changes a hot path, B2 is a lifetime change | 2 loops | 35–50 min |
| 10 core | 6 | **N1/N2 are macro work across 3 crates**; N3 touches web + templates | 3 loops | 70–100 min |
| 11 store | 6 | S2/S4 add SQL; S3 is a bind-closure refactor | 2 loops | 45–60 min |
| 12 sharerr | 3 | all three are contained | 1 loop | 20–30 min |
| MSRV gate | — | `docker build .` once after X-series | — | 10–20 min |

**Total: roughly 6–9 hours of working time**, spread across ~20 verification
loops.

The dominant variable is the loop itself. `cargo test --workspace` plus
`clippy --all-targets --all-features` over 1,216 tests is minutes, not seconds,
and the X-series and testkit changes cascade to nearly every crate — so the
estimate is mostly *waiting on cargo*, not writing code. If that lands slower
than assumed, the total stretches proportionally.

Three items carry most of the schedule risk:

- **N1/N2** (the `str_enum!` and `config_paths!` macros) are the largest single
  piece — nine enums and 39 path constants across three crates. If declarative
  macros fight the utoipa derives, this is the one to defer.
- **R4** (splitting rtorrent) is mechanical but touches every line's module
  path; cheap to do, noisy to review.
- **Q3** cannot be time-boxed honestly until the test pairs are actually
  diffed — the survey's "9 duplicates" claim did not survive checking, so the
  real number is unknown until it is looked at directly.

**If a shorter pass is wanted:** X1–X3, W1–W3, N6, S5, and the crate-local
items (P1–P2, T1–T4, L2, R5) are the mechanical two-thirds — call it
**2.5–3.5 hours** for a diff that is almost all deletion, with N1/N2/N3, R4,
B1/B2, and S2/S3 deferred to a second pass.

## Verification

After **each** crate, the project's standard loop (`CLAUDE.md` → "The
verification loop"):

```bash
cargo test --workspace \
  && cargo clippy --workspace --all-targets --all-features -- -D warnings \
  && cargo build \
  && cargo fmt --all --check
```

Run `cargo fmt --all` first if `--check` trips. Clippy stays at zero warnings —
never weaken a workspace lint to make something compile; a test that must panic
gets the module-level `#![allow(clippy::unwrap_used, clippy::expect_used)]`.

Two extra gates for this particular set of changes:

- **X1/X2/X4 and R4 touch crate boundaries and public surface.** `cargo build`
  alone will not catch an MSRV breach — `rust-version` is 1.98 and a local
  toolchain is newer. Run `docker build .` once after the cross-crate work
  lands, per `CLAUDE.md`'s MSRV note.
- **N2 changes how `config_paths::ALL` is generated**, and `ALL` is what the
  settings UI enumerates. After it, confirm the settings page still renders and
  saves every section — `web/settings.rs` and `web/templates/settings.html`
  hold hand-typed path strings that must agree.
