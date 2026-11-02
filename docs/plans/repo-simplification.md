# Repo-wide simplification

Status: in progress. Tick an item when applied and building; delete this file
once every item is applied or recorded as held back.

Quality only: no behaviour change, no new precondition. Each group ends with
`cargo clippy` and the touched crates' tests; the whole list ends with
`scripts/check.sh`. Paths are from the repo root; `S/` is `crates/sharerr/src/`.

## Held back (changes behaviour; needs a decision)

- notify webhook and heartbeat URL cache (`S/notify.rs:433`, `:116`,
  `S/commands/serve.rs:587`): saves about 1,440 Argon2 derivations a day, but a
  URL written by the `vault` CLI from another process would need a restart.
- `light-dark()` palette (`S/web/assets/style.css:104-176`): removes about 60
  lines, narrows browser support to May 2024 and later.
- `.wide .wide` (`style.css:895`): inert, but its comment defends it as
  documentation.
- `S/main.rs:50` dispatching `preview` and `openapi` before config load: drops
  the error log on a bad config.

## Testkit, arr, probe, lighthouse

- [x] lighthouse `report` and `lookup` to plain `fn`, drop allows and awaits
- [x] probe: `.inspect_err(..).ok()?` for four matches, inline `open_logged`
- [x] arr: `try_join!` as tail expression (sonarr `:37`, readarr `:86`, lidarr `:88`)
- [x] testkit `net::closed_url()`; use at closed-port sites
- [x] tests: `mock::base_url` at 31 sites (doctor tests, gluetun, notify, checks,
      lighthouse_client)
- [x] tests: `mount_json` and `mount_text` at plain stubs (qbit tests, doctor
      tests, discovery); gluetun's stubs all carry a matcher or status
- [x] tests: `mock::body_text` five copies in `S/sync/tests.rs`; `QBIT_API_KEY`
      and `ARR_API_KEY` literals in `S/checks.rs`, doctor tests
- [x] testkit: delete `movie_json`; `series_json`, `episode_json` free functions;
      use them and `system_status_json` in `crates/sharerr-arr/tests/discovery.rs`
- [ ] discovery tests: local `discover`, `mount_series_files`, `by_file`
- [x] `crates/sharerr/tests/cli.rs`: `closed_port` for `free_port`, test calls
      `stop_gracefully`, merge `impl Serving`, `write_sync_config`, `MASTER_KEY`,
      `malformed_config`
- [x] secrets: `rpc_credentials` calls `fresh_password`; shared `test_key_bytes`,
      `random_secret` (gossip, lighthouse_client, openapi tests)
- [x] `seed-arr.rs`: `seed_one`, `ensure_root`, prefix constants; `library.rs`
      `media_file`
- [x] lighthouse tests call `post_report`; drop redundant `TcpStream` connects
      (`tests/binary.rs:95`, `crates/sharerr/tests/cli.rs:364`)
- [x] probe test `extension_matching_ignores_case` asserts `Some` on `Film.MKV`;
      move stale docs

## Core and store

- [x] `PeerScope::admits` replaces `scope_admits` (`S/web/items/mod.rs:297`);
      `seeding_summaries` reads once and filters; test Rust and SQL agree
- [x] `item_update!` macro for the `update_item` tail (`db.rs:368`); returns `Result<()>`
- [x] `Store::changed(query) -> Result<bool>` at 9 methods
- [x] vault: `read_if_present`, `io_err`, `locked`
- [x] `touch_peer` branches on `seen`, `TOUCH_WINDOW_SECS` (`peers.rs:418`)
- [x] `MIGRATE_LOCK` only in `open`; doc names the real racers (`db.rs:140`, `:25`)
- [x] `record_peer_endpoint` one bind chain (`endpoints.rs:131`)
- [x] `placeholders(n)`, `fetch_by_ids`, import `HashMap` (`endpoints.rs:206`, `db.rs:754`)
- [x] `scene_token` table helper (`crates/sharerr-core/src/model.rs:222`, `:267`)
- [x] `query_scalar` and `query_as` for single-value reads; not `peer_gossip_record`
- [x] `RetentionCell` to `RwLock<Retention>` (`retention.rs:46`)
- [x] `sharerr_core::saturating_i64` at 10 sites
- [x] `StoreError::from_insert`; `pub(crate) type Result<T>` in `db.rs`
- [x] misplaced and stale docs: `config.rs:979`, `peers.rs:257`, `:639`,
      `db.rs:1284`, `:372`, `endpoints.rs:90`, `:696`; one-liners: `endpoint.rs:81`,
      `db.rs:726`, `users.rs:290`, `MAX_SAMPLES`, `MAX_HISTORY`, `literal_url`
- [ ] endpoints tests `sight`, `sightings`; db tests `seeded`; `upgrade.rs` `run`
- [x] `Store::active_peers()` for about nine "not revoked" filters;
      `diagnostics::active_friends` body goes

## Torrent and client crates

