# Governance

How decisions get made, who holds which role, and what happens to the
project if the one person currently running it disappears. [The
README](../README.md) covers using sharerr;
[`CONTRIBUTING.md`](CONTRIBUTING.md) covers submitting a change; this page is
about who decides and why, for the parts neither of those documents.

## Table of contents

- [Decision-making model](#decision-making-model)
- [Roles](#roles)
- [How a change gets in](#how-a-change-gets-in)
- [Continuity](#continuity)

## Decision-making model

sharerr is a **single-maintainer project**. There is no steering committee,
no vote, and no second person to appeal a decision to — [`docs/CODEOWNERS`](CODEOWNERS)
names one owner for the whole tree because there is nobody yet to split
ownership between. This is stated here plainly rather than implied by an
empty page, because a governance document that pretends otherwise would be
less useful than one that says what is actually true.

That does not mean decisions are made in private: direction lives in
[the README's roadmap](../README.md#roadmap), design tradeoffs are recorded in
[`docs/DESIGN.md`](DESIGN.md) and [`docs/LIGHTHOUSE.md`](LIGHTHOUSE.md), and
anything feature-sized starts as a public issue before a PR, per
[`CONTRIBUTING.md`](CONTRIBUTING.md#before-you-start) — so the reasoning
behind a decision is visible even though the decision itself is not a
committee vote.

## Roles

| Role | Who | What they can do |
| --- | --- | --- |
| Maintainer | `@ivylikethevine`, per [`docs/CODEOWNERS`](CODEOWNERS) | Reviews and merges PRs, cuts releases, holds repo admin access, sets the project's direction in [the README's roadmap](../README.md#roadmap), triages security reports. |
| Contributor | Anyone who opens a PR | Proposes a change. No merge rights; every PR is reviewed by the maintainer before it lands, per the CI table in [`CONTRIBUTING.md`](CONTRIBUTING.md#what-ci-runs). |
| Reporter | Anyone who opens an issue, a discussion, or a private security advisory | Raises a bug, a feature request, or a vulnerability report. See [`docs/SUPPORT.md`](SUPPORT.md) and [`docs/SECURITY.md`](SECURITY.md#reporting-a-vulnerability) for the right channel for each. |

**Becoming a maintainer**: nobody has yet, so there is no established
process to describe. If sustained contribution ever makes this worth
revisiting, it will be decided and recorded here rather than happening
informally.

## How a change gets in

1. Anything feature-sized starts as an issue — see
   [`CONTRIBUTING.md`](CONTRIBUTING.md#before-you-start). A small, obviously
   correct fix doesn't need that step.
2. A PR branches from `dev`, the active development branch.
3. CI has to pass — the blocking jobs are listed in
   [`CONTRIBUTING.md`](CONTRIBUTING.md#what-ci-runs): `rustfmt`,
   `clippy + tests`, `msrv`, `cargo-deny`, `shell + compose`, and workflow
   lint. Three more (hadolint, markdownlint, typos) report but don't block.
4. The maintainer reviews and merges into `dev`.
5. `main` carries a GitHub ruleset — pull request required, protected ref,
   verified commit signatures — so nothing reaches `main`, including from
   the maintainer's own machine, without a signed commit and a PR. See
   [`CONTRIBUTING.md`](CONTRIBUTING.md#commits-and-pull-requests).

## Continuity

**What access exists**: GitHub repository admin (the source of everything
else — branch rulesets, environments, Actions secrets), the `release`
deployment environment's approval gate, and the GHCR packages the release
workflows publish to. All of it currently sits with the one maintainer
account.

**The honest continuity argument, not an overstated one**: sharerr holds no
user data of its own — no hosted accounts, no telemetry, nothing collected
from any instance other than the maintainer's own. Every secret an instance
holds (vault contents, session tokens, gossip keys) lives on that operator's
own machine, encrypted with a key only they hold; losing the maintainer does
not expose or endanger anyone else's data, because there isn't a shared
store of it to lose. The project is MIT-licensed
([`LICENSE.md`](../LICENSE.md)) and the full source, history, and CI
configuration are public, so a fork by anyone willing to pick it up is
possible today without asking permission — that is the actual continuity
plan for a single-maintainer project this shape, rather than a promise of
a bus-factor this project does not have.

This is a **should**, not a **must**, in the OpenSSF Best Practices Badge's
own silver criteria (`bus_factor`) precisely because a solo project cannot
truthfully claim otherwise — see [`docs/SECURITY.md`](SECURITY.md) for the
same candor applied to what the project's security controls do and do not
cover.
