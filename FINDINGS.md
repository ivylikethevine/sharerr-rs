# Code review findings — entire codebase

Review date: 2026-08-21 (branch `dev`, HEAD `3d2579d`).
Method: 8 finder angles (3 correctness, 3 cleanup, 1 altitude, 1 conventions)
over the whole workspace, every candidate then independently verified against
the code by a separate pass. Verdicts: **CONFIRMED** = reproduced from the code
with citations; **PLAUSIBLE** = realistic but depends on ordering/config;
nothing was refuted. Line numbers are as of the commit above.

Nothing below has been fixed yet; this file is the worklist.

---

## Top 10 (most severe first)

### 1. rTorrent upload limit calls a non-existent XML-RPC method — CONFIRMED
`crates/sharerr-rtorrent/src/lib.rs:351` sends `throttle.up.max.set` with a
bytes/s value (`kib * 1024`, :349). rTorrent's API is `throttle.up = name,rate_kib`
(KiB/s); `throttle.up.max` is a name→rate getter with no `.set` variant.
**Failure:** `torrent_backend = rtorrent` + any `seeding.upload_limit_kib`:
`load.raw_start` (:339) loads the torrent, then the throttle call faults and
`add()` returns `Err` (:350-354) — every item is recorded failed on every pass
while it is actually live, and no throttle is ever applied. Module docs (:50) and
tests (:1115, :1141) encode the same bogus method name, so nothing hermetic
catches it.

### 2. rTorrent XML-RPC parser breaks on `&`, `<`, `>` in any name/path — CONFIRMED
`crates/sharerr-rtorrent/src/lib.rs:654` `read_element_text` expects exactly
one `Event::Text` then calls `expect_end` (:656); `parse_value` (:583) has the
same shape. quick-xml 0.41.0 (pinned in Cargo.lock) tokenises
`<string>Tom &amp; Jerry</string>` as `Text("Tom") / GeneralRef("amp") / Text("Jerry")`
(verified with a scratch run). The reader config (:543-544) only sets
`trim_text`, no entity expansion.
**Failure:** any torrent named e.g. `Tom & Jerry (1940)` → `expect_end` receives
`GeneralRef` → `Err("expected </string>, got GeneralRef")` (:678-681) →
`ClientError::Malformed` (:162) → the whole `list()`/`files()` fails → every
rTorrent sync pass aborts.

### 3. gluetun advertises `<vpn-exit-ip>:<static tracker.port>` when the forwarded-port lookup fails — CONFIRMED
`crates/sharerr/src/gluetun.rs:403` takes `fallback_port` from
`endpoint.current()`, which falls back to the *static* configured base
(`crates/sharerr-core/src/endpoint.rs:184`, `.or_else(|| inner.static_base.clone())`)
whenever no dynamic observation exists — first poll after start, or right after
`/gluetun/down` cleared the history. `resolve_base` (:296-305) then pairs the
live VPN exit IP with that static port on `NoForwardedPort` (port 0, :277) or any
error; `observe()` (endpoint.rs:205) has no guard; the sync loop is woken
(:413-416).
**Failure:** every seeding torrent's announce URL (via `set_trackers` + cached
`.torrent`) is rewritten to an address reachable nowhere, and the bogus URL
lingers in `recent()`/magnet tiers for up to 4 observations. Only guarded when
no static base is configured at all (fallback `None` → `Err`). Contradicts the
comment that `/gluetun/down` clears the fallback.

### 4. Feed `.torrent` links are `http://localhost:<port>/…` on a gluetun-only deployment — CONFIRMED
`crates/sharerr/src/torznab.rs:634` feeds `Matched::download_url` (:549-556)
from `config.public_base_url()`, which (`crates/sharerr-core/src/config.rs:419-426`)
only consults the static advertised base and otherwise returns
`http://localhost:<bind port>`. The magnet tiers in the same response use the live
`state.endpoint().recent()` (:623-628). Same static base in `jackett.rs:143/151`
(site_link) and `web/peers.rs:308` (feed_url).
**Failure:** the README-recommended gluetun-only setup (no `advertised_host`): a
friend's Sonarr grabs `http://localhost:8477/torrents/<hash>.torrent?token=…` on
*their* box and fails, while the `magneturl` attr is correct. Feed preview looks
healthy.