- [x] `sharerr_client::RpcConnection` for transmission, rtorrent, part of deluge
- [x] rtorrent `add` calls `set_limits`; inline `warn_no_ratio_limit`
- [x] qbit: delete inherent `login`; `lift` helper for `translate`
- [x] `sharerr_client::decode_json`
- [x] drop unused `tokio` normal dep (transmission, deluge, qbit)
- [x] `Torrent::content_length`, `Retargeted::torrent()`, drop `length` field
- [x] `percent_encoding` for the announce decoder (`announce.rs:565-621`)
- [x] rtorrent `call_batch(method, &[[Param; N]]) -> Result<()>`
- [x] transmission `ListedTorrent::into_summary`
- [x] `sharerr_client::single_tag`; deluge `write_trackers`
- [x] qbit: drop `tag` parameter and dead guard (`torrents.rs:28`, `:255`)
- [x] `AnnounceSet` private fields and `primary()`; collect at `tracker.rs:134`
- [x] `hash_pieces` reads `stream` directly (`factory.rs:275`)
- [ ] adapter tests: `respond_all`, testkit mounts, sharerr-client default-method
      tests, `body_json`, `calls` in testkit, `urls`
- [x] misplaced docs: sharerr-client's `reported` test helper, `sharerr-qbit/src/adapter.rs:22`

## Gossip, notify, state

- [x] delete `AlwaysEmptyQbit` for `StubClient` (`S/test_support.rs:64`, `S/state.rs:942`)
- [x] `state::fixtures::with_open_vault` behind the five helpers (metrics, tracker,
      api, probe, settings tests). Not applied: `with_no_master_key` and the inline
      `Jail` copies in peers, notify, gluetun and serve, about 40 closures that each
      seed and assert differently, so one fixture would not shorten them.
- [x] metrics `Family` with `.sample`, `gauge`, one run block (`S/metrics.rs:169`)
- [x] gluetun: drop `last_error` copy, `poll_once(state, target, http)`
- [x] one tracker token cache; delete test-only accessors (`S/state.rs:93`, `:501`)
- [x] `VaultCaches` swapped whole in `invalidate` (`S/state.rs:680`)
- [x] `QuietNotified` local to `quiet_peers_loop` (`S/notify.rs:62`)
- [x] `MetricsSnapshot::zeroed()`, drop dead `items_by_state` input
- [x] notify `stored_url` for the three vault-url reads; dead `secs.max(1)`
- [x] gossip `ingest` uses `peer_gossip_records`, local `signed_at` map (`S/gossip.rs:275`)
- [x] lighthouse_client stores `LighthouseSnapshot` directly (`:79`, `:153`)
- [x] Not applied: `Identity::from_seed`, `test_identity`. `test_key_bytes` already
      removed the duplication; only `gossip.rs` builds a test `Identity`.
- [x] gossip one `filter_map`; `self_record` closure (`:494`, `:138`)
- [x] small: `is_hex_pubkey` shared with lighthouse `valid_key_hash`; inline
      `apply_lookup`; totp `code_for`; `SystemStats.disk`; `AtomicU32` counters;
      drop `chain` alias

## Sync, tracker, torznab

- [x] drop `KnownTorrents`, `normalize_path`; compare `Path`s (`S/sync/seed.rs:632`)
- [x] `read_cached`, `Retargeted` accessors, flatten `adopt` (`seed.rs:236`)
- [x] jackett `rename_all = "PascalCase"`, keep exceptions (`S/jackett.rs:286`)
- [x] jackett move fields out of `Matched`; `QueriedIndexer::sharerr`
- [x] consts for "Always 200" and the `apikey` description (`S/tracker.rs:204`)
- [x] `PassContext` holding per-pass values (`S/sync/mod.rs:646`, `:879`)
- [x] extract `confirm_live` from `share` (`S/sync/mod.rs:843`)
- [x] one `SeedOutcome` match; inline `missing` (`S/sync/mod.rs:1044`)
- [x] `Seeder.seeding: SeedingConfig` (`S/sync/mod.rs:424`)
- [x] `LimitsMemo` under one mutex (`S/sync/mod.rs:182`)
- [ ] seed tests `built`, `paths_for`, `announce`; sync tests shared `Seeder`
- [x] tracker tests `SharedItem { .., ..shared_item() }`
- [x] torznab `groups.contains`, `is_illegal_control` (`S/torznab/mod.rs:141`, `:169`)
- [x] small: `library.rs:398`, `:387`, `pathsuggest.rs:120`, `tracker.rs:590`,
      `sharerr_torrent::torrent_file_name`

## Commands, CLI, API

- [x] api handlers return `Result<Json<T>, Response>`; `with_config` (`S/api.rs:108`, `:238`)
- [ ] doctor tests: helpers on top, `qbit_check` (the three redundant tests are deleted)
- [x] Not applied: doctor natively async. Keeping the 500 on a panic still needs a
      spawn, so `S/api.rs:289` barely shrinks while async spreads through the vault,
      library and path checks and their tests.
