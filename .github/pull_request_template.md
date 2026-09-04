# What's new

## What changed and why

<!-- Keep it brief, but descriptive. -->

____________ used to [be/do/say] ____________ but now it [is/does/says] ____________ because of ____________.

<!-- The PR *title* becomes a line in the generated release notes
     (`gh release create --generate-notes` reads merged PR titles), so write
     the title for a user: what they see differently after upgrading, present
     tense, no file names. -->

## Issue and discussion links

Link any relevant issues or discussions here.

## Checklist

If one of these cannot be completed, give a justification.

- [ ] The verification loop in [docs/CONTRIBUTING.md](../docs/CONTRIBUTING.md#the-verification-loop) passes (`cargo test --workspace --all-features --locked`, clippy with `-D warnings`, `cargo build`, `cargo fmt --all --check`)
- [ ] MSRV 1.98 still holds (`docker build -f docker/Dockerfile .` is the real check; a local toolchain won't catch a breach)
- [ ] Tier-1 tests stay hermetic: no network, no containers, no database
- [ ] This adds or extends a test that would have failed without the change, if it's a new feature or a bug fix ([test policy](../docs/CONTRIBUTING.md#test-policy))
- [ ] No secret reaches `sharerr.toml` ([settings reference](../docs/SETTINGS.md#vault-secrets))

## AI disclosure

AI usage is allowed on this project, but "[agent] said _______" is not a valid excuse for problems with the final product. See [AI usage](../README.md#ai-usage).

- [ ] (If applicable) This code was written with generative AI.
- [ ] **(If yes to above)** I have reviewed, understood, and stand behind this code as if it were entirely hand written.
