# Releasing

What a `v*` tag does, the tag scheme every image follows, the one-time setup
it depends on, and how to rehearse the path without it reaching anyone.
`v0.0.1`, `v0.1.0` and `v0.1.1` have driven it end to end; rehearsing via
`workflow_dispatch` is still the way to test a change to the workflows
themselves without cutting a tag.

## Table of contents

- [What ships, and where](#what-ships-and-where)
- [The tag scheme](#the-tag-scheme)
- [Cutting a release](#cutting-a-release)
- [The GitHub Release](#the-github-release)
- [The one-time setup this depends on](#the-one-time-setup-this-depends-on)
- [Rehearsing it](#rehearsing-it)
- [Verifying a published image](#verifying-a-published-image)
- [Between releases: the sha tag](#between-releases-the-sha-tag)

## What ships, and where

Two container images, built by one workflow and approved together, plus a
GitHub Release page once both are published:

| Image | Workflow | `docker/Dockerfile` target |
| --- | --- | --- |
| `ghcr.io/ivylikethevine/sharerr-rs` | `docker.yml` | `runtime-sharerr` |
| `ghcr.io/ivylikethevine/sharerr-lighthouse` | `docker.yml` | `runtime-lighthouse` |

`docker.yml` runs two build jobs — `docker` and `lighthouse` — each a thin
caller of the shared `docker-image.yml`, which holds the actual build logic.
One Dockerfile, one builder stage, one MSRV pin. The two build jobs run
unattended and independently, but a single `publish` job, behind one
`environment: release` approval, promotes both: approving a release approves
both images as a matched set. If either build fails, `publish` never runs,
and neither image is promoted.

This used to be two separate workflow files, each with its own approval,
specifically so a break in one build could never hold up the other's
release — cutting a release meant approving twice. That has been reversed in
favor of the one-approval flow above: the two images now always ship
together or not at all, trading the old failure isolation (a lighthouse-only
break no longer lets the sharerr image ship on its own) for a release that
only ever needs one click.

Nothing else is published anywhere: no crates.io (see
[`docs/SUPPORT.md`](SUPPORT.md#publishing-to-cratesio)), no binaries attached
to the Release. The container image is the distribution channel; the Release
page is a pointer at it.

## The tag scheme

| Trigger | What is pushed | Architectures |
| --- | --- | --- |
| Pull request | Nothing. Build only, as a check | amd64 |
| Push to `main` | `sha-<7-char-sha>`, unattended. No `latest`, no branch tag | amd64 + arm64 |
| `workflow_dispatch` | `pending-<full-sha>`, a rehearsal nobody would find by browsing | amd64 + arm64 |
| `v*` tag | `pending-<full-sha>` from `build`; then, after approval, `publish` retags that digest as `X.Y.Z`, `X.Y`, `latest` and `sha-<7>` | amd64 + arm64 |

A prerelease tag (one with a `-` segment, `v1.0.0-rc1`) never moves `latest`.

## Cutting a release

The tag is the version; there is nothing to bump first.
`[workspace.package].version` in `Cargo.toml` is a fixed `0.0.0-dev`
placeholder: a build script in each binary crate reads `SHARERR_VERSION`,
`docker/Dockerfile` takes it as a build arg, and `docker-image.yml`'s
`version` step sets it to the tag minus its `v` (or to `0.0.0-dev+g<sha7>`
for a `main` push or a rehearsal). `sharerr --version`, the web footer, the
OpenAPI document, Jackett's `server_config` and `sharerr-lighthouse --version`
all report that one string.
A tag that is not `vMAJOR.MINOR.PATCH[-prerelease]` fails the `version` step
before anything is built or pushed.

Push a **signed** tag: `main`'s ruleset already
requires verified commit signatures, and `git tag -v` gives anyone checking
out the release something to verify beyond "GitHub says this ref exists".

```bash
git tag -s v1.2.3
git push origin v1.2.3
git tag -v v1.2.3   # verify, the same as a commit
```

Each of `docker.yml`'s two build jobs then runs unattended: checks out the
tag, cross-compiles both architectures, pushes its multi-arch image to
`pending-<sha>` with a signed build-provenance attestation. `publish` runs
next, behind one required reviewer, and does not rebuild either image: it
retags the exact digests `docker` and `lighthouse` attested with `docker
buildx imagetools create`, one image at a time. Approving a release means
approving bytes that already exist, for both images at once. See
`docker-image.yml`'s own comments for the job-by-job detail of a single
build, and `docker.yml`'s `publish` job for how the two are promoted
together.

## The GitHub Release

`docker.yml` carries one more job, `release`, that runs after `publish` —
so only once both images have actually been promoted — and creates the
Releases-tab entry:

- **Title and tag**: the pushed `v*` tag, verbatim.
- **Notes**: `gh release create --generate-notes`, so merged PR titles since
  the last tag, with a short preamble giving the `docker pull` and
  `gh attestation verify` commands for both images. The PR _title_ is the
  release-notes line; write it for a user. See
  [`docs/SUPPORT.md`](SUPPORT.md#a-maintained-changelogmd) for why this is
  the model instead of a `CHANGELOG.md`.
- **Prerelease**: a tag with a `-` segment is marked as one.

Because `release` needs `publish`, which itself needs both build jobs to have
succeeded, the release always describes both images — there is no longer a
path where one image ships without the other, or without the Release page.

## The one-time setup this depends on

**`environment: release` needs a required reviewer configured before it gates
anything.** This is a repo setting (Settings → Environments → `release` →
"Required reviewers"), not something a workflow file can assert. Until it
exists, `publish` runs unattended the moment `build` finishes. Confirm it
before trusting that a release needs a human in the loop.

## Rehearsing it

`docker.yml` accepts a manual `workflow_dispatch`: both its build jobs push
to the same `pending-<sha>` tags and produce the same attestations a real tag
push would, so the whole build-push-attest path for both images can be
exercised any day. `publish` and `release` both require
`github.event_name == 'push'`, which a dispatch can never satisfy, so a
rehearsal publishes nothing and creates no Release whatever the
environment's reviewer setting says.

```bash
gh workflow run docker.yml
```

## Verifying a published image

```bash
gh attestation verify oci://ghcr.io/ivylikethevine/sharerr-rs:latest \
  --repo ivylikethevine/sharerr-rs
```

Checks the signed proof that the tag's digest was built by this repo's own
workflow run, from the commit it claims. The attestation travels with the
digest through the retag, so it needs no regenerating against the final
tags. Substitute `sharerr-lighthouse` for the lighthouse image.

## Between releases: the sha tag

Every push to `main` ships its own image unattended under `sha-<7-char-sha>`
and nothing else. It is findable only by someone who already has the commit,
which is why this half stays unattended while the tagged path does not: the
risk of a sha-only tag reaching someone who did not choose it is close to
zero, where `latest` moving unattended is not. No rolling prerelease is
planned; the sha tag already serves that purpose. The push to `main` is also
where the arm64 cross-compile path gets proven on every merge, since a pull
request builds amd64 only.