- [x] `check_arr`, `check_library`, `fix_missing_tag` shape (`doctor/mod.rs:513`)
- [x] doctor gluetun loop, drop the dead arm (`doctor/mod.rs:226`, `:877`)
- [x] `checks(config, error, &DoctorArgs)` (`doctor/mod.rs:157`)
- [x] `check_vault` uses `credential`; no `noun` compare; `credential_name` (`:326`, `:678`)
- [ ] serve tests `drive_background_until`
- [x] preview `mock` route helper, keeping `page`'s inline error (`preview.rs:44`)
- [ ] preview `SETUP_STEPS`, `title_case`, `arr_section`, `scope_options`
- [x] openapi one `SecuritySchemes`, inline `doc_to_json`, `random_bytes`, doc count
- [x] serve mount decision once; `degraded` bool (`serve.rs:108`, `:159`, `:61`)
- [x] `PrivateCaller` extractor (`serve.rs:408`, `:449`, `:494`)
- [x] `Config::advertised_base()`, `checks::dial_target`

## Web auth and peers

- [x] `ok_or_return!` and `unavailable` across web handlers (27 sites)
- [x] peers `rows`, no parallel vectors, tag passed to `row` (`peers.rs:638`)
- [x] `own_invite -> String`, `invite: String`, merged tail in `pair`
- [x] drop `build`'s `revealed` parameter
- [x] `internal_docs!` macro for `S/web/docs.rs`, keeping full URL literals
- [x] `bytes_or_empty` at `S/web/mod.rs:409`, `:475`, `peers.rs:715`
- [x] `password_ok` in `auth.rs`, shared; drop form clones
- [x] `vault_unavailable` derived (`peers.rs:554`)
- [x] `Config::sync_every`, `with_config` instead of whole-`Config` clones; `glance` comment
- [x] drop the outer `directory` check (`S/web/probe.rs:44`)
- [x] `mint_peer` one `rejected` call
- [x] `pending_cookie` beside `PENDING_TTL`; `base_cookie`
- [x] auth tests: `location`, `setup_form`, `login_form`, `body_of`, closed-store state
- [x] `test_support::peer`, `test_support::shared_item`, `seed_sam`

## Web settings, items, topology

- [x] `import_config` through `commit_config` (`settings/mod.rs:1052`)
- [x] `TokenToggleForm`, `save_token_toggle`
- [x] `ItemContext::gather`, `ItemFacts` embedded in both page models
- [x] `load_item` for four items handlers
- [x] `Edit::or_unset`, `Edit::url_or_unset`, `parse_optional`, one seeding parse
- [x] drop duplicate filter fields; `FilterState::query`, `FilterOption::named`
- [x] `endpoint_status(&serve, config, target)`, `gluetun_section`
- [x] `ClientCheck::healthy()`, `Default`
- [x] `Channel(Option<Seen>)`, drop `EdgeStyle::None`
- [x] diagnostics `bar_height`
- [x] composition `&'static str` labels, `Bucket`
- [x] `Swarms::counts`, `Swarms::snapshot`, `Store::share_end`, one-item audience read
- [x] topology `mask_address` doc, "three" channels, `placed` only

## Templates and assets

- [x] `ui::secret_field` (14 sites)
- [x] shared `arr_form`, `path_map_form`, `qbit_form`; lock-key test reads the new file
- [x] `ui::text_field`, `ui::check_field`; widen the lock-key scanner
- [x] stroke-glyph selector list; `.bar nav :is(a, button)`
- [x] dead CSS except `.wide .wide` and `--sp-12`
- [x] pill and status selector lists
- [x] topology `mismatch_table`; items `filter_select`
- [x] `flash_error`, `token_badge`, `or_dash`, `step_mark`
- [x] one `.hint`, one `.foot`, one `32rem` block
- [x] `--seg` for composition colours
- [x] `ui::test_button`; drop default `hx-swap`
- [x] `id_prefix`, `client_section`, dead `describe` checks, thin wrappers, aliases,
      classes with no reference

## CI, scripts, docker, docs

- [x] Not applied: `scripts/lib_docker_tests.sh`, `poll_until`, one client `case`,
      `check_client_announces`, `check_indexer`. Only `remove_state` is identical across
      the runners, and nothing here can run them.
- [ ] `docker/compose.stack-base.yml` with `extends`; `validate_compose.sh` skip
- [ ] docs: TESTING owns tiers, docker/README owns stacks (the `--test e2e` commands
      are fixed)
- [ ] `check_hadolint`; `.github/actions/advisory-check`
- [ ] path-filter list once; YAML anchors in `docker.yml` for DNS step and egress
- [x] `.dockerignore` adds docs, scripts, scratch, local, coverage-html, editor dirs
- [ ] workflow and Dockerfile headers cut to the why; stale statements fixed
- [x] Not applied: delete `.tool-versions`. Nothing in the repo reads it, but mise
      does, on any machine that uses it.
- [x] mesh `x-node`, `x-lsio-env`, each checked with a `docker compose config` diff;
      not applied: one config template, which needs a run of the mesh script
- [x] `_cov_report`; not applied: `check_coverage-lines`, `--coverage-floor`, which
      would mean editing two workflows to save one `awk` and one `sed`
- [ ] docs point at the owner for groups, roster, floor, defaults
- [ ] mesh unused-variable line, `docker compose port`, integration jq filter
