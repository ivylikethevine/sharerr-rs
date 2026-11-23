# Roadmap

<!-- markdownlint-disable-file MD029 -->
<!-- a list after a heading goes on from the number before it -->

Where sharerr is going next. Nothing below is a release commitment; within a
section the ordering is by size, not a schedule. Items are numbered through
the whole file, so a number names one item, and a step of an item is a letter
on its number. An item is removed the moment it ships. An idea that gets
declined instead moves to
[`docs/COMPATIBILITY.md`](COMPATIBILITY.md#not-supported), with the reason
attached.

## Where it stands

As of 2026-11-23, branch `dev`, at v0.2.6.

- A simplification pass over `sharerr-store` is in the tree, uncommitted:
  `is_shared` uses the info-hash indexes (checked with `EXPLAIN QUERY PLAN`),
  an unchanged friend catalog takes no write, `touch_peer` takes the
  `last_seen_at` its caller read, vault writes from request handlers run off
  the runtime, `fd-lock` gave way to `std::fs::File::lock`, `Store::open`
  takes the retention limits, and `Store::close` replaced the public `pool()`.
  `cargo test --workspace --all-features` passes (1,782 tests); seven held-back
  items wait under On hold (14 to 20). `url` 2.5.9 in `Cargo.lock` clears
  RUSTSEC-2026-0592.
- Docs corrections are in the tree, uncommitted: the deploy README's proxy
  and header advice, the notification trigger counts, crate, client and
  stack counts, stale module paths, the vault fixture, the blocking-egress
  job list, the table list, `SHARERR_VERSION`, and `skip_checking`'s owning
  doc. `scripts/check.sh --fast` passes.
- The public lighthouse is live at `https://lighthouse.edenlabs.dev` (its
  health route answered on 2026-11-20) and is named in `docs/LIGHTHOUSE.md`
  and the README.
- The theme toggle's empty phone-width state is fixed (the toggle is exempt
  from the 32rem rule hiding bar glyphs); it has not been seen in a narrow
  preview.
- The dev profile builds dependencies with line tables only, and the coverage
  build runs without an incremental cache, to cut the size of `target/`.
  Neither the disk saving nor the effect on build and mutant times (5) is
  measured.
- Open questions: who may call the local hooks (6), and the held-back
  simplifications (10 to 20, 27 to 106, 115 to 125).

## Order of work

- The items under Now: 4, which publishes outside the repo and so starts on
  the maintainer's word, and 5 a phase at a time.
- 6, 10 to 20, 27 to 106 and 115 to 125 once their decisions are made, 27 to
  31 (small fixes for real misreports) first; 7 once Prowlarr allows it.
- The items under Later are not scheduled.

## Now

4. **Unraid and TrueNAS app listings.** Where much of the \*arr audience
   installs from; a Community Applications template and a TrueNAS app.
