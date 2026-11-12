# Resuming the repo-wide simplification

Paused 2026-11-01 because every `cargo` command needed a manual approval.

**The ask:** `/simplify` over the entire repo, then "do all suggested
simplifications". Ten read-only reviewers reported about 147 findings, deduplicated
to 139 items in `docs/plans/repo-simplification.md`. Quality only: no behaviour
change, no new precondition. Anything that would change behaviour is held back for
the user, not applied.

## State of the tree

- Branch `dev` at `a23f54e`, level with `origin/dev`. **Nothing is committed**; all
  of it is in the working tree (88 tracked files, +2409/-3163, plus the untracked
  plan and this file). Do not commit or push without being asked.
- `docs/plans/repo-simplification.md` is the checklist. A `[x]` means applied and
  building.
- Clippy passed on the main crate at the last check. The last full test run predates
  the web changes listed under "First steps".
- `local/openapi-new.json` is a scratch copy, gitignored; delete it.

## Next

Only the plan's unticked items are left: test helpers, two preview fixtures, and CI
and docs tidying. Run `cargo clippy --workspace --all-targets -- -D warnings` and
`cargo test --workspace -q --no-fail-fast` after each group.

For anything touching templates or CSS, build the commit before these edits in a
`local/` worktree and compare with `scripts/scratch/preview_html_diff.sh` and
`scripts/scratch/preview_visual_diff.sh`. The invite blob on `/peers` differs on every run.

## The `cargo` setup

`.claude/settings.json` carries `excludedCommands: ["cargo *"]`, so a plain `cargo`
command runs without a prompt. A redirected one (`cargo test > file`) falls back into
the sandbox: use `-q` instead. That settings change is in the tree and should not be
swept into a simplification commit unnoticed: it may belong in the untracked
`.claude/settings.local.json`.

Known sandbox artefacts:

- Inside the sandbox a wiremock test can fail with "did not receive any request" in a
  full run and pass alone. A `<sandbox_violations>` line naming `127.0.0.1:<port>`
  confirms it. Rerun that test alone before believing it.
- `scripts/check.sh terraform zizmor` cannot run inside the sandbox; hand that command
  to the user.

## Traps found while applying

- **Read a failing `the_committed_document_is_current` before regenerating.** It caught
  a real bug: a constant inserted between a route's doc comment and `#[utoipa::path]`
  removed the route's OpenAPI summary. The document should stay byte-identical
  throughout this work.
- Never insert an item between a doc comment and what it documents; multi-line perl
  edits did this twice.
- `Store::fail_after(n)` counts queries, so a change to the query count shifts these
  tests' budgets. Check that the assertion still means the same thing, then adjust.
- A handler returning `Result<_, Response>` fails clippy's `result_large_err`; use a
  small error type (`Unavailable` in `api.rs`) or a boxed response.
- Use `git grep -nE`, or quote globs: the shell is zsh, and `--include=*.rs` unquoted
  fails. `\|` in plain `git grep` matched nothing.
- `git grep` prints a harmless `.gitmodules: Permission denied` warning.

## Held back for the user's decision

Each changes behaviour, so none is applied. None has been answered yet.

1. Cache the notification webhook and heartbeat URLs (`notify.rs`). Saves about 1,440
   Argon2 runs a day, but a URL set through the `vault` CLI would need a restart.
2. `light-dark()` palette (`style.css:104-176`). Removes about 60 lines, drops browsers
   older than May 2024.
3. Delete the inert `.wide .wide` CSS rule (`style.css:895`), which its comment defends
   as documentation.
4. Dispatch `preview` and `openapi` before config load (`main.rs:50`), which drops the
   error log on a bad config.

Possible bug, outside this work and unconfirmed: at 32rem or below,
`style.css:1624` hides `.bar nav button svg`, including the theme toggle's system
glyph, so the toggle may look empty on a phone.

## Decisions already made

- Early returns use the `ok_or_return!` macro rather than an `Early` newtype, since an
  extractor would change which response wins.
- `refuse_public` is a plain function rather than a `PrivateCaller` extractor, because
  about six tests call those handlers directly.
- `sharerr-deluge` keeps its own connection; only Transmission and rTorrent use
  `RpcConnection`.
- `peer_gossip_record` is not converted to `query_scalar`: it swallows decode errors
  and converting it would change behaviour.
- `persist`'s `create_dir_all` in the vault stays; a direct-call test pins it.
- Log-only change accepted: rTorrent `add` now also emits `apply_limits`' info line.
- `metrics::render` skips the whole last-run block when `finished_at` is `None`, which
  `gather` already filters out.

## Notes for the open items

Line numbers are from before these edits; grep for the names.

