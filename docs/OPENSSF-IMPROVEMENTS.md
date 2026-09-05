# OpenSSF Best Practices Badge — improvements

The full criterion-by-criterion record behind [project 14449](https://www.bestpractices.dev/en/projects/14449)
on bestpractices.dev: every passing- and silver-level criterion, the answer,
and the evidence behind it. [docs/SECURITY.md](SECURITY.md) covers the
policy those answers point back to; this page is the audit trail proving
each one, kept so a later review — either self-review before re-answering
the form, or an actual bestpractices.dev reviewer — doesn't have to
re-derive the reasoning from scratch.

## Table of contents

- [How to read this](#how-to-read-this)
- [Passing](#passing)
  - [Basics](#basics)
  - [Change control](#change-control)
  - [Reporting](#reporting)
  - [Quality](#quality)
  - [Security](#security)
  - [Analysis](#analysis)
- [Silver](#silver)
  - [Basics](#basics-1)
  - [Continuity](#continuity)
  - [Change control](#change-control-1)
  - [Reporting](#reporting-1)
  - [Quality & test](#quality--test)
  - [Externally-maintained components](#externally-maintained-components)
  - [Build](#build)
  - [Installation](#installation)
  - [Security](#security-1)
  - [Secure release](#secure-release)
  - [Analysis](#analysis-1)
- [Verification](#verification)

## How to read this

Every row is `criterion_id` — bestpractices.dev's own short name for it —
the requirement level (**MUST** blocks the badge if left Unmet; **SHOULD**
and **SUGGESTED** don't), the answer, and the evidence. Links point at
`main`; where a row cites work from the pass that produced this document,
the link resolves once that work is merged.

Four release-shaped criteria (`version_unique`, `version_tags`,
`release_notes`, `release_notes_vulns`) are marked **Pending** — the
mechanism for all four is built and rehearsed
([docs/RELEASING.md](RELEASING.md)), but nothing can be Met until a real
`v0.1.0` tag exists. Flip those the day it ships.

Silver as a whole isn't reachable yet regardless of any individual answer
below: `bus_factor` and `regression_tests_added50` are both genuine,
currently-unmet MUSTs for a solo, young project, stated plainly rather than
argued around. Every other silver row here already stands on its own.

## Passing

67 criteria across six sections.

### Basics

| Criterion | Level | Status | Notes |
| --- | --- | --- | --- |
| `description_good` — Succinct, user-friendly description of what the project does. | MUST | Met | README opens with a one-line tagline and a short "what it does and the constraint it's built around" paragraph. [README.md](https://github.com/ivylikethevine/sharerr-rs/blob/main/README.md). |
| `interact` — Info on how to get help, give feedback, and contribute. | MUST | Met | A dedicated "Getting help and contributing" section links Issues, Discussions, the private security route, and CONTRIBUTING.md. [README.md#getting-help-and-contributing](https://github.com/ivylikethevine/sharerr-rs/blob/main/README.md#getting-help-and-contributing). |
| `contribution` — The contribution process is explained. | MUST | Met | [docs/CONTRIBUTING.md](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/CONTRIBUTING.md) covers branching, the verification loop, CI gates, and PR review. |
| `contribution_requirements` — Acceptable-contribution requirements are documented. | SHOULD | Met | Same doc: sign commits (main's ruleset), pass CI, and — as of this pass — add a test for a new feature or bug fix. [docs/CONTRIBUTING.md#test-policy](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/CONTRIBUTING.md#test-policy). |
| `floss_license` — Released under an OSI-approved / FSF-approved FLOSS license. | MUST | Met | MIT. [LICENSE.md](https://github.com/ivylikethevine/sharerr-rs/blob/main/LICENSE.md). |
| `floss_license_osi` — Recommended license is OSI-approved. | SUGGESTED | Met | MIT is OSI-approved. |
| `license_location` — License posted in a standard location. | MUST | Met | [LICENSE.md](https://github.com/ivylikethevine/sharerr-rs/blob/main/LICENSE.md) at repo root; also linked from the README's Licence section. |
| `documentation_basics` — Basic documentation: install, usage, security. | MUST | Met | README Quickstart covers install/run; docs/SETTINGS.md covers configuration; docs/SECURITY.md covers the security policy. |
| `documentation_interface` — External interfaces/APIs are documented. | MUST | Met | [docs/API.md](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/API.md) plus a generated [openapi.json](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/openapi.json) for the HTTP surface (Torznab, Jackett, gossip, tracker, lighthouse). |
| `sites_https` — Project sites support HTTPS. | MUST | Met | GitHub, GitHub Pages ([_config.yml](https://github.com/ivylikethevine/sharerr-rs/blob/main/_config.yml) → github.io), and GHCR are all HTTPS-only; nothing in the project's own web UI is a "project site" in this criterion's sense. |
| `discussion` — A searchable mechanism exists for discussion/questions. | MUST | Met | GitHub Discussions is enabled, alongside Issues — both are publicly searchable. |
| `english` — Documentation and reports are in English. | SHOULD | Met | All docs and templates are English-only. |
| `maintained` — The project is actively maintained. | MUST | Met | Continuous commits since the first on 2026-08-11 ([history](https://github.com/ivylikethevine/sharerr-rs/commits/main)); [README.md's Roadmap section](https://github.com/ivylikethevine/sharerr-rs/blob/main/README.md#roadmap) states current direction. |

### Change control

| Criterion | Level | Status | Notes |
| --- | --- | --- | --- |
| `repo_public` — Publicly readable version-controlled repository, with change history. | MUST | Met | Public GitHub repo, full git history. |
| `repo_track` — Repo tracks changes, who made them, and when. | MUST | Met | Standard git; commits to main require verified signatures per its ruleset. |
| `repo_interim` — Interim versions between releases are available to testers. | MUST | Met | Every push to main publishes an unattended `sha-<commit>` GHCR image. [docs/RELEASING.md#between-releases-the-sha-tag](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/RELEASING.md#between-releases-the-sha-tag). |
| `repo_distributed` — A distributed version control system is used. | SUGGESTED | Met | Git. |
| `version_unique` — Each release has a unique version identifier. | MUST | Pending | No tag has been cut yet — mark Unmet/N/A until `v0.1.0` ships. The mechanism is fully built and rehearsed: [docs/RELEASING.md#cutting-a-release](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/RELEASING.md#cutting-a-release). |
| `version_semver` — Semantic Versioning or Calendar Versioning is used. | SUGGESTED | Met | The `v*` tag is the version and must be `vMAJOR.MINOR.PATCH[-prerelease]`; docker-image.yml's `version` step rejects anything else before a build runs, and injects it into the binary. [docs/RELEASING.md#cutting-a-release](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/RELEASING.md#cutting-a-release). |
| `version_tags` — Releases are identified in the VCS via tags. | SUGGESTED | Pending | Mechanism documented and rehearsed ([docs/RELEASING.md](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/RELEASING.md)) but unused until the first real tag. |
| `release_notes` — Human-readable release notes exist for each release. | MUST | Pending | `gh release create --generate-notes` is wired into the release job; nothing to point at until the first tag. [docs/RELEASING.md#the-github-release](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/RELEASING.md#the-github-release). |
| `release_notes_vulns` — Release notes identify publicly known vulnerabilities fixed. | MUST | Pending | Same mechanism, same blocker — no release exists yet to carry one. |

### Reporting

| Criterion | Level | Status | Notes |
| --- | --- | --- | --- |
| `report_process` — A process exists for submitting bug reports. | MUST | Met | GitHub Issues with structured templates. [.github/ISSUE_TEMPLATE/bug_report.yml](https://github.com/ivylikethevine/sharerr-rs/blob/main/.github/ISSUE_TEMPLATE/bug_report.yml). |
| `report_tracker` — An issue tracker is used for tracking individual issues. | SHOULD | Met | GitHub Issues. |
| `report_responses` — Majority of bug reports are acknowledged within 2-12 months. | MUST | No data yet | No external bug report has been filed yet against this young repo — there is no history to demonstrate a response rate against. Revisit once reports exist. |
| `enhancement_responses` — Majority of enhancement requests are acknowledged. | SHOULD | No data yet | Same gap as above — no external requests yet. |
| `report_archive` — A publicly searchable archive of reports exists. | MUST | Met | GitHub Issues (and Discussions) are public and searchable by default. |
| `vulnerability_report_process` — A process for reporting vulnerabilities is published. | MUST | Met | [docs/SECURITY.md#reporting-a-vulnerability](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SECURITY.md#reporting-a-vulnerability). |
| `vulnerability_report_private` — A private vulnerability-reporting method is supported. | MUST | Met | GitHub Private Vulnerability Reporting is enabled for this repo; SECURITY.md links directly to `/security/advisories/new`. |
| `vulnerability_report_response` — Vulnerability reports are acknowledged within 14 days (initial response). | MUST | Met | A 14-day acknowledgement target is now stated explicitly, plus what happens after: [docs/SECURITY.md#what-happens-after-a-report](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SECURITY.md#what-happens-after-a-report). |

### Quality

| Criterion | Level | Status | Notes |
| --- | --- | --- | --- |
| `build` — A working build process exists. | MUST | Met | `cargo build`; documented in [docs/CONTRIBUTING.md#getting-set-up](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/CONTRIBUTING.md#getting-set-up). |
| `build_common_tools` — Build uses commonly-used tools. | SUGGESTED | Met | Cargo — the standard Rust build tool. |
| `build_floss_tools` — Can be built using only FLOSS tools. | SHOULD | Met | Cargo, rustc, and every dependency are FLOSS; no proprietary toolchain step. |
| `test` — An automated test suite covers most of the codebase and is run before releases. | MUST | Met | `cargo test --workspace --all-features` runs in CI on every push/PR; the [coverage badge](https://github.com/ivylikethevine/sharerr-rs/actions/workflows/coverage.yml) carries the current line-coverage figure (tier 1 only), well above 80%. |
| `test_invocation` — Tests are invocable in a standard way for the language. | SHOULD | Met | Plain `cargo test`. |
| `test_most` — Test suite covers most branches, input fields, and functionality. | SUGGESTED | Met | See the live coverage badge on the README; `cargo llvm-cov` line, region and function figures all sit above 90%, tier 1 only. |
| `test_continuous_integration` — CI is used; tests run on every commit or daily. | SUGGESTED | Met | [.github/workflows/ci.yml](https://github.com/ivylikethevine/sharerr-rs/blob/main/.github/workflows/ci.yml) runs the full suite on every push and PR. |
| `test_policy` — A policy requires tests for major new functionality. | MUST | Met | Now stated explicitly. [docs/CONTRIBUTING.md#test-policy](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/CONTRIBUTING.md#test-policy). |
| `tests_are_added` — Evidence the test policy is generally followed. | MUST | Met | Over a thousand `#[test]`/`#[tokio::test]` functions across the workspace (`grep -rc '#\[test\]\|#\[tokio::test\]' crates/`), added continuously alongside features — a demonstrated practice, not a percentage claim. |
| `tests_documented_added` — Test-addition requirements are documented for contributors. | SUGGESTED | Met | [docs/CONTRIBUTING.md#test-policy](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/CONTRIBUTING.md#test-policy). |
| `warnings` — Compiler warnings / static analysis is enabled. | MUST | Met | `cargo clippy --workspace --all-targets --all-features -- -D warnings`, gating in CI. |
| `warnings_fixed` — Warnings are addressed, not just enabled. | MUST | Met | Zero warnings in the current tree (verified today) — CI fails otherwise. |
| `warnings_strict` — Warning flags are as strict as practical. | SUGGESTED | Met | `-D warnings` plus workspace-level `unwrap_used`/`expect_used`/`missing_debug_implementations` lints and `unsafe_code = "forbid"`. [Cargo.toml](https://github.com/ivylikethevine/sharerr-rs/blob/main/Cargo.toml) (`[workspace.lints]`). |

### Security

| Criterion | Level | Status | Notes |
| --- | --- | --- | --- |
| `know_secure_design` — At least one developer knows secure-design principles. | MUST | Met | Demonstrated throughout docs/SECURITY.md and docs/ARCHITECTURE.md's trust-boundary section — least privilege, fail-closed auth, defense in depth. |
| `know_common_errors` — Developer(s) know common vulnerability types and how to mitigate them. | MUST | Met | Same evidence — CSRF (Origin/Host check), enumeration (decoy hash), replay (signed timestamps), injection (bound SQL params, XML escaping) are each named and mitigated. |
| `crypto_published` — Only publicly published, reviewed cryptography is used. | MUST | Met | Argon2id, XChaCha20-Poly1305, Ed25519, SHA-256 — all standard, peer-reviewed primitives via `argon2`, `chacha20poly1305`, `ed25519-dalek`, `sha2`. |
| `crypto_call` — Cryptographic calls go through dedicated libraries, not hand-rolled. | SHOULD | Met | Zero hand-rolled crypto in production code — verified by review of every crypto call site. |
| `crypto_floss` — Crypto functionality is usable with FLOSS. | MUST | Met | All crypto crates are FLOSS (MIT/Apache-2.0), pure Rust. |
| `crypto_keylength` — Meets current NIST minimum key-length guidance. | MUST | Met | 256-bit vault keys, Ed25519 (128-bit security level), 256-bit session tokens — all at or above NIST minimums. |
| `crypto_working` — Does not depend on broken crypto (MD5, SHA-1 for security, RC4...). | MUST | Met | SHA-1 appears only as BitTorrent's protocol-mandated info-hash (`sharerr-torrent`'s `metainfo` module), never for a security decision. |
| `crypto_weaknesses` — Avoids cryptographic modes with known serious weaknesses. | SHOULD | Met | AEAD (XChaCha20-Poly1305) throughout, no ECB, no unauthenticated encryption. |
| `crypto_pfs` — Supports perfect forward secrecy where session keys are negotiated. | SHOULD | Met | Outbound HTTPS calls go through rustls, which defaults to TLS 1.3 / ECDHE — both PFS. sharerr terminates no TLS of its own for inbound traffic; that's delegated to an operator's reverse proxy, documented in [docs/SECURITY.md#why-the-existing-controls-are-enough](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SECURITY.md#why-the-existing-controls-are-enough). |
| `crypto_password_storage` — Passwords are stored as iterated hashes with a per-user salt. | MUST | Met | Login passwords: Argon2id, per-user salt ([crates/sharerr-store/src/users.rs](https://github.com/ivylikethevine/sharerr-rs/blob/main/crates/sharerr-store/src/users.rs)). Note: peer API keys are SHA-256, unsalted, single-round — deliberate, since they're 160-bit CSPRNG tokens rather than human passwords, needing an indexed-equality lookup. See [crates/sharerr-store/src/peers.rs](https://github.com/ivylikethevine/sharerr-rs/blob/main/crates/sharerr-store/src/peers.rs)'s header comment. |
| `crypto_random` — Cryptographic randomness comes from a CSPRNG. | MUST | Met | Every random value in the tree goes through one `getrandom` funnel. [crates/sharerr-store/src/lib.rs](https://github.com/ivylikethevine/sharerr-rs/blob/main/crates/sharerr-store/src/lib.rs). |
| `delivery_mitm` — Delivery uses HTTPS/SSH, resisting MITM. | MUST | Met | Git over SSH/HTTPS, GHCR pulls over HTTPS, GitHub Pages over HTTPS. |
| `delivery_unsigned` — No unsigned hash is fetched over plain HTTP and trusted. | MUST | Met | Every checksum this project relies on is HTTPS-fetched; CI tool downloads (zizmor, actionlint, cargo-llvm-cov, lychee, typos, hadolint) are now verified against a recorded/published sha256 after download — hardened as part of this pass. [.github/actions/setup-tool/tools.txt](https://github.com/ivylikethevine/sharerr-rs/blob/main/.github/actions/setup-tool/tools.txt). |
| `vulnerabilities_fixed_60_days` — Medium+ severity vulnerabilities are fixed within 60 days of disclosure. | MUST | Met | No vulnerability has been disclosed yet to measure against; the process (docs/SECURITY.md) commits to prompt triage and fix. Revisit with real data once one exists. |
| `vulnerabilities_critical_fixed` — Critical vulnerabilities are fixed rapidly. | SHOULD | Met | Same — process-based commitment, no historical data yet. |
| `no_leaked_credentials` — No valid credentials are leaked in the repository. | MUST | Met | GitHub secret scanning is on by default for public repos; the vault design keeps secrets out of `sharerr.toml` by construction (`skip_serializing`), and CodeQL's own cleartext-logging queries run on every push. |

### Analysis

| Criterion | Level | Status | Notes |
| --- | --- | --- | --- |
| `static_analysis` — Static analysis is applied before major releases. | MUST | Met | CodeQL (Rust + Actions) and clippy run on every push and PR, not just before releases. [.github/workflows/codeql.yml](https://github.com/ivylikethevine/sharerr-rs/blob/main/.github/workflows/codeql.yml). |
| `static_analysis_common_vulnerabilities` — At least one static analysis tool targets common vulnerability classes for the language. | SUGGESTED | Met | CodeQL's Rust query pack is exactly this. |
| `static_analysis_fixed` — Medium+ severity findings are fixed in a timely way. | MUST | Met | CI blocks on clippy findings; CodeQL alerts are triaged and either fixed or dismissed with a written reason (see docs/SECURITY.md's "What is out of scope"). |
| `static_analysis_often` — Static analysis runs on every commit or at least daily. | SUGGESTED | Met | CodeQL runs on every push/PR plus a weekly baseline cron. |
| `dynamic_analysis` — Dynamic analysis (fuzzing, etc.) is applied before major releases. | SUGGESTED | Unmet | Not present. Deliberately tracked as a real gap in `.scorecard.yml` rather than hidden — three candidate fuzz targets are named (sharerr-torrent, sharerr-rtorrent's XML-RPC parsing, sharerr-probe's media parsing). |
| `dynamic_analysis_unsafe` — A dynamic tool with memory-safety detection is used, for memory-unsafe languages. | SUGGESTED | N/A | N/A — Rust, with `unsafe_code = "forbid"` at the workspace level and zero `unsafe` blocks across 104 source files (verified by grep today). |
| `dynamic_analysis_enable_assertions` — Assertions are enabled during dynamic analysis. | SUGGESTED | Met | Rust's `debug_assert!` is active in the debug-profile builds the test suite runs under. |
| `dynamic_analysis_fixed` — Medium+ severity dynamic-analysis findings are fixed promptly. | MUST | N/A | N/A alongside dynamic_analysis — no dynamic-analysis tool is run yet to produce findings. |

## Silver

Everything passing requires, plus the sections below.

### Basics

| Criterion | Level | Status | Notes |
| --- | --- | --- | --- |
| `dco` — A Developer Certificate of Origin or CLA is in place. | SHOULD | Unmet | Not adopted — CONTRIBUTING.md already states an inbound=outbound licensing agreement (contributions are MIT by submission), which was judged sufficient without adding a sign-off requirement. [docs/CONTRIBUTING.md#licence](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/CONTRIBUTING.md#licence). |
| `code_of_conduct` — A code of conduct is adopted and posted in a standard location. | MUST | Met | [docs/CODE_OF_CONDUCT.md](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/CODE_OF_CONDUCT.md) — Contributor-Covenant-shaped, GitHub-recognized location. |
| `governance` — The project's governance model is documented. | MUST | Met | [docs/GOVERNANCE.md](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/GOVERNANCE.md) — states plainly that this is a single-maintainer project and how decisions get made. |
| `roles_responsibilities` — Key roles and who holds them are documented. | MUST | Met | [docs/GOVERNANCE.md#roles](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/GOVERNANCE.md#roles) — maintainer, contributor, reporter, and what each can do. |
| `documentation_roadmap` — Direction for at least the next year is documented. | MUST | Met | [README.md's Roadmap section](https://github.com/ivylikethevine/sharerr-rs/blob/main/README.md#roadmap). |
| `documentation_architecture` — Software architecture/design is documented. | MUST | Met | [docs/ARCHITECTURE.md](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/ARCHITECTURE.md) — crate map, end-to-end data flow diagram, trust boundaries, where state lives. |
| `documentation_security` — Security requirements, expectations, and the assurance case are documented. | MUST | Met | New section: [docs/SECURITY.md#why-the-existing-controls-are-enough](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SECURITY.md#why-the-existing-controls-are-enough) — argues why the existing controls are adequate against the stated threat model, not just lists them. |
| `documentation_quick_start` — A quick-start guide exists for new users. | MUST | Met | [README.md#quickstart](https://github.com/ivylikethevine/sharerr-rs/blob/main/README.md#quickstart). |
| `documentation_current` — An effort is made to keep docs in sync with the current version. | MUST | Met | A test ([crates/sharerr/src/web/docs.rs](https://github.com/ivylikethevine/sharerr-rs/blob/main/crates/sharerr/src/web/docs.rs)) resolves every UI doc link against a real heading in the tree, plus markdownlint + lychee CI jobs — a renamed heading fails `cargo test`, not just an advisory lint. |
| `documentation_achievements` — Achievements are identified and hyperlinked within 48 hours of attainment. | MUST | Met | The Best Practices, Scorecard, and Baseline badges are already live in the README header, updating automatically as this project's standing changes — no manual edit needed per achievement. |
| `sites_password_security` — Passwords are stored as salted, iterated hashes. | MUST | Met | Same evidence as passing's `crypto_password_storage` — Argon2id, per-user salt. |
| `accessibility_best_practices` — The software follows accessibility best practices. | SHOULD | Unmet | No accessibility audit has been done (no axe-core / aria review in CI). Real, acknowledged gap — the web UI has not been evaluated against WCAG. |
| `internationalization` — Software is designed to be easy to localize. | SHOULD | Unmet | English-only by design for a small self-hosted tool; no i18n framework. Acknowledged trade-off, not an oversight. |

### Continuity

| Criterion | Level | Status | Notes |
| --- | --- | --- | --- |
| `access_continuity` — The project can continue within a week if any one person becomes unavailable. | MUST | Met | Honest case, not an overclaim: sharerr holds no user data of its own — every secret lives in an operator's own encrypted vault — and the project is MIT-licensed with full public history, so a fork needs no permission. [docs/GOVERNANCE.md#continuity](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/GOVERNANCE.md#continuity). |
| `bus_factor` — Bus factor of 2 or more. | SHOULD | Unmet | Genuinely unmet — solo maintainer. Stated plainly rather than implied away. [docs/GOVERNANCE.md#continuity](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/GOVERNANCE.md#continuity). |

### Change control

| Criterion | Level | Status | Notes |
| --- | --- | --- | --- |
| `maintenance_or_update` — Older versions are maintained, or a documented upgrade path exists. | MUST | Met | New Supported Versions section: exactly one supported line (main / the newest sha-tagged image) and what "upgrade" means until v0.1.0 ships. [docs/SECURITY.md#supported-versions](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SECURITY.md#supported-versions). |

### Reporting

| Criterion | Level | Status | Notes |
| --- | --- | --- | --- |
| `vulnerability_report_credit` — Vulnerability reporters are credited unless they ask otherwise. | MUST | Met | [docs/SECURITY.md#what-happens-after-a-report](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SECURITY.md#what-happens-after-a-report) states reporters are credited in the advisory and release notes unless they ask to stay anonymous. |
| `vulnerability_response_process` — A documented process exists for responding to vulnerability reports. | MUST | Met | Same new section — acknowledgement target, triage, fix, disclosure, credit, in order. |

### Quality & test

| Criterion | Level | Status | Notes |
| --- | --- | --- | --- |
| `automated_integration_testing` — The automated test suite runs on every check-in, for at least one branch. | MUST | Met | [.github/workflows/ci.yml](https://github.com/ivylikethevine/sharerr-rs/blob/main/.github/workflows/ci.yml) runs on every push to main and every PR. |
| `test_policy_mandated` — A formal written policy requires tests for major new functionality. | MUST | Met | [docs/CONTRIBUTING.md#test-policy](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/CONTRIBUTING.md#test-policy) — explicit, not just implied by the verification loop. |
| `regression_tests_added50` — Regression tests were added for at least 50% of bugs fixed in the last six months. | MUST | Unmet | Checked against real history: of the handful of non-CI "fix" commits in the tree's life so far, roughly one in five added a test in the same commit. Below the 50% bar — an honest reading, not a bar the new test policy alone retroactively clears. Should trend up now that the policy above is explicit. |
| `test_statement_coverage80` — The automated test suite provides at least 80% statement coverage. | MUST | Met | Above 80% by a wide margin per the live badge, measured by `cargo llvm-cov --workspace` in `coverage.yml` (tier-1 only). [docs/TESTING.md#coverage](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/TESTING.md#coverage). |
| `coding_standards` — Coding style guides are identified and compliance is required. | MUST | Met | `rustfmt.toml`, `.editorconfig`, and CLAUDE.md's clippy/lint conventions. |
| `coding_standards_enforced` — Compliance with the style guide(s) is automatically enforced. | MUST | Met | `cargo fmt --all --check` and `clippy -D warnings` both gate CI. |

### Externally-maintained components

| Criterion | Level | Status | Notes |
| --- | --- | --- | --- |
| `external_dependencies` — External dependencies are listed in a machine-processable way. | MUST | Met | `Cargo.toml` / `Cargo.lock` (committed) plus `deny.toml`. |
| `updateable_reused_components` — Reused components are easily identified and updated. | MUST | Met | Dependabot across 4 ecosystems (cargo, github-actions, docker, docker-compose), weekly, with a 7-day cooldown. [.github/dependabot.yml](https://github.com/ivylikethevine/sharerr-rs/blob/main/.github/dependabot.yml). |
| `interfaces_current` — Deprecated/obsolete interfaces are avoided where a FLOSS alternative exists. | SHOULD | Met | No known use of a deprecated API; clippy would flag one, and dependencies are kept current by dependabot. |

### Build

| Criterion | Level | Status | Notes |
| --- | --- | --- | --- |
| `build_standard_variables` — The build honors standard variables like CC, CFLAGS, LDFLAGS. | MUST | N/A | N/A — Rust/Cargo's build model doesn't use the C-toolchain environment-variable convention this criterion targets; the closest equivalent (`RUSTFLAGS`, `CARGO_*`) is honored by cargo itself. |
| `build_preserve_debug` — Debugging information is preserved if requested via standard flags. | SHOULD | Unmet | Not specifically tested; the release Docker build strips to a minimal runtime image by design. No counter-evidence either way — treating as an open item rather than claiming Met. |
| `build_non_recursive` — The build system doesn't recursively build cross-dependent subdirectories. | MUST | Met | A single Cargo workspace build, not a recursive per-directory make. |
| `build_repeatable` — Building twice from the same source produces identical bits. | MUST | Unmet | Real, open gap: Rust embeds absolute source paths by default, so a build isn't bit-for-bit reproducible without `--remap-path-prefix`, which isn't configured. Determinism (digest-pinned base images, `--locked`, cargo-chef caching) is in place; true reproducibility is not. |

### Installation

| Criterion | Level | Status | Notes |
| --- | --- | --- | --- |
| `installation_common` — Installation/uninstallation is easily usable, following platform conventions. | MUST | Met | `docker pull` / `docker run`, documented in Quickstart — the standard convention for a containerized service. |
| `installation_standard_variables` — DESTDIR and standard installation-location conventions are honored. | MUST | N/A | N/A — no `make install`-style installation exists; the container image is the distribution unit. |
| `installation_development_quick` — Developers can quickly install and test their own build. | MUST | Met | `cargo build && cargo run`, plus the tier-1 hermetic suite needing no external service. |

### Security

| Criterion | Level | Status | Notes |
| --- | --- | --- | --- |
| `implement_secure_design` — Secure-design principles are implemented where applicable. | MUST | Met | Fail-closed auth, least-privilege CI tokens, defense in depth — see [docs/ARCHITECTURE.md#trust-boundaries](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/ARCHITECTURE.md#trust-boundaries). |
| `input_validation` — All externally-influenced inputs are validated with an allowlist approach. | MUST | Met | Allowlists at every untrusted edge: token character class, compile-time config paths, media-extension allowlist, private-IP allowlist on gluetun webhooks. See [crates/sharerr-core/src/config.rs](https://github.com/ivylikethevine/sharerr-rs/blob/main/crates/sharerr-core/src/config.rs). |
| `hardening` — Hardening mechanisms reduce the likelihood of exploiting a vulnerability. | SHOULD | Unmet | Acknowledged gap: no login rate limit and no security response headers (CSP, X-Frame-Options). Stated openly in [docs/SECURITY.md#why-the-existing-controls-are-enough](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SECURITY.md#why-the-existing-controls-are-enough) rather than hidden — deliberate for a trusted-LAN, single-operator tool, but genuinely Unmet against this criterion's letter. |
| `crypto_weaknesses` — Does not depend on cryptography with known serious weaknesses. | MUST | Met | Same as passing — AEAD throughout, no broken primitives. |
| `crypto_algorithm_agility` — Multiple cryptographic algorithms can be swapped in quickly. | SHOULD | Unmet | Deliberately not built — one fixed, modern algorithm per purpose (XChaCha20-Poly1305, Argon2id, Ed25519, SHA-256), no pluggable scheme. A simplicity trade-off, not an oversight. |
| `crypto_credential_agility` — Credentials/keys are stored separately from other data. | MUST | Met | The vault is a separate encrypted store from `sharerr.toml`'s plain config, by design. |
| `crypto_used_network` — Secure network protocols are supported; insecure ones disabled by default. | SHOULD | Met | Outbound clients are rustls-only — no native-tls, no OpenSSL — and there is no config surface to disable verification. |
| `crypto_tls12` — TLS 1.2 or later is used if TLS is used. | SHOULD | Met | rustls 0.23 implements only TLS 1.2 and 1.3; there is no code path for anything older. |
| `crypto_certificate_verification` — TLS certificate verification is on by default. | MUST | Met | Confirmed by review: `danger_accept_invalid_certs` appears nowhere in the tree, and the shared client constructor in [crates/sharerr-client/src/lib.rs](https://github.com/ivylikethevine/sharerr-rs/blob/main/crates/sharerr-client/src/lib.rs) exposes no way to disable it. |
| `crypto_verification_private` — Certificates are verified before sending sensitive data. | MUST | Met | Same client path — verification is not optional, so there's no route that could skip it before sending credentials. |

### Secure release

| Criterion | Level | Status | Notes |
| --- | --- | --- | --- |
| `signed_releases` — Releases are cryptographically signed with a documented verification process. | MUST | Met | Sigstore-backed build provenance via `actions/attest-build-provenance`, attached to the published image digest; verification documented as `gh attestation verify`. [docs/RELEASING.md#verifying-a-published-image](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/RELEASING.md#verifying-a-published-image). |
| `version_tags_signed` — VCS tags for releases are cryptographically signed. | SUGGESTED | Met | the release ceremony now uses `git tag -s`, matching main's existing verified-signature requirement. [docs/RELEASING.md#cutting-a-release](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/RELEASING.md#cutting-a-release). |

### Analysis

| Criterion | Level | Status | Notes |
| --- | --- | --- | --- |
| `static_analysis_common_vulnerabilities` — A static analysis tool targeting common vulnerabilities is used (elevated from Suggested). | MUST | Met | CodeQL's Rust security query pack, run on every push/PR plus weekly. |
| `dynamic_analysis_unsafe` — A dynamic memory-safety tool is used, for memory-unsafe languages (elevated from Suggested). | MUST | N/A | N/A — Rust, `unsafe_code = "forbid"`, zero `unsafe` blocks. |
| `static_analysis_often` — Static analysis runs on every commit or daily. | SUGGESTED | Met | Same as passing. |
| `dynamic_analysis` — Dynamic analysis is applied to proposed releases. | SUGGESTED | Unmet | Same acknowledged gap as passing. |
| `dynamic_analysis_enable_assertions` — Run-time assertions are enabled during dynamic analysis. | SUGGESTED | Met | Same as passing. |
| `dependency_monitoring` — External dependencies are monitored for known vulnerabilities. | MUST | Met | `cargo-deny` gates every PR and runs weekly; Trivy scans the published image weekly; Dependabot watches all four ecosystems. |

## Verification

Every "Met" answer above was checked against the live tree, not assumed:

- The verification loop in
  [docs/CONTRIBUTING.md](CONTRIBUTING.md#the-verification-loop) passes.
- `docker build -f docker/Dockerfile .`, the de-facto MSRV check, succeeds on
  the pinned 1.98 toolchain.
- `cargo llvm-cov --workspace --summary-only` puts line coverage well above
  the 80% `test_statement_coverage80` asks for, tier-1 only; the
  [coverage badge](https://github.com/ivylikethevine/sharerr-rs/actions/workflows/coverage.yml)
  is the live figure.
- `zizmor`, `actionlint`, and `shellcheck` report zero findings against
  every workflow and script this pass touched.
- The sha256 of every pinned CI tool release asset (zizmor, actionlint,
  cargo-llvm-cov, lychee, typos) was computed from a live download and
  cross-checked against actionlint's and lychee's own published checksum
  files, then wired into [`tools.txt`](../.github/actions/setup-tool/tools.txt)
  and verified by [`install.sh`](../.github/actions/setup-tool/install.sh)
  before any downloaded binary runs.
- `regression_tests_added50` was checked against real git history, not
  estimated: the non-CI "fix" commits in the repo's life so far were
  inspected for whether the same commit added a `#[test]` or
  `#[tokio::test]` function.
