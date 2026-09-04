# Releasing

What a `v*` tag does, the tag scheme every image follows, the one-time setup
it depends on, and how to rehearse the path without it reaching anyone. No
tag has yet driven this path end to end, so rehearse it rather than trusting
it.

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

Two container images, from two workflows, each approved separately, plus a
GitHub Release page once both images have their tags:

| Image | Workflow | `docker/Dockerfile` target |
| --- | --- | --- |
| `ghcr.io/ivylikethevine/sharerr-rs` | `docker.yml` | `runtime-sharerr` |
| `ghcr.io/ivylikethevine/sharerr-lighthouse` | `docker-lighthouse.yml` | `runtime-lighthouse` |

Both workflows are thin callers of the shared `docker-image.yml`, which holds
the build/publish logic. One Dockerfile, one builder stage, one MSRV pin; two
packages behind two approvals, so a break in one build cannot hold the
other's release.

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
Until the first `v*` tag exists, `:latest` and every version tag are
unpublished; anything that says `:latest` in a compose file will not pull.

## Cutting a release

Bump `[workspace.package].version` in `Cargo.toml` first; the tag follows it.
`docker.yml`'s `release` job fails loudly, before writing anything, if the tag
disagrees with it. Then push a **signed** tag: `main`'s ruleset already
requires verified commit signatures, and `git tag -v` gives anyone checking
out the release something to verify beyond "GitHub says this ref exists".

```bash
git tag -s v1.2.3
git push origin v1.2.3
git tag -v v1.2.3   # verify, the same as a commit
```

Each workflow's `build` job then runs unattended: checks out the tag,
cross-compiles both architectures, pushes the multi-arch image to
`pending-<sha>` with a signed build-provenance attestation. `publish` runs
next, behind a required reviewer, and does not rebuild: it retags the exact
digest `build` attested with `docker buildx imagetools create`. Approving a
release means approving bytes that already exist. See `docker-image.yml`'s
own comments for the job-by-job detail.

## The GitHub Release

`docker.yml` carries one more job, `release`, that runs after `publish` and
creates the Releases-tab entry:

- **Title and tag**: the pushed `v*` tag, verbatim.
- **Notes**: `gh release create --generate-notes`, so merged PR titles since
  the last tag, with a short preamble giving the `docker pull` and
  `gh attestation verify` commands for both images. The PR _title_ is the
  release-notes line; write it for a user. See
  [`docs/SUPPORT.md`](SUPPORT.md#a-maintained-changelogmd) for why this is
  the model instead of a `CHANGELOG.md`.
- **Prerelease**: a tag with a `-` segment is marked as one.

It runs independently of `docker-lighthouse.yml`; a lighthouse build failure
does not block it, and the release describes whichever images actually
finished publishing for that tag.

## The one-time setup this depends on

**`environment: release` needs a required reviewer configured before it gates
anything.** This is a repo setting (Settings → Environments → `release` →
"Required reviewers"), not something a workflow file can assert. Until it
exists, `publish` runs unattended the moment `build` finishes. Confirm it
before trusting that a release needs a human in the loop.

## Rehearsing it

Both workflows accept a manual `workflow_dispatch`: `build` pushes to the
same `pending-<sha>` tag and produces the same attestation a real tag push
would, so the whole build-push-attest path can be exercised any day.
`publish` and `release` both require `github.event_name == 'push'`, which a
dispatch can never satisfy, so a rehearsal publishes nothing and creates no
Release whatever the environment's reviewer setting says.

```bash
gh workflow run docker.yml
gh workflow run docker-lighthouse.yml
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
