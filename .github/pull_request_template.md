# What's new

## What changed and why

<!-- Keep it brief, but descriptive. -->

____________ used to [be/do/say] ____________ but now it [is/does/says] ____________ because of ____________.

## Release note

<!-- One or two sentences a user reads on the release page: what they see
     differently after upgrading, present tense, no file names. Write `none`
     when nothing a user sees changes (tests, CI, doc wording). The release
     workflow collects these into the release body; a dev -> main PR sums up
     the notes of the PRs it brings in. -->

none

## Issue and discussion links

Link any relevant issues or discussions here.

## Checklist

If one of these cannot be completed, give a justification.

- [ ] The verification loop in [docs/CONTRIBUTING.md](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/CONTRIBUTING.md#the-verification-loop) passes (`scripts/check.sh`; a check it reports as skipped has not passed)
- [ ] MSRV 1.98 still holds (CI's `msrv` job checks it; locally, `docker build -f docker/Dockerfile .` is the equivalent, since a newer local toolchain won't catch a breach)
- [ ] Tier-1 tests stay hermetic: no network, no containers, no database
- [ ] This adds or extends a test that would have failed without the change, if it's a new feature or a bug fix ([test policy](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/CONTRIBUTING.md#test-policy))
- [ ] No secret reaches `sharerr.toml` ([settings reference](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#vault-secrets))
- [ ] Affected docs are updated ([which doc changes with what](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/CONTRIBUTING.md#which-doc-changes-with-what))

## AI disclosure

AI usage is allowed on this project, but "[agent] said _______" is not a valid excuse for problems with the final product. See [AI usage](https://github.com/ivylikethevine/sharerr-rs/blob/main/README.md#ai-usage) and [AI-assisted contributions](https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/CONTRIBUTING.md#ai-assisted-contributions).

- [ ] (If applicable) This code was written with generative AI.
- [ ] **(If yes to above)** I have reviewed, understood, and stand behind this code as if it were entirely hand written.
