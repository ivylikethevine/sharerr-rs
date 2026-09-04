# Documentation

Reference material that doesn't fit in [the README](../README.md)'s
walkthrough. Each doc says at the top what it covers versus the README, so
this page is deliberately just a map, not a summary.

| Doc | Covers |
| --- | --- |
| [Settings reference](SETTINGS.md) | Every `sharerr.toml` field, environment variable, and vault secret. |
| [Support](SUPPORT.md) | What sharerr talks to today and the seam each category plugs into, and what is deliberately left out — with the reason attached. |
| [The API](API.md) | The machine-facing HTTP contract (Torznab, Jackett, gossip, tracker, lighthouse) and how it's generated. |
| [Architecture](ARCHITECTURE.md) | The crate map, how a share moves end to end, trust boundaries, and where state lives. |
| [The lighthouse](LIGHTHOUSE.md) | Design rationale for the rendezvous service — why it exists and its privacy property. |
| [Roadmap](ROADMAP.md) | What's still ahead, from feature-sized commitments to ideas only weighed so far, including what stands between the tree and a first tagged release. |
| [Design brief](DESIGN.md) | The original statement of intent, kept verbatim, and where the implementation disproved it. |
| [Testing](TESTING.md) | The two test tiers, the compose stacks tier 2 drives, coverage, and what's not there yet (benchmarks, fuzzing). |
| [Releasing](RELEASING.md) | What a `v*` tag triggers, the two GHCR images, the approval gate, and how to rehearse it. |
| [Security policy](SECURITY.md) | How to report a vulnerability, and what's in and out of scope. |
| [Governance](GOVERNANCE.md) | Decision-making, roles, and what continuity looks like for a single-maintainer project. |
| [Code of conduct](CODE_OF_CONDUCT.md) | Standards for participating in issues, pull requests, and any other project space. |
| [Contributing](CONTRIBUTING.md) | How to build, test, and submit a change. |
