<!--
docs/CONTRIBUTING.md has the long version of all of this.
-->

# What this changes, and why

<!-- The behaviour that moved. Link the issue if there is one. -->

## The constraints

- [ ] `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, and `cargo test --workspace` all pass — see CLAUDE.md's verification loop
- [ ] MSRV 1.98 still holds (`docker build -f docker/Dockerfile .` is the real check; a local toolchain won't catch a breach)
- [ ] tier-1 tests stay hermetic — no network, no containers, no database
- [ ] no secret reaches `sharerr.toml` — see CLAUDE.md's "Secrets never go in sharerr.toml"

## Docs

- [ ] the docs that were affected by this change are updated

## AI (if applicable)

- [ ] some of this was written with generative AI, and I have understood,
      reviewed and stood behind it — see [README's AI usage](https://github.com/ivylikethevine/sharerr-rs/blob/main/README.md#ai-usage)
