# What's New

## What Changed & Why

<!-- Keep it brief, but descriptive please! -->

____________ used to [be/do/say] ____________ but now it [is/does/says] ____________ because of ____________.

## Release note

<!-- One or two sentences a user reads on the release page: what they see
     differently after upgrading, present tense, no file names. Write `none`
     when nothing a user sees changes (tests, CI, doc wording). The `release`
     job in docker.yml publishes this section as the release's "What changed"
     list; the PR title is only the fallback. -->

none

## Issue/Discussion Links

Please links and reference any relevant issues/discussions/etc here.

### Checklist

If one of these cannot be completed, please give a justification.

- [ ] `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, and `cargo test --workspace` all pass — see CLAUDE.md's verification loop
- [ ] MSRV 1.98 still holds (`docker build -f docker/Dockerfile .` is the real check; a local toolchain won't catch a breach)
- [ ] tier-1 tests stay hermetic — no network, no containers, no database
- [ ] this adds or extends a test that would have failed without the change, if it's a new feature or a bug fix — see docs/CONTRIBUTING.md's test policy
- [ ] no secret reaches `sharerr.toml` — see CLAUDE.md's "Secrets never go in sharerr.toml"

### AI Disclosure

AI usage is allowed on this project, but "[agent] said _______" is not a valid excuse for problems with the final product. see: ([AI Usage](https://github.com/ivylikethevine/sharerr-rs/blob/main/README.md#ai-usage))

- [ ] (If applicable) This code was written with generative AI.
- [ ] **(If yes to above)** I have reviewed, understood, and stand behind this code as if it were entirely hand written.