### 5. A "Reused" pre-existing torrent is published with no cached `.torrent`, no sharerr tracker, and is later removed by untag — CONFIRMED
`crates/sharerr/src/sync/seed.rs:84-92` returns `SeedOutcome::Reused` straight
from `find_existing` (fed by `qbit.list(None)` at `sync/mod.rs:382`, i.e. *all*
torrents, not just sharerr's category) without calling `set_trackers` or writing
`torrent_dir/<hash>.torrent`. `sync/mod.rs:613` `set_seeding` then records
Seeding + the current token fingerprint.
**Failure:** library path == download path (or an operator cross-seeds), so the
client already holds the original private-tracker torrent covering the file.
(a) `tracker::torrent_file` (`tracker.rs:441-450`) 404s "torrent file missing"
on every friend download; the magnet's `tr=` points at sharerr's tracker but the
local client never announces there → friends join an empty swarm.
(b) Operator removes the tag → `withdraw_untagged` (`sync/mod.rs:661`) calls
`qbit.remove(hash)` on a torrent sharerr did not create — violates "preserve
existing torrents".

### 6. Lighthouse `report` is unpinned, unbounded, and case-inconsistent — CONFIRMED (all sub-claims)
`crates/sharerr-lighthouse/src/lib.rs:189-206`.
(a) `key_hash` is SHA-256 of the shared API key (`hash_key`, :123-131), not of
the pubkey; `report()` never binds `key_hash` to `record.pubkey` — `verify()`
(:105-121) only checks the record is self-consistent under whatever pubkey it
carries. Anyone who learns a key_hash (it is a URL path segment, visible in
proxy/access logs) can displace the genuine record with one under their own
keypair. The impersonation is caught client-side (`lighthouse_client.rs:246`
`record.pubkey != expected_pubkey` → treated as decoy), but the legitimate
record is still gone and rendezvous for that pair breaks.
(b) `signed_at` is compared only against the existing entry (:200-201), never
against now, so `signed_at = i64::MAX` locks the slot forever (all later genuine
reports answer `Stale`).
(c) `records: RwLock<HashMap>` has no cap/TTL/eviction; the unauthenticated POST
handler (:301) inserts any valid 64-hex key_hash with up to the 2 MB JSON body →
memory DoS.
(d) `valid_key_hash` (:126) accepts uppercase hex, `report()` stores verbatim
(:205), `lookup()` lowercases (:329) → mixed-case entries are unreachable and
bypass per-hash staleness dedup (2^64 case variants per logical hash).

### 7. Tracker fails open after a transient vault error — CONFIRMED
`crates/sharerr/src/state.rs:315-320` `cached_from_vault`: `Err(_) => None`
then `*cache.write().await = Some(value.clone())`, so a failed `open_vault` is
cached as "no token" until `invalidate()` (i.e. an operator saves settings).
`tracker.rs:355-357` `let Some(required) = required else { return Ok(TokenAuth::default()) }`
admits every announce. `handle_announce` explicitly fails *closed* for an
unopenable store (:177-184) but not for an unopenable vault.
**Failure:** `tracker.token` set; first announce after start arrives while
`open_vault_at` (`secrets.rs:80-87`) fails transiently (spawn_blocking/IO error,
`SHARERR_MASTER_KEY_FILE` briefly unreadable during a mount/rotation race) →
token auth silently disabled, peer attribution/revocation stop working. The
doc comment at :302-306 calls caching "correct"; the consequence stands.

### 8. `refresh_announce` reports a never-updated torrent as using the current token — CONFIRMED
`crates/sharerr/src/sync/seed.rs:141` returns `Ok(false)` on `NotFound` of the
cached `.torrent` without touching the client; `sync/mod.rs:532-537` matches
`Ok(_) =>` and calls `set_announce_token_fp(current)`; `web/items.rs:279`
renders stored == current as `TokenStatus::Valid`.
**Failure:** operator rotates the token, then the torrents cache dir is lost
(fresh `/data` with intact DB) or the item is a Reused one (finding 5): items
page shows every torrent Valid while the client still announces the old token;
operator finalises the rotation trusting the page; local seeder announces are
rejected (`BadToken`), no swarm forms, no UI signal.

### 9. Open redirect via `sanitize_next` — CONFIRMED
`crates/sharerr/src/web/settings.rs:92`
`next.filter(|path| path.starts_with('/') && !path.starts_with("//"))`.
**Failure:** `POST /settings/general?next=/\evil.example` (or `%5C`) passes;
browsers normalise `\` → `/` in special-scheme URLs, so the Location resolves as
scheme-relative `//evil.example` and sends the signed-in operator off-site.
Also: `next` containing `%0D`/`%0A` passes and axum 0.8 `Redirect::into_response`
turns the `HeaderValue` failure into a 500 (`redirect.rs:89-91`) — the save
succeeds but the response is an error page.

### 10. Secrets are written to the vault before the rest of the same form is validated — CONFIRMED
`crates/sharerr/src/web/settings.rs:474-485` `save_tracker` runs
`clear_tracker_token`/`rotate_tracker_token` (vault put of
`TRACKER_TOKEN_PREVIOUS` + `TRACKER_TOKEN`, syncer invalidate, legacy-status reset
— :986-1020) and only then calls `write_config` (:485), whose closure validates
port/host/url. Same secret-first ordering in `save_arr` (:307 before :312),
`save_qbittorrent` (:356), `save_transmission` (:396), `save_rtorrent` (:431),
`save_gluetun_section` (:693), `save_notifications` (:789).
**Failure:** operator types token T1 + a bad advertised host in one submit;
rotation runs (previous=T0, current=T1), then `reject()` shows an error and the
token field re-renders empty. Operator fixes the host and pastes T2 → previous=T1,
current=T2 — T0 silently dropped from the grace slot, every peer still on T0
rejected immediately, defeating the rotation grace period. For the other
handlers: a rejected save still commits the credential and invalidates the
syncer while the page implies nothing was saved.

---

## Also verified — correctness, CONFIRMED

- **`write_config` is an unsynchronised read-modify-write of `sharerr.toml`** —
  `crates/sharerr/src/web/settings.rs:884` `ConfigFile::open`, :894 `save()`
  (fixed `sharerr.toml.tmp` + rename, `config_io.rs:252-255`), :898
  `replace_config`; no mutex (ServeState only has per-field RwLocks,
  `state.rs:52-99`). Two concurrent section saves (two tabs, wizard + settings,
  htmx double-submit): each applies only its own edits, the second rename
  overwrites the first, the last `replace_config` installs a stale `Config`, both
  redirect with `?saved=`.

- **`Vault::put` is an unlocked whole-file RMW with a shared `vault.tmp`** —
  `crates/sharerr-store/src/vault.rs:234` inserts into the in-memory map and
  `persist` (:305) rewrites the entire file via `self.path.with_extension("tmp")`
  (:331) + rename; no flock/mutex/re-read anywhere. Every caller opens a fresh
  `Vault` (`state.rs:287` → `secrets.rs:80`): `web/settings.rs:952/1014/1039/1063`,
  `web/peers.rs:162-170`, `gossip.rs:165` `Identity::load_or_create`,
  `state.rs:581` `load_or_create_decoy_seed`, `commands/vault.rs:20/66`.
  Concurrent writers (settings save vs gossip identity / decoy seed; CLI
  `vault set` vs `serve`) last-write-wins and drop each other's records, and can
  rename each other's half-written temp file into place.

- **Concurrent first `Store::open` races on migrations** — `state.rs:271`
  `ServeState::store()` (read lock dropped before `Store::open`, so two web
  requests also race) and `Syncer::build` (`sync/mod.rs:302`) both run
  `MIGRATOR.run` (`db.rs:99`); `serve.rs:174-176` starts `axum::serve` and
  `background()`/`ensure_ready` concurrently. sqlx-sqlite 0.8.6 `lock`/`unlock`
  are no-ops; migrations use plain `CREATE TABLE`/`CREATE INDEX`/`ALTER TABLE ADD COLUMN`
  (0001:6,27; 0005:11,40; 0006:24,38,47; 0007:15) with no `IF NOT EXISTS`. On an
  upgrade with a pending migration the loser fails with "already exists" /
  "duplicate column" → feed answers 503 or the syncer is marked blocked until
  retry.

- **Transmission and rTorrent HTTP clients have no timeout** —
  `crates/sharerr-transmission/src/lib.rs:80-82` and
  `crates/sharerr-rtorrent/src/lib.rs:113-115` are `reqwest::Client::builder().build()`
  with no `.timeout` and no per-request timeout; qbit (`client.rs:96`, 60s) and
  arr (`client.rs:64`, 30s) set one. A host that accepts TCP and stalls (VPN
  namespace half-up, nginx→SCGI wedged) blocks the sequential `background()` sync
  loop forever; `/ready` still reports ready; `sharerr sync` never exits.

- **No SIGTERM/SIGINT handling; runs as PID 1 with no init** —
  `crates/sharerr/src/commands/serve.rs:163` and `:174` call `axum::serve` with no
  `with_graceful_shutdown`; no `tokio::signal`/`ctrl_c` anywhere under
  `crates/sharerr/src`; `Dockerfile:128-129` `ENTRYPOINT ["/usr/local/bin/sharerr"]`
  with no tini/`STOPSIGNAL`; no compose file under `docker/` sets `init: true`.
  PID 1 without an installed handler ignores SIGTERM, so `docker stop` waits the
  full grace period then SIGKILLs — possibly mid-sync, mid `torrents/add`, or
  mid config rewrite, on every restart/upgrade. Same for `sharerr-lighthouse`.

- **Torznab daily-series searches get a bare 400** — `crates/sharerr/src/torznab.rs:390`
  `pub ep: Option<u32>` extracted via plain `Query<SearchQuery>` (:504). Torznab
  daily shows send `ep=MM/DD` (e.g. `ep=01/15`); deserialisation fails before the
  handler runs; Prowlarr logs an indexer failure and escalates backoff, hiding
  every other release.

- **A hand-typed tracker token with `/`, `?`, `#` makes every announce URL unroutable** —
  `crates/sharerr/src/web/settings.rs:473-477` only trims; `commands/vault.rs:104-111`
  `validate_secret` only trims/rejects empty; `crates/sharerr-torrent/src/tracker.rs:161`
  builds `format!("{}/{token}", join_path(base, ANNOUNCE_PATH))` unencoded, and the
  route `/announce/{token}` (`tracker.rs:118-121`) matches one segment. A pasted
  base64 token like `ab/cd+ef==` 404s for every client and
  `token_from_announce_url` returns only `ab`.

- **`advertised_host` accepts a scheme/path and silently yields a dead base** —
  `crates/sharerr-core/src/endpoint.rs:68` `Url::parse(&format!("http://{host}:{port}"))`
  with no scheme check; `web/settings.rs:1168-1183` `validate_advertised_host`
  only rejects localhost/private IPs; core config has no validation. Verified:
  `Url::parse("http://https://seed.example:8477")` succeeds with host `https`,
  path `//seed.example:8477`. `seed.example/sharerr` likewise loses the port.

- **IPv4-mapped IPv6 peers never canonicalised** — `crates/sharerr-torrent/src/announce.rs:401`
  partitions on `a.is_ipv4()`, so `::ffff:a.b.c.d` lands in v6 and is packed
  16+2 bytes into `peers6` (`pack_compact` :556); `sharerr-core/src/endpoint.rs:29`
  treats V6 as private only if loopback or fc00::/7, so
  `is_private_ip(::ffff:192.168.1.5) == false` and `resolve_addr` (:160-162)
  ignores the declared `ip=`. No `to_canonical`/`to_ipv4_mapped` anywhere.
  Caveat: default `server.bind` is `0.0.0.0:8477` (`config.rs:543`, docs, every
  docker example), so this only triggers when an operator binds `[::]:port`.

- **Non-compact announce responses omit IPv6 peers entirely** —
  `crates/sharerr-torrent/src/announce.rs:404-417` builds the dictionary `peers`
  list from `&v4` only; :421 guards `peers6` with `compact && !v6.is_empty()`. A
  `compact=0` client in an IPv6 swarm sees `complete`/`incomplete` counting
  peers but an empty list forever.

- **Freshness is decided purely by sender-supplied timestamps, no skew clamp** —
  `crates/sharerr/src/gossip.rs:335` `stored >= record.signed_at`,
  `crates/sharerr-lighthouse/src/lib.rs:201` `existing.record.signed_at >= record.signed_at`,
  `crates/sharerr-store/src/endpoints.rs:117` `excluded.observed_at > peer_endpoints.observed_at`.
  No comparison to local time anywhere in ingest. A friend's host clock 2 days
  fast signs a self-record; after they fix it, every genuine update is `stale`
  for two days and `record_peer_endpoint` rejects fresher observations; a VPN
  port rotation in that window leaves a dead endpoint nothing can correct.

- **An item that fails in `resolve_for` is counted failed but invisible** —
  `crates/sharerr/src/sync/mod.rs:557` `resolve_for` (`paths.rs:76` errors
  `NotAbsolute`) runs before the "record before anything can fail" upsert
  (:605); the error branch (:434) `set_state(Failed)` is a plain `UPDATE … WHERE`
  (`db.rs:291`) affecting 0 rows. Sonarr/Radarr on Windows (`C:\tv\…`, which
  `Path::is_absolute()` rejects on Linux): run summary says "1 failed", items
  page has no row and no `last_error`.

- **qBittorrent "Test connection" tests whichever backend is selected** —
  `crates/sharerr/src/web/probe.rs:197` `qbit_badge` calls
  `torrent_client_badge(state, config, config.torrent_backend)` (doc at :195 even
  says so), while `transmission_badge`/`rtorrent_badge` pin their own backend;
  `settings.html:86` and `wizard.html:126` post `/settings/test/qbittorrent` from
  the `<h3>qBittorrent</h3>` section (settings.html:335). With
  `torrent_backend = transmission` that button reports Transmission's result and
  never checks the qBittorrent credentials just saved.

- **Items page renders the full announce URL, embedding the "never rendered back" tracker token** —
  `crates/sharerr/src/web/items.rs:289-293` `current_announce_url` builds
  `announce_url(&base, tracker_token())`, stored at :261, printed verbatim per
  row in `items.html:108` (`<code title="{{ url }}">{{ url }}</code>`), contradicting
  `settings.rs:11-13` ("A stored secret is never rendered back, not even masked")
  and `generate_secret`'s show-once model (:67-69). Mitigations: `/items` is
  behind the same `require_auth` layer as `/settings` (`web/mod.rs:107,121,157`),
  and the token is by design shipped inside every `.torrent`, so it is a bearer
  value already disseminated to peers.

- **`set_gossip` orphans vault secrets; `delete_peer` never cleans them** —
  `crates/sharerr/src/web/peers.rs:162-174` opens the vault and
  `vault.put(peer_gossip_key(id))` before any existence check; the only DB touch
  is `store.set_peer_gossip_url` (:176), whose `Ok(false)` (rows_affected == 0,
  `endpoints.rs:190-198`) is swallowed by `applied()`'s `Ok(_) => Redirect`
  (:248). `delete` (:186-196) only calls `store.delete_peer` (:349-356); no code
  path removes `peer.gossip.{id}`. `POST /peers/9999/gossip` from a stale tab
  leaves an orphan visible in `sharerr vault list` with no UI to remove it.
  Mitigation: `peers.id` is AUTOINCREMENT (`0003_peers.sql:32`) so an orphan is
  never re-attached to a new friend.

- **`dotted()`/`loose_eq()` drop every non-ASCII character** —
  `crates/sharerr-torrent/src/title.rs:231`
  `.map(|c| if c.is_ascii_alphanumeric() { c } else { ' ' })` with empty →
  `"Unknown"` (:236-237); `:259` key `.filter(|c| c.is_ascii_alphanumeric())`.
  `千と千尋の神隠し (2001)` → release title `Unknown.2001.WEB-DL.x264-SHARERR`
  (every CJK/Cyrillic/Greek title collapses to the same name); `Amélie (2001)` →
  `Am.lie.2001…`, loose key `amlie`. Release title is the only matchable data
  for directory-sourced items.

## Also verified — correctness, PLAUSIBLE

- **`resolve_sharerr` and `resolve()` disagree on rules with `qbit = None`** —
  `crates/sharerr-core/src/paths.rs:80-85` `resolve()` is first-match-in-order
  with `map.qbit.as_ref().unwrap_or(&map.sharerr)`; `:120-121` `resolve_sharerr()`
  is most-specific-match over only rules with `let qbit_prefix = map.qbit.as_ref()?`.
  With `[{arr=/tv/extras, sharerr=/media/extras}, {arr=/tv, sharerr=/media, qbit=/downloads}]`
  (specific rule first), `resolve` gives `/media/extras/x` and `resolve_sharerr`
  gives `/downloads/extras/x` — same file, two qBittorrent paths by entry point;
  with `skip_checking = true` the torrent points at a non-existent file. (The
  order given in the original candidate does not diverge, hence PLAUSIBLE.)

- **`hash_of_last_add` can throttle an unrelated torrent** —
  `crates/sharerr-rtorrent/src/lib.rs:411-425` does `d.multicall2(main, d.hash=)`
  and takes `rows.last()` (:418). rTorrent processes `load.raw*` asynchronously
  and a duplicate info hash is only logged ("Info hash already used"), not
  faulted, so the RPC succeeds with nothing appended; `main` honours any
  `view.sort_current = main,…` in `.rtorrent.rc`. Either way
  `d.throttle_name.set` (:356) lands on the wrong torrent. The info hash is
  already known to the caller (`factory.rs:147`) but `AddRequest`
  (`sharerr-client/src/lib.rs:211`) does not carry it.

---

## Cleanup / efficiency / altitude — CONFIRMED unless noted

- **Vanished torrent triggers a full re-hash although the `.torrent` is cached** —
  `crates/sharerr/src/sync/seed.rs:95`: `share()` passes only paths/announce/
  torrents to `seed()` (never `known.info_hash`); `find_existing` → `build()`
  always runs `LavaTorrentFactory.create` ("gigabytes of CPU work", :183) and
  rewrites `torrent_file_path(torrent_dir, info_hash)` — the very file that
  already exists. After a client reinstall / wiped session every item is
  re-hashed. Pass `known.info_hash` in; if the cached file reads, `rewrite_announce`
  it (logic `refresh_announce` :135 already has) and `add` those bytes.

- **Torrent-client credential resolution re-implemented four times with differing semantics** —
  `crates/sharerr/src/sync/mod.rs:715-733` (`vault.get(key)?` + context naming
  missing keys), `web/probe.rs:212-239` (opens vault once, folds unreadable into
  `Err`), `commands/doctor.rs:515-543` (reads api_key via `quiet_secret`, password
  via reporting `secret()`, returns early, never calls `TorrentCredential::choose`),
  `web/topology.rs:405-418` (`secret_or_none` + inline match). `checks.rs:425`
  comment already admits "three of which resolve secrets from three different
  places". Next divergence makes `doctor` test a different credential than the
  one that seeds. Put one `resolve_torrent_credential` next to
  `checks::build_torrent_client`.

- **`TorrentBackend` lacks `ALL` + `parse()`** — `crates/sharerr-core/src/config.rs:604-623`
  has only `as_str`/`display_name` (vs `LighthouseMount` :732/746, `NotifyKind`
  :974/985, `LibraryKind` :1007/1020, `MediaSource` `model.rs:56/101`).
  Hand-matched at `web/probe.rs:38-42`, `web/settings.rs:456-459`,
  `web/settings.rs:1119-1123`, `probe.rs:467-469` (tests); `web/mod.rs:124-126`
  per-backend routes + near-identical `save_transmission`/`save_rtorrent`.
  Adding a fourth client means ~7 sites; miss the probe arm and its Test button
  says "Unknown service".

- **`topology::gather` duplicates `diagnostics::gather`, and both serialise independent I/O** —
  `crates/sharerr/src/web/topology.rs:185-241` and `web/diagnostics.rs:58-117`:
  `join_all` over `configured_sources` → `check_arr`, then `spawn_blocking` over
  `config.library` → `check_library`, then `spawn_blocking check_paths`; in both
  the library scan only starts after the arr `join_all` has been awaited
  (topology :200 vs :221; diagnostics :75 vs :87). Already diverging (diagnostics
  reports a panicked library scan; topology drops it). Lift into one
  `checks::snapshot(...)` and `tokio::join!` the arr probes with the library
  scan — wall time of the two slowest pages drops from sum to max.

- **`doctor` and `sync` hard-code the tracker tunnel** — `commands/doctor.rs:705-710`
  reads only `config.gluetun.control_url` + `secret_keys::GLUETUN_API_KEY`; no
  reference to `GluetunTarget`/`gluetun_client` in doctor.rs, so `[gluetun_client]`
  (`config.rs:317`, `GLUETUN_CLIENT_API_KEY` :65) is never checked, while
  `gluetun.rs:62-72` and `web/diagnostics.rs:121-135` cover both.
  `commands/sync.rs:18-23` + `gluetun_api_key` (:69-75) likewise single-tunnel
  (it does use `GluetunClient::new`, and a one-shot sync arguably only needs the
  tracker tunnel — weaker). A dual-VPN operator with a wrong client-tunnel key
  gets a clean `doctor`.

- **Dual-token admission spelled out twice** — `crates/sharerr/src/tracker.rs:180-193`
  (announce) and `:244-261` (scrape) both do store → `authenticate_token(current,
  previous, supplied)` → `if via_previous { record_used }`. `items.rs:271-285`
  `token_status` and `:293-299` `current_announce_url` consult only the current
  fingerprint, so a previous-token item renders Stale while the tracker admits
  it — the doc comment (:280-282) frames that as intended, so the items half is
  design. Expose one `TrackerState::authenticate(...)` owning lookup + `record_used`.

- **Four hand-rolled `reqwest::Client` builders; duplicated `fn unreachable`** —
  `sharerr-transmission/src/lib.rs:80` and `sharerr-rtorrent/src/lib.rs:113` (no
  timeout), `sharerr-qbit/src/client.rs:96` and `sharerr-arr/src/client.rs:63`
  (each with its own `DEFAULT_TIMEOUT`, 60s vs 30s); `fn unreachable` is
  byte-identical apart from `base` vs `endpoint` at transmission :98 / rtorrent
  :124. `sharerr_client` already exports `normalise_base` (:88), `error_chain`
  (:47), `clamp_body` (:38) — add `http_client()` there.

- **Per-tick / per-event client rebuild and vault re-open** — `gossip.rs:476-479`
  (per `run_exchange`), `lighthouse_client.rs:82-84` (per tick from :67),
  `notify.rs:84-86` (`webhook()` per event at :107/:166) each build a fresh
  `reqwest::Client`; `gossip.rs:475`, `lighthouse_client.rs:79`, `notify.rs:74`
  open the vault (Argon2, "tens of milliseconds and ~19 MiB") each time.
  `gluetun.rs:200-215` keeps one client and documents why; `state.rs:306`
  `cached_from_vault` exists. Gossip's per-peer keys legitimately need the vault;
  the client rebuild does not.

- **Dead public API** — `crates/sharerr-lighthouse/src/lib.rs:223`
  `pub async fn known_key_hashes`: `grep -rwn` finds only the definition; the
  standalone binary its doc comment names never calls it. Delete or wire into
  the binary's log line.

- **Trivial duplicates — PLAUSIBLE** — `web/items.rs:181-183` `urlencode` is the
  identical one-liner to `torznab.rs:250-252` `encode_component`; `peers.rs:352`
  `ago` and `topology.rs:641` `compact_ago` share the 60/3600/86400 ladder but
  `compact_ago`'s doc (:638-640) states the format difference is intentional.
  Make `encode_component` `pub(crate)`; optionally express both `ago`s through
  one function so one test covers both thresholds.

---

## Conventions (CLAUDE.md)

No violations found.

## Suggested first batch

Small, self-contained, high value: **1** (rTorrent throttle method), **2**
(quick-xml `GeneralRef`), **3** (gluetun fallback port), **7** (tracker
fail-open), **9** (`sanitize_next`). Then **10** + the two RMW races (config,
vault) together, since they share a "serialise/validate before mutate" shape.
