# Releasing

What a `v*` tag does, the tag scheme every image follows, the one-time setup
it depends on, and how to rehearse the path without it reaching anyone.
Every entry on the [Releases page](https://github.com/ivylikethevine/sharerr-rs/releases)
is a tag that drove it end to end; rehearsing via `workflow_dispatch` is
still the way to test a change to the workflows themselves without cutting a
tag.

## Contents

- [What ships, and where](#what-ships-and-where)
- [The tag scheme](#the-tag-scheme)
- [Cutting a release](#cutting-a-release)
- [The GitHub Release](#the-github-release)
- [The one-time setup this depends on](#the-one-time-setup-this-depends-on)
- [Rehearsing it](#rehearsing-it)
- [Verifying a published image](#verifying-a-published-image)
- [Between releases: the sha tag](#between-releases-the-sha-tag)

## What ships, and where

Two container images, built by one workflow and published together, plus a
GitHub Release page once both are published:

| Image                                       | Workflow     | `docker/Dockerfile` target |
| ------------------------------------------- | ------------ | -------------------------- |
| `ghcr.io/ivylikethevine/sharerr-rs`         | `docker.yml` | `runtime-sharerr`          |
| `ghcr.io/ivylikethevine/sharerr-lighthouse` | `docker.yml` | `runtime-lighthouse`       |

`docker.yml` runs two build jobs — `docker` and `lighthouse` — each a thin
caller of the shared `docker-image.yml`, which holds the actual build logic.
One Dockerfile, one builder stage, one MSRV pin. A `v*` tag then runs one
chain of jobs:

| Job       | What it does                                                                                                  |
| --------- | ------------------------------------------------------------------------------------------------------------- |
| `gate`    | Refuses a malformed tag, an unsigned or unknown-key tag, a commit not on `main`, or a commit without green CI |
| builds    | `docker (sharerr)` and `docker (lighthouse)`: build both architectures, smoke-test, push to `pending-<sha>`   |
| `scan`    | trivy against both pushed digests; a fixable HIGH or CRITICAL finding in either blocks the release            |
| `publish` | Promotes both scanned digests to their real tags, in the `release` environment                                |
| `release` | Creates the GitHub Release                                                                                    |

`gate` runs beside the builds rather than before them, since it waits on
`ci.yml`, which takes about as long as a build. `publish` needs `gate`, both
builds and `scan`, and `release` needs `publish`, so a failure anywhere
upstream promotes neither image and creates no Release. A refusal leaves both
images at `pending-<sha>`, exactly where a rehearsal leaves them.

This used to be two separate workflow files, each with its own `release`
environment gate, specifically so a break in one build could never hold up
the other's release. That has been reversed in favor of the one `publish`
job above: the two images now always ship together or not at all, trading
the old failure isolation (a lighthouse-only break no longer lets the sharerr
image ship on its own) for one release path instead of two.

Nothing else is published anywhere: no crates.io (see
[`docs/COMPATIBILITY.md`](COMPATIBILITY.md#publishing-to-cratesio)), no binaries attached
to the Release. The container image is the distribution channel; the Release
page is a pointer at it.

## The tag scheme

| Trigger             | What is pushed                                                                                                                                  | Architectures |
| ------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------- | ------------- |
| Pull request        | Nothing. Build and smoke test only, as a check                                                                                                  | amd64         |
| Push to `main`      | `sha-<7-char-sha>`, unattended and attested. No `latest`, no branch tag                                                                         | amd64 + arm64 |
| `workflow_dispatch` | `pending-<full-sha>`, a rehearsal nobody would find by browsing                                                                                 | amd64 + arm64 |
| `v*` tag            | `pending-<full-sha>` from the builds; then, once `gate` and `scan` pass, `publish` retags that digest as `X.Y.Z`, `X.Y`, `latest` and `sha-<7>` | amd64 + arm64 |

A prerelease tag (one with a `-` segment, `v1.0.0-rc1`) is published as
`X.Y.Z-rc1` and `sha-<7>` only: never `latest`, never `X.Y`. Its GitHub
Release is marked as a prerelease and never as Latest. No tag deploys the
docs site, prerelease or not: `pages.yml` builds from `main`.

Every build that pushes (a `main` push, a `v*` tag, a rehearsal) carries an
SPDX SBOM and `mode=max` build provenance inside the image index, plus signed
GitHub attestations for the provenance and for the SBOM of each
architecture, pushed to GHCR next to the digest. Every build, pushed or not,
runs `docker/smoke_test.sh`: the binary's `--version`, and the image's own
`HEALTHCHECK` reporting healthy.

## Cutting a release

The tag is the version; there is nothing to bump first.
`[workspace.package].version` in `Cargo.toml` is a fixed `0.0.0-dev`
placeholder: a build script in each binary crate reads `SHARERR_VERSION`,
`docker/Dockerfile` takes it as a build arg, and `docker-image.yml`'s
`version` step sets it to the tag minus its `v` (or to `0.0.0-dev+g<sha7>`
for a `main` push or a rehearsal). `sharerr --version`, the web footer, the
OpenAPI document, Jackett's `server_config` and `sharerr-lighthouse --version`
all report that one string.

Push a **signed** tag, from a commit `main` already carries, ideally after
`ci.yml` has finished on it. Sign with an SSH key listed in
`.github/allowed_signers` as that file stands on `main`, with the matching
email as the tagger:

```bash
git tag -s v1.2.3
git push origin v1.2.3
git tag -v v1.2.3   # verify, the same as a commit
```

Pushing the tag starts the chain above, unattended. `gate` refuses, and
nothing is published, when:

- **the tag is not `vMAJOR.MINOR.PATCH[-prerelease]`** (the builds'
  `version` step refuses the same shape before building);
- **the tag is lightweight, unsigned, or signed by a key not in `main`'s
  `.github/allowed_signers`.** The list is read from `main`, not from the
  tagged commit, so a key removed on `main` stops verifying at once, even for
  a tag on an older commit that still lists it;
- **the tagged commit is not reachable from `main`**, so a tag cut on `dev`
  or a feature branch publishes nothing;
- **`ci.yml`'s push run on `main` did not conclude `success` for that exact
  commit.** A run whose checks were carried forward from a green PR run on
  the same tree counts. A run still queued or in progress is waited on for up
  to 20 minutes, then refused; no run at all is refused at once.

The builds check out the tag, cross-compile both architectures, smoke-test,
and push each multi-arch image to `pending-<sha>` with its attestations. A
release build rebuilds the runtime stages' shared base (`runtime-base`) from
scratch rather than from the build cache (`no-cache-filters`), because that
stage runs `apt-get upgrade`, and a cached layer would ship whatever Debian
packages were current when `main` last wrote the cache, past the scan meant
to see today's. The Rust builder stages, where the cache saves the time,
still read it.

`scan` runs trivy against the exact digests the builds pushed, for fixable
HIGH and CRITICAL vulnerabilities (`--ignore-unfixed`: a finding with no fix
is one a rebuild cannot clear). Findings go to code scanning under
`release-scan-<image>` either way, so a blocked release shows what blocked
it in the Security tab as well as in the log. The one escape hatch is
`docker/.trivyignore`, read from the tagged commit: each entry needs a reason
and an `exp:` expiry date, after which trivy stops ignoring it, and it is
committed before the tag like any other change. There is no bypass switch.

`publish` does not rebuild either image: it retags the digests `scan`
passed, by digest rather than by the `pending-<sha>` tag (which could move
in between), with `docker buildx imagetools create`, one image at a time. A
release promotes bytes that already exist and were already attested, for
both images at once. See `docker-image.yml`'s own comments for the
job-by-job detail of a single build, and `docker.yml` for the gate, the
scan and the promotion.

## The GitHub Release

`docker.yml`'s `release` job runs after `publish` — so only once both images
have actually been promoted — and creates the Releases-tab entry:

- **Title and tag**: the pushed `v*` tag, verbatim.
- **Preamble**: the `docker pull` commands for both images, the
  `gh attestation verify` commands for their provenance and SBOMs, and, when
  `coverage.yml` measured the tagged commit, its test count and line
  coverage.
- **What changed**: the `## Release note` section of each pull request
  between the previous non-prerelease tag and this one (so a release covers
  its release candidates too), one bullet per PR. A `none` note is left out.
  `release-note.yml` warns (without failing) on a PR whose body has no such
  section, or a blank one; a `dev` → `main` PR's note
  aggregates the notes of the PRs merged into `dev`. See
  [`docs/COMPATIBILITY.md`](COMPATIBILITY.md#a-maintained-changelogmd) for why this is
  the model instead of a `CHANGELOG.md`.
- **Fallback**: when no PR in the range wrote a note, GitHub's generated list
  of merged PR titles stands in as the body.
- **Prerelease**: a tag with a `-` segment is marked as a prerelease and is
  never marked Latest, matching the images, where it never moves `latest`.

Because `release` needs `publish`, which itself needs `gate`, both builds and
`scan`, the release always describes both images — there is no path where
one image ships without the other, or without the Release page.

## The one-time setup this depends on

**Releases run unattended: the `release` environment has no required
reviewer.** Pushing a `v*` tag is the decision to release; nothing waits for
a click afterwards. What stands between that push and GHCR is `gate` and
`scan` above, plus:

- **`.github/allowed_signers` on `main`.** `gate` verifies the tag against
  the copy on `main`, so a new signing key (another machine, a rotation) has
  to reach `main` through a PR before a tag signed with it can publish.
- **Write access.** Only someone with write access can push a `v*` tag.
- **The environment's deployment policy.** Settings → Environments →
  `release` → "Deployment branches and tags" limits the environment to
  `main` and `v*` tags, so no other ref can reach `publish`.
- **The egress allowlist.** `publish` and `release` hold write tokens, so
  their `step-security/harden-runner` step blocks network access outside
  GitHub, GHCR and Sigstore rather than only auditing it.

The environment's reviewer list and ref policy are repo settings, not
something a workflow file can assert: confirm
the environment lists no required reviewer (a leftover one silently turns
every release back into a pending approval) and keeps that ref policy.

## Rehearsing it

`docker.yml` accepts a manual `workflow_dispatch`: both its build jobs push
to the same `pending-<sha>` tags, produce the same attestations, and run the
same smoke test a real tag push would, and `scan` scans them, so the whole
build-push-attest-scan path for both images can be exercised any day.
`gate`, `publish` and `release` all require `github.event_name == 'push'`,
which a dispatch can never satisfy, so a rehearsal publishes nothing and
creates no Release whatever the environment's settings say.

```bash
gh workflow run docker.yml
```

## Verifying a published image

```bash
# build provenance: built by this repo's workflow, from the commit it claims
gh attestation verify oci://ghcr.io/ivylikethevine/sharerr-rs:latest \
  --repo ivylikethevine/sharerr-rs

# the signed SPDX SBOM: what is inside the image
gh attestation verify oci://ghcr.io/ivylikethevine/sharerr-rs:latest \
  --repo ivylikethevine/sharerr-rs \
  --predicate-type https://spdx.dev/Document/v2.3
```

The first checks the signed proof that the tag's digest was built by this
repo's own workflow run, from the commit it claims; the second, the signed
SBOM of the image's packages, one per architecture. Both attestations travel
with the digest through the retag, so they need no regenerating against the
final tags. Substitute `sharerr-lighthouse` for the lighthouse image, or a
`sha-<7>` tag for a `main` build. The same commands, filled in for the tag,
head each GitHub Release.

## Between releases: the sha tag

Every push to `main` ships its own image unattended under `sha-<7-char-sha>`
and nothing else, attested and smoke-tested the same as a release build. It
is findable only by someone who already has the commit, which is why this
half needs no gate or release scan while the tagged path does: the risk of a
sha-only tag reaching someone who did not choose it is close to zero, where
`latest` moving on a broken commit is not. No rolling prerelease is planned;
the sha tag already serves that purpose. The push to `main` is also where the
arm64 cross-compile path gets proven on every merge, since a pull request
builds amd64 only.
