# Releasing

What a `v*` tag actually does, the one-time setup it depends on, and how to
rehearse the whole path without it reaching anyone. No tag has yet driven this
path end to end, so rehearse it (below) rather than trusting it.

## Contents

- [What ships, and where](#what-ships-and-where)
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

Both workflows are thin callers of the shared `docker-image.yml` — see that
file for the actual build/publish logic each of the two just parameterizes.

Nothing else is published anywhere — no crates.io (see
[`docs/SUPPORT.md`](SUPPORT.md#publishing-to-cratesio)), no npm, no PyPI, no
binaries attached to the Release. The container image is the distribution
channel; the Release page is a human-readable pointer at it, not a second one.

## Cutting a release

The local ceremony is one command:

```bash
git tag v1.2.3
git push origin v1.2.3
```

From there, each workflow runs its own `build` job unattended: checks out the
tag, cross-compiles both architectures (`linux/amd64,linux/arm64`), and pushes
the real, fully-built multi-arch image to a provisional tag nobody would
discover by browsing GHCR (`ghcr.io/.../image:pending-<sha>`), with a signed
build-provenance attestation attached to it. Nothing about that build is
gated — a bad tag fails loudly before anyone is asked to approve anything.

`publish` runs next, behind a required reviewer (see [the next
section](#the-one-time-setup-this-depends-on)), and does not rebuild: it
retags the exact digest `build` already produced and attested onto the tags an
operator would actually pull — `latest`, `X.Y.Z`, `X.Y` — with `docker buildx
imagetools create`. Approving a release means approving the bytes that already
exist, not asking for a second, unaudited build to happen.

`v1.2.3` has to actually be `1.2.3` — `docker.yml`'s `release` job (next
section) reads `[workspace.package].version` out of `Cargo.toml` and fails
loudly, before writing anything, if the tag disagrees with it. Bump the
version in `Cargo.toml` first; the tag follows it, not the other way around.

## The GitHub Release

`docker.yml` carries one more job, `release`, that only reaches `publish`'s
approval — it does not gate on its own. It creates the Releases-tab entry:

- **Title and tag**: the pushed `v*` tag, verbatim.
- **Notes**: GitHub's own generated notes (`gh release create --generate-notes`
  — merged PR titles since the last tag), with a short preamble prepended
  giving the exact `docker pull` and `gh attestation verify` commands for both
  images, so the release page is self-contained. See
  [`docs/SUPPORT.md`](SUPPORT.md#a-maintained-changelogmd) for why this is the
  model instead of a hand-maintained `CHANGELOG.md`.
- **Prerelease**: a tag with a `-` segment (`v1.0.0-rc1`) is marked as a
  GitHub prerelease. It also never moves `:latest` — see `docker-image.yml`'s
  `meta` step.

It runs after `publish` (`needs: docker`, the job that calls `docker-image.yml`
and therefore already carries the approval), and independently of
`docker-lighthouse.yml`'s own build — a lighthouse build failure does not
block it, for the same "a break in one build cannot hold the other's release"
reason the two images are separate workflows at all. The release describes
whichever images actually finished publishing for that tag.

## The one-time setup this depends on

**`environment: release` needs a required reviewer configured before it gates
anything.** This is a repo setting, not something either workflow file can
assert or enforce: Settings → Environments → `release` → tick "Required
reviewers" and add whoever should be able to approve a publish. Until that
setting exists, the environment imposes no gate at all and `publish` runs
unattended the moment `build` finishes — both workflow files say so at the
`publish` job, and it's worth confirming this is actually configured before
trusting that a release needs a human in the loop.

## Rehearsing it

Both workflows accept a manual `workflow_dispatch` as a rehearsal: `build`
pushes to the same provisional `pending-<sha>` tag and gets the same
provenance attestation a real `v*` tag push would produce, so the whole
build-push-attest path can be exercised any day — not only discovered broken
the day a tag is actually pushed.

`publish` cannot be reached this way, structurally: it requires
`github.event_name == 'push'` in addition to the tag-ref check, which a
dispatch can never satisfy (dispatching against an _existing_ tag's ref would
otherwise have been enough on its own — this is the gap that check closes; see
`docker.yml`'s own `publish` job comment). A rehearsal builds and attests the
real thing and publishes nothing, whatever the environment's reviewer setting
says. `release` carries the identical `github.event_name == 'push'` guard and
is equally unreachable from a dispatch — a rehearsal creates no Releases-tab
entry either.

There's no fake-version input to pass: this workflow's tags are git-ref-derived
and the provisional destination needs no version to be meaningful — the
commit sha it's keyed on is real either way.

Run it from the Actions tab, or:

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
workflow run, from the commit it claims — the attestation travels with the
digest through the retag `publish` does, rather than needing to be regenerated
against the final tags. Substitute `sharerr-lighthouse` and its own `--repo`
for the lighthouse image.

## Between releases: the sha tag

Every push to `main` — not just a tagged release — ships its own image
unattended, under `ghcr.io/.../image:sha-<7-char-sha>` and nothing else: no
`latest`, no branch tag, nothing a human would stumble across browsing GHCR's
tag list. No rolling prerelease mechanism is planned here: a sha-tagged image
already serves that purpose for anyone who has the commit — findable only by
someone who already knows the exact sha that produced it, and pullable the
moment it lands. See
CLAUDE.md's "Repository" section for why this half stays unattended while the
tagged path does not: the risk of an sha-only tag reaching someone who did not
already choose it is close to zero, where `latest` moving unattended is not.