**Doctor async:** make `doctor::checks` natively async using `secrets::open_vault_async`,
`checks::scan_libraries` and `spawn_blocking` around `check_paths_of`, then call
`json_report` directly from `api.rs`. Drop the item and report it if the future is not
`Send`. A panic now returns 500 through the `JoinError` arm; keep that handling.

**Doctor tests:** tests at `:1243` and `:1491` are the same test; `:1277` and `:1308`
are subsumed by `:1524`. Move helpers to the top, add `qbit_check(&config, &vault, fix)`
as a twin of `arr_check`, and make `qbit_config` take a `&Url`.

**`with_open_vault`:** one `state::fixtures::with_open_vault(config, seed, body)` plus
`with_no_master_key` for the `clear_env` cases. `peers.rs`'s
`export_degrades_gracefully_without_an_openable_vault` changes the environment partway,
so it stays hand-written.

**`Identity::from_seed`:** `sharerr-testkit` is a dependency of `sharerr` but not of
`sharerr-lighthouse`; leave lighthouse's own copy.

**`test_support::shared_item` and `peer`:** `sharerr-testkit` has no `sharerr-core`
dependency, so these go in the `sharerr` crate's `test_support`.

**Testkit mounts:** only stubs with no `.expect` or body matcher fit `mount_json`,
`mount_text` and `mount_ok`; check each.

**`internal_docs!`:** keep full URL literals. `.github/link-check-paths.txt` has lychee
sweep `web/docs.rs`, so building URLs with `concat!` would break that sweep. Also fix
the false `REPO` comment.

**`import_config`:** build a `PendingConfig` and call `commit_config(&state, "config",
None, pending)`, then run `import_peers` if `response.status().is_redirection()`. Keep
the import's own log line.

**`Edit` helpers:** leave the differing `i64::try_from(..).unwrap_or(60/900/604_800/MAX)`
fallbacks in settings alone; unifying them changes behaviour.

**`ItemFacts`:** the templates and `preview.rs`'s five `ItemRow` literals change to
`.facts.x`.

**`preview` mock route:** keep `page`'s body inside `mock`; `render` would turn a
template failure into a 500. `title_case` and `arr_section` need wider visibility.

**Templates:** the page-width rules in `style.css` stay untouched. Widen the lock-key
scanner (`templates/mod.rs:1398`) when adding `ui::text_field`, or it stops seeing those
keys. Keep `.topo-icon` out of the stroke selector, because CSS `stroke` would override
its attribute. Keep `class="invite"`: `peers.rs` uses it as a test marker. Check each
page with the `/ui-check` skill.

**CI, scripts, docker:**

- Only the repo's own CI files are in scope: `ci`, `coverage`, `docker`, `docker-image`,
  `integration` and `prepare` workflows, `check_tool_versions.local.sh`,
  `validate_compose.sh`, `actions/rust-toolchain`. Everything else under `.github` is
  synced from another repo; never edit those copies.
- No git hooks, no new step or gate on a release, no bot commits, no cross-repo `uses:`.
  Actions stay SHA-pinned. A tag gate checks only the tag.
- Before deleting `.tool-versions`, check whether anything local reads it; it pins yq and
  terraform, and terraform has drifted from `tools.txt`.
- Hadolint: `found` means non-empty output there but non-zero exit for semver; confirm
  they match before sharing `advisory-check`.
- YAML anchors are new to these workflows. Run `scripts/check.sh zizmor actionlint
workflows`; `lint_workflows.sh:92` reads the first step through yq and must follow the
  alias.
- `.dockerignore`: confirm no crate embeds `docs/`; only a test-only read at
  `openapi.rs:435` was found.
- Docker runners cannot run here. Say so: only shellcheck and `validate_compose.sh`
  verify them.

## Finishing

1. Run `scripts/check.sh` and read its `skipped:` line. Hand the user anything the
   sandbox blocks.
2. Grep the diff outside `docs/plans/` for `§|\bS[0-9]\b|\bC[0-9]\b|the plan`.
3. Delete `docs/plans/repo-simplification.md` once every item is applied or recorded,
   and delete this file.
4. End with the end-of-task summary: changes, what was verified and not, suggested commit
   boundaries by group, pending GitHub actions, scripts added to `scripts/scratch/` (none so far),
   agents and their tokens, and assumptions.

**Agents used:** 10 read-only reviewers, all finished. Tokens: core and store 269,820;
torrent and client 232,807; arr, probe, lighthouse, testkit 248,036; sync, tracker,
torznab 197,410; gossip, notify, state 278,686; commands 228,590; web auth 223,131; web
settings 199,400; templates 221,227; CI, scripts, docker 244,361. The ask capped agents
at 10, so the rest is done in the main session unless the user says otherwise.

**Assumptions so far:** the log-only rTorrent change and the `metrics::render` block
above; the `Unavailable` error type in `api.rs`.
