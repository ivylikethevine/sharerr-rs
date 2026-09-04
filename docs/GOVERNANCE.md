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
no vote, and no second person to appeal a decision to — [`docs/CODEOWNERS`](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/CODEOWNERS)
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
| Maintainer | `@ivylikethevine`, per [`docs/CODEOWNERS`](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/CODEOWNERS) | Reviews and merges PRs, cuts releases, holds repo admin access, sets the project's direction in [the README's roadmap](../README.md#roadmap), triages security reports. |
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
3. CI has to pass; which checks block is listed in
   [`CONTRIBUTING.md`](CONTRIBUTING.md#what-ci-runs).
4. The maintainer reviews and merges into `dev`.
5. `main` carries a GitHub ruleset — pull request required, protected ref,
   verified commit signatures — so nothing reaches `main`, including from
   the maintainer's own machine, without a signed commit and a PR. See
   [`CONTRIBUTING.md`](CONTRIBUTING.md#commits-and-pull-requests).

## Continuity

All access (GitHub repository admin, the `release` environment's approval
gate, the GHCR packages) sits with the one maintainer account. The honest
continuity argument is that sharerr holds no user data of its own: no hosted
accounts, no telemetry, nothing collected from any instance but the
maintainer's. Every secret an instance holds lives on that operator's own
machine, encrypted with a key only they hold, so losing the maintainer
exposes nobody. The project is MIT-licensed ([`LICENSE.md`](../LICENSE.md))
with its full source, history, and CI configuration public, so a fork by
anyone willing to pick it up needs no permission. That is the continuity
plan, rather than a promise of a bus factor this project does not have;
the OpenSSF badge's `bus_factor` criterion is recorded as unmet for the same
reason in [`OPENSSF-IMPROVEMENTS.md`](OPENSSF-IMPROVEMENTS.md).
