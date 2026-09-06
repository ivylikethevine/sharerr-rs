# Documentation

Reference material that does not fit in [the README](../README.md)'s
overview. Each fact has one home; every other page links to it. This page is
the map.

## Using sharerr

| Doc | Covers |
| --- | --- |
| [Settings reference](SETTINGS.md) | Every `sharerr.toml` field, environment variable, and vault secret, plus backup, restore, and the `[[peers]]` recovery block. |
| [Support](SUPPORT.md) | What sharerr talks to today, how the three torrent clients differ, and what is deliberately left out, with the reason attached. |
| [The lighthouse](LIGHTHOUSE.md) | Why the rendezvous service exists, its privacy property, and how to use or run one. |
| [The API](API.md) | The machine-facing HTTP contract (Torznab, Jackett, gossip, tracker, lighthouse, ops) and how it is generated. |
| [Deploying](https://github.com/ivylikethevine/sharerr-rs/blob/main/docker/deploy/README.md) | The four compose layouts, what every one needs, exposing one port to several audiences, and gluetun's key. Repo only. |
| [Security policy](SECURITY.md) | How to report a vulnerability, what is in and out of scope, and the threat model. |

## Understanding it

| Doc | Covers |
| --- | --- |
| [Architecture](ARCHITECTURE.md) | The crate map, how a share moves end to end, trust boundaries, where state lives, and refactors weighed and declined. |
| [Design brief](DESIGN.md) | The original statement of intent, kept verbatim, and where the implementation disproved it. |
| [Alternatives](ALTERNATIVES.md) | How sharerr differs from acquisition/ratio automation (Autobrr, cross-seed) and from a shared or pooled *arr instance, and what it deliberately trades away. |

## Changing it

| Doc | Covers |
| --- | --- |
| [Contributing](CONTRIBUTING.md) | The verification loop, lint and test policy, MSRV, what CI runs, and how to submit a change. |
| [Testing](TESTING.md) | The two test tiers, the compose stacks tier 2 drives, fixtures, and coverage. |
| [The compose test stacks](https://github.com/ivylikethevine/sharerr-rs/blob/main/docker/README.md) | The disposable stacks tier 2 runs against and how to drive them by hand. Repo only. |
| [Releasing](RELEASING.md) | The tag scheme, what a `v*` tag triggers, the approval gate, and how to rehearse it. |
| [Governance](GOVERNANCE.md) | Decision-making, roles, and continuity for a single-maintainer project. |
| [Code of conduct](CODE_OF_CONDUCT.md) | Standards for participating in any project space. |
| [OpenSSF Best Practices Badge](OPENSSF-IMPROVEMENTS.md) | Every passing- and silver-level criterion, the answer, and the evidence. |