5. **Mutation survivors in `sharerr`, in phases.** `scripts/check.sh mutants`
   exists, and every other crate, plus `sharerr`'s `web/auth.rs` and
   `totp.rs`, has had its survivors worth killing killed (what is left and
   why: [`docs/TESTING.md`](TESTING.md#mutation-testing)). A `sharerr` mutant
   costs about 19 seconds at `-j 4` (16CPU/60GB), measured before the dev
   profile cut dependencies' debug info, because each one runs the crate's
   whole suite, so a 30-minute sitting covers about 95 mutants. The
   rest of the crate is 2,173 mutants, about 11.5 hours or 23 sittings, below
   by phase with counts from `cargo mutants --list -p sharerr` on 2026-11-06
   (they move with the code; re-list a phase before starting it, then split
   it by file into sittings of about 95). Each phase records its survivors in
   `docs/TESTING.md` and kills only the ones whose behaviour matters;
   `MUTANTS_ARGS` takes `-p` for a crate, `--file` for a file, and `--iterate`
   to re-test only the last run's survivors after adding tests.
   - 5a. Sync, 340 (4 sittings): `sync/mod.rs` (128), `sync/seed.rs` (92),
     `library.rs` (83), `tracker.rs` (37).
   - 5b. Items and settings, 350 (4): `web/items/mod.rs` (164),
     `web/settings/mod.rs` (134), `web/config_io/mod.rs` (52).
   - 5c. Friends, 415 (5): `web/topology/mod.rs` (203), `web/peers.rs` (121),
     `gossip.rs` (48), `pairing.rs` (33), `catalog.rs` (10).
   - 5d. Feeds and diagnostics, 350 (4): `torznab/mod.rs` (119),
     `web/diagnostics.rs` (79), `checks.rs` (58), `web/composition.rs` (55),
     `api.rs` (27), `jackett.rs` (12).
   - 5e. Commands and state, 435 (5): `commands/doctor/mod.rs` (91),
     `state.rs` (82), `commands/preview.rs` (71), `notify.rs` (61),
     `commands/serve.rs` (54), `metrics.rs` (43), `gluetun.rs` (33).
   - 5f. The rest, 283 (3): the 21 files under 35 mutants each.

## On hold

6. **IPv6 link-local and IPv4-mapped addresses in `is_private_ip`.** Waits on
   a decision. Its doc says link-local, but the IPv6 branch covers only
   loopback and `fc00::/7`, so `fe80::/10` reads as public; and it never
   canonicalises, so with `server.bind = "[::]:8477"` an IPv4 neighbour
   arrives as `::ffff:a.b.c.d` and reads as public too. Three callers would
   change: the gluetun refresh hook (`commands/serve.rs`'s `refuse_public`)
   would admit those neighbours, the advertised-host check in `web/settings`
   would refuse a typed `fe80::` address, and an announce from `fe80::` would
   take the peer's declared IP. Widens who may call the local hooks, so it
   needs a decision first; the code is a line plus a test case.
7. **A Prowlarr catalog listing.** Blocked upstream. A per-host definition
   friends install by hand exists ([`docs/prowlarr/`](prowlarr/README.md)); one
   listing in Prowlarr's own catalog, so a friend picks "sharerr" and types one
   key, needs Prowlarr to accept a custom base URL for Cardigann indexers. That
   doc says why.

Four simplifications held back from the repo-wide pass, since each changes
behaviour; each waits on a decision.

10. **Cache the notify webhook and heartbeat URLs** (`notify.rs`'s
    `stored_url`, and its caller in `commands/serve.rs`). Saves about 1,440
    Argon2 derivations a day, but a URL the `vault` CLI writes from another
    process would then take a restart to apply.
11. **A `light-dark()` palette** in `web/assets/style.css`. Removes about 60
    lines and narrows browser support to May 2024 and later.
12. **Drop `.wide .wide`** in `style.css`. Inert, but its comment defends it as
    documentation.
13. **Dispatch `preview` and `openapi` before the config loads** in
    `main.rs`. Drops the error log on a bad config.

Seven held back from the `sharerr-store` simplification pass: five change
behaviour, and two were judged not simpler than what is there. Each is TBD.

14. **Skip a seeding row's write when nothing changed.** TBD. `confirm_seeding`
    and `set_ratio` (`sharerr-store/src/db.rs`) rewrite every seeding row on
    every sync pass, one write transaction each, because `item_update!` always
    bumps `updated_at`; the caller (`sync/mod.rs`) already holds the stored
    fingerprint and ratio to compare. Skipping stops `updated_at` moving on an
    unchanged item.
15. **Entity-neutral store errors.** TBD. A duplicate or invalid friend name
    comes back as `UserExists` or `InvalidUser`, whose text says "user", so
    `web/peers.rs` rewrites the message. A `NameTaken`/`InvalidInput` pair, or
    `from_insert` taking the noun, fixes it at the source and changes the
    rendered text.
16. **One long-lived `Vault` in `ServeState`.** TBD. Every `Vault::open` runs
    the Argon2 derivation (about 16 ms and 19 MiB): each Diagnostics and
    Topology render, each probe's `WebState::secret`, each gossip round, each
    notify URL lookup. A cached handle refreshed with `reload()` before a read
    would miss a rotated master-key file and fail on a vault file replaced
    since it opened, where a fresh open succeeds. Would cover 10.
17. **One write transaction per newly shared item.** TBD. `upsert`,
    `set_seeding`, `set_info_hash_v2`, `set_item_origin` and `set_file_stamp`
    each commit on their own (`sync/mod.rs`'s share path). Folding them makes
    a failed origin or stamp write roll back the seeding record, where today
    the item stays seeding without it, and moves every `fail_after` count in
    `sync/tests.rs`.
18. **Doctor reuses the open store, and `MIGRATE_LOCK` goes.** TBD.
    `/api/v1/doctor` opens a fresh pool and re-runs migrations on every call;
    passing it the served `Store` stops that, but its check then no longer
    proves the file opens. The lock in `sharerr-store/src/db.rs` can go only
    once the syncer also shares `ServeState`'s store rather than opening its
    own pool.
19. **The audience filter in SQL.** TBD. `seeding_items_for`
    (`sharerr-store/src/peers.rs`) reads the whole `item_audience` table on
    every feed poll to filter in Rust. Filtering in the items query puts the
    audience rule in SQL beside `PeerLimits::admits`, two definitions where
    there is one, which `ScopeFilter`'s own doc warns against.
20. **One prune-to-newest helper.** TBD. `finish_run`, `record_swarm_sample`
    and `record_peer_endpoint` each write their own `DELETE ... NOT IN
(SELECT ... LIMIT ?)`, with different partitions and orderings and only
    the first treating `0` as unbounded. A shared helper would interpolate
    table, partition and order, which was judged no simpler than three
    explicit statements.

Held back from the second simplification pass, over the torrent-client, core
and support crates, `sharerr` and the docs. Each is TBD; "Changes behaviour"
marks the ones whose fix does.

### Closer to bugs

27. **Classify the client probe's `version()` errors.** TBD. Changes
    behaviour. With qBittorrent the probe never reports a rejected key or a
    dead host: `login()` does nothing there and every `version()` error reads
    as `Failed` (two `checks.rs` tests pin it). Dropping `login` also stops
    Transmission and rTorrent sending `version` twice.
28. **Validate the advertised host in core.** TBD. Changes behaviour. A host
    from the toml, an env var or an import skips the settings form's check,
    so `https://x` or `x/path` builds broken announce URLs while doctor says
    ok; `advertised_base` (`sharerr-core/src/endpoint.rs`) would check it.
29. **One gluetun "active" test.** TBD. Changes behaviour. With `[gluetun]`
    configured but disabled, doctor and Topology say the endpoint comes from
    gluetun while no poller runs; `is_active` everywhere, `GluetunTarget` in
    core.
30. **Topology dials the public feed address.** TBD. Changes behaviour. Its
    feed check ignores `ServeState::public_base_url`, so under gluetun it
    dials localhost (`web/topology/mod.rs`).
31. **`Config::sync_every` returns the floored interval.** TBD. Changes
    behaviour. The dashboard's next-sync estimate uses the interval before
    its floor (`web/mod.rs`).
32. **Lighthouse decoys shaped like real records.** TBD. Changes behaviour. A
    decoy carries one bare `ip:port`, a real record two or three `http://`
    endpoints, which tells them apart; the lighthouse crate would own the
    shape and build both.
33. **One vault closure for the settings save helper.** TBD. Changes
    behaviour. `save_notifications` stores the heartbeat URL before
    validating and opens the vault twice; `save_tracker` invalidates its
    cache before the commit (`web/settings/mod.rs`).
34. **`Config::validate` at load.** TBD. Changes behaviour. Absolute,
    non-overlapping library paths and a finite ratio are checked only by the
    form and the scanner, so a bad toml loads and fails later.
35. **Topology legend from `EdgeStyle::ALL`.** TBD. Changes behaviour. The
    legend has no row for the Restored line style.
36. **Record signing and admission in the lighthouse crate.** TBD. Signing is
    hand-written three times, gossip copies the time-skew check, and the
    server compares pubkeys case-insensitively where the client does not;
    `sign`, `admit` and `admit_as` there.

### Torrent-client crates

37. **qBittorrent returns `ClientError` directly.** TBD. Changes behaviour.
    Translating its own error type drops the key hint, turns decode errors
    into `Api` and doubles the unreachable message; `AuthRejected` would
    carry the hint.
38. **Shared `require_success` and `join_under`.** TBD. qBittorrent and the
    \*arr client each check responses and join base URLs their own way.
39. **Deluge on `RpcConnection`.** TBD. Deluge re-implements it; the
    connection's credentials become optional.
40. **A `refused()` helper.** TBD. Transmission, Deluge and rTorrent each
    build the "client refused the request" error by hand.
41. **`TorrentSummary::new` lowercases the hash.** TBD. Three adapters
    lowercase, qBittorrent does not, and `sync` and Topology lowercase again.
42. **One client-kind enum with capabilities.** TBD. `skip_checking` lives in
    three places; config forces it off for non-qBittorrent clients, so the
    adapters' warnings never fire and Deluge's `seed_mode` is dead.
    `ClientKind` duplicates `TorrentBackend`.
43. **Drop dead summary and add fields.** TBD. Changes behaviour. Nothing
    reads `TorrentSummary::category` or `tags`, `list(category)` serves one
    doctor count, and `AddRequest::stopped` is never true.
44. **Drop dead public items in the client crates.** TBD.
    `RpcConnection::endpoint`, `MAX_ERROR_BODY`, the two per-field
    `AddRequest` limit setters, `ArrClient::discover`, qBittorrent's public
    wire methods, rTorrent's `call_batch` and `as_str`.
45. **Default trait methods for adding trackers.** TBD. The read-modify-write
    is written twice, and `set_trackers(&[])` differs per client.
46. **The default `set_limits` logs once.** TBD. Four adapters each log
    "seeding limits applied" and re-apply limits after an add.
47. **Drop the "in {kind}" error prefix.** TBD. Changes behaviour. Nine
    callers in `sync` add it to errors that already name the client.
48. **Rename qBittorrent-named shared code.** TBD. `Seeder.qbit`,
    `ResolvedPaths::qbit`, `QbitOutcome` and `check_qbit` serve all four
    clients.
49. **One shared `Failure` type.** TBD. Two checks sort errors into
    unreachable, auth-rejected or other, and four renderers split them again.
50. **An `ensure_label` trait method.** TBD. Doctor builds a second
    qBittorrent client just to check categories.
51. **Deluge matches `RpcError` before text.** TBD. It reads an RPC error by
    searching the flattened text, and `apply_label`'s doc sits on
    `write_trackers`.
52. **One process-wide `reqwest::Client`.** TBD. Each constructor builds its
    own, so a page load or test button pays a TLS handshake and a rebuild
    drops warm pools; friend traffic has three more.
53. **Batched seeding limits.** TBD. Changes behaviour. Limits go out one
    torrent at a time, two POSTs each on qBittorrent: up to 4,000 requests at
    a quiet-hours edge.
54. **`find_existing` filters by `content_path` first.** TBD. Changes
    behaviour. It calls `files` on every sibling torrent under the save path,
    about n²/2 calls over a season (`sync/seed.rs`).
55. **Small copies in the client crates.** TBD. `json!` deep-copies request
    bodies, qBittorrent decodes to text before parsing, rTorrent allocates a
    string per XML value, `kib*1024` is written twice, base64 per adapter.
56. **The \*arr walks set the source once.** TBD. Each stamps its own source
    and clones the tag list per file, and the Whisparr and Readarr probes run
    one after the other.
57. **Deluge remembers created labels.** TBD. Changes behaviour. It sends
    `label.add` on every add.
58. **rTorrent uses the shared `content_path`.** TBD. Changes behaviour. It
    re-implements it.
59. **Trim client-crate doc comments.** TBD. They narrate refactor history,
    and some are already wrong (`sharerr-client/src/lib.rs`).
60. **Shared client-crate test helpers.** TBD. Closed-port URLs ×3
    (`closed_url` exists), qBittorrent builders ×4, rTorrent XML fixtures, a
    Transmission matcher ×9, a `TorrentSummary` literal ×3.

### `sharerr`: sync and feeds

61. **Parse a `.torrent` once per add.** TBD. Changes behaviour. The seed
    path parses it up to four times, `with_upstream` re-implements
    `rewrite_announce`, and the factory sets announce fields three ways;
    `Torrent::set_announce` and `AnnounceSet::followed_by`.
62. **`share()` without the six-element tuple.** TBD. A 42-line match unpacks
    it, and the item's path is resolved twice.
63. **`write_cached` beside `read_cached`.** TBD. The `.torrent` cache is
    written three ways.
64. **One "remove our own torrent".** TBD. `unshare_one` and
    `prepare_rebuild` each write it.
65. **Feed rendering without throwaway strings.** TBD. It allocates per item,
    and jackett and torznab each list the ten media fields.
66. **Serialise the catalog once.** TBD. It is serialised twice and clones
    fields it could move (`catalog.rs`).
67. **List each library root once per scan.** TBD. Each is listed twice, and
    path suggestion indexes every file under `/media`.
68. **Use the torrent crate's helpers.** TBD. `sharerr` re-implements
    `announce_set_for`, `info_hash_from_hex` (twice) and `torrent_file_name`.

### `sharerr`: web

69. **Settings client sections from `TorrentBackend::ALL`.** TBD.
    `settings.html` handles the four clients as five string branches over 12
    page fields, and `save_rpc_client`'s match restates the config.
70. **One builder for the qBittorrent and tracker fields.** TBD. Settings and
    the wizard each build them, and the wizard copies the tracker form.
71. **Handlers take the user `require_auth` found.** TBD. Four look it up
    again, and `login_submit` copies `password_ok`.
72. **One `parse_secs` and an `Edit::uint`.** TBD. Changes behaviour. Four
    hand-written seconds parsers and four `i64` conversions.
73. **Join independent awaits.** TBD. The items page and item detail (with a
    blocking `exists()`) run them in series, and Topology waits for its
    source probes before the client probe.
74. **Read config fields without cloning `Config`.** TBD. Five sites clone it
    for one or two fields, and the env-override map is cloned every render.
75. **Template copies.** TBD. The gluetun panel hand-writes field macros,
    `peers.html` copies its scope select ×3 and copy button ×3, flash
    messages follow two conventions, and Topology's lanes and accents live in
    three places.
76. **`save_libraries` and `save_paths` without tuples.** TBD. Each clones
    every form cell into one.
77. **One wizard handler on `Path<WizardStep>`.** TBD. Changes behaviour.
    Five one-line wrappers, the query struct three times, and
    `generate_secret` takes a path segment that only accepts "tracker".

### `sharerr`: friends, state and ops

78. **The last vault writes off the runtime.** TBD. `try_cached_from_vault`'s
    first-use load, `import_one_peer` and `apply_invite` still write on it;
    `with_vault`, and one `store_outbound` shared with `set_gossip`.
79. **The lighthouse loop opens the vault only to look something up.** TBD.
    Changes behaviour. It opens it every pass, 96 a day, though a pass
    usually looks up nothing.
80. **Gossip asks only about active friends.** TBD. Changes behaviour. Each
    exchange asks about revoked friends and rebuilds the same request per
    friend.
81. **`Caller` keeps its friend row.** TBD. It drops the row it loaded, and
    `pair` reads it again.
82. **One friends-to-endpoints step.** TBD. Topology and the Friends page
    write it three times.
83. **Derive `PeerRow::setup_done`.** TBD. It is stored though derivable, and
    `friend_tag` holds a placeholder.
84. **Lighthouse route paths from the crate.** TBD. The client types them by
    hand; export them and test them against `api_spec()`.
85. **A `join_path_str` for string bases.** TBD. Three call sites trim a
    trailing slash by hand.
86. **Doctor on `checks::snapshot`.** TBD. Changes behaviour. Doctor runs the
    probes one at a time, and `/api/v1/doctor` wraps it in `spawn_blocking`
    plus `block_on`.
87. **One torrent-client probe.** TBD. Diagnostics, probe and Topology each
    write it.
88. **One map for the single-secret vault caches.** TBD. Changes behaviour.
    Six of `VaultCaches`' fields are the same cache.
89. **A `PrivateCaller` extractor.** TBD. Each local hook repeats the
    private-address check, and `ApiAuth` and `MetricsAuth` are one extractor
    twice.
90. **Small `sharerr` copies.** TBD. `unavailable(&format!)` allocates twice
    at six sites, `commands/sync.rs` hardcodes the gluetun key name, and
    doctor restates the qBittorrent path-prefix default.
91. **Shared `sharerr` test helpers.** TBD. `with_open_vault` is rebuilt by
    hand about 17 times; also the `Seeder` literal ×3, the gluetun stub ×2
    and about nine hand JSON mocks, signing helpers ×4, client configs ×3,
    and the request recipe ×6.

### Core and support crates

92. **One `ClientSecret` enum.** TBD. `TorrentClientConfig` holds two
    credential `Option`s with exactly one set, so callers carry arms that
    never run.
93. **One torrent request type.** TBD. `TorrentRequest`, `SidecarRequest` and
    a stateless `TorrentFactory` exist for one decision; an optional
    `sidecars` field replaces them.
94. **A sync `ensure_ready`.** TBD. Changes behaviour. It duplicates
    `announce_set` and nothing in the trait awaits; `async-trait` leaves the
    torrent crate.
95. **`str_enum!` emits serde.** TBD. Changes behaviour. Eight types spell
    their wire strings twice, in `rename_all` and as literals.
96. **A function column in `ARR_WIRING`.** TBD. `Config::service` and a test
    keep the two in step.
97. **Title code.** TBD. `ParsedTitle` duplicates `MediaSpec`, `library.rs`
    keeps its own release-token list, `parse` has two near-identical loops,
    and `resolve` takes six positional arguments.
98. **An `InfoHash` newtype.** TBD. Changes behaviour. Hashes are raw bytes in
    one API and hex in the other, with six conversion sites; `parse_scrape`
    replaces the public `parse_query`.
99. **Small core copies.** TBD. `Swarms::snapshot` and `snapshots`,
    `hash_key` in the store and the lighthouse, two percent-encoders, the
    lighthouse's copy of the log setup, and `secret_keys!` re-implementing
    `config_paths!`.

### Core and support crates, continued

100. **sharerr-core's dependencies and `SceneMeta::new`.** TBD. Changes
     behaviour. `time` is unused, `serde_json` is test-only, and
     `SceneMeta::new` accepts 2021-02-30.
101. **`catch_unwind` inside sharerr-probe.** TBD. It promises panic safety
     that only its caller provides.
102. **Overlap reading and hashing.** TBD. Hashing takes turns with reads, and
     hybrid builds run SHA-1 then SHA-256 on one core (`factory.rs`).
103. **Fewer `.torrent` round trips.** TBD. Each tokened download decodes and
     re-encodes the file, each build serialises the info dictionary three
     times, and bencode allocates a string per integer.
104. **Swarm tracker locking and sweeps.** TBD. A tokio `RwLock` where a std
     `Mutex` fits, stale swarms swept only by an announce, and announce URLs
     built as strings then re-parsed.
105. **The lighthouse refuses before it verifies.** TBD. Changes behaviour. It
     checks a signature before its cheap table-full refusal, holds the write
     lock to refuse, and clones and re-serialises every lookup.
106. **testkit without per-file fsync.** TBD. Changes behaviour.
     `write_media_file` fsyncs every fixture, and `seed-arr` commits each
     insert on its own.

### Docs

115. **CLAUDE.md keeps only the agent traps.** TBD. It restates CONTRIBUTING,
     TESTING, RELEASING and CODEQL: the verification loop, test tiers, CI
     workflow lists, release gates, "the version is the tag" twice, and the
     CodeQL policy.
116. **TESTING owns the fixtures.** TBD. It defers to CLAUDE.md; the fixture
     list and both `Jail` rules move there.
117. **One home for the release gates.** TBD. Six docs restate them, and the
     release-note mechanics are split between CONTRIBUTING and RELEASING.
118. **CODEQL.md owns the CodeQL policy.** TBD. It appears four times.
119. **One explanation of private flags and magnets.** TBD. README, SETTINGS
     and COMPATIBILITY each give it, and README calls them "settled roadmap
     questions".
120. **One home each for the test tiers and the MSRV.** TBD. Each appears
     four times.
121. **One write-up of the gluetun key and hooks.** TBD. It is written four
     times, and each deploy layout README restates the shared needs it links
     to.
122. **GOVERNANCE, ALTERNATIVES and OPENSSF ownership.** TBD. GOVERNANCE
     copies CONTRIBUTING, ALTERNATIVES.md is linked only from the index and
     restates COMPATIBILITY, and OPENSSF cites the wrong owning docs.
123. **Cut narrated history from the docs.** TBD. RELEASING, CLAUDE.md,
     SUPPORT, GOVERNANCE, `docker/README.md`, COMPATIBILITY and README.
124. **Trim SETTINGS' `[gossip]`.** TBD. Five sentences of reasoning that
     `GossipConfig`'s doc holds.
125. **OPENSSF-IMPROVEMENTS without its audit narration.** TBD. "As of this
     pass" and "verified today" run through it.

## Later

8. **Season packs and albums as one torrent.** One media file per torrent means
   a friend's season search returns single episodes; a multi-file torrent over
   the existing folder still moves nothing, and `seeding.sidecars` already builds
   one. `seeding.hybrid` and joining a friend's swarm take single-file torrents
   only, so each needs its multi-file case too.
9. **Request flow.** Discovery is one-way today. An inbound request queue
   with an approve step is the other half of the original idea. Seerr (the
   merged Overseerr and Jellyseerr) is the request app most friends already
   run, so its request model is the one to fit.

## Decisions

A declined idea is recorded in
[`docs/COMPATIBILITY.md`](COMPATIBILITY.md#not-supported) with its reason, and
none is kept here.

## Done

Nothing is listed: an item is removed the moment it ships.
