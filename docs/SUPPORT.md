# Support

Where to ask for help with sharerr, what to put in the request, and what to
expect back. What sharerr works with (apps, clients, indexers) and what it
deliberately does not do is [`COMPATIBILITY.md`](COMPATIBILITY.md). That
material used to live on this page, so an older reference to `SUPPORT.md`
for supported services, the feed's magnet link, or the private flag (a
migration comment, say) now means
[`COMPATIBILITY.md`](COMPATIBILITY.md#the-feeds-magnet-link).

## Contents

- [Where to ask](#where-to-ask)
- [What to include](#what-to-include)
- [What to expect](#what-to-expect)

## Where to ask

- **Found a bug or want a feature?**
  [Open an issue](https://github.com/ivylikethevine/sharerr-rs/issues/new/choose)
  and pick the bug report or feature request form. Check
  [`COMPATIBILITY.md`](COMPATIBILITY.md) for what is supported today and
  [the roadmap](../README.md#roadmap) for what is already planned or
  considered.
- **Have a question, or want to show off your setup?**
  [Start a discussion](https://github.com/ivylikethevine/sharerr-rs/discussions).
- **Found a security issue?** Do not open a public issue; see
  [`SECURITY.md`](SECURITY.md#reporting-a-vulnerability) for the private
  route.
- **Want to contribute a change?** [`CONTRIBUTING.md`](CONTRIBUTING.md), and
  the [code of conduct](CODE_OF_CONDUCT.md) for any project space.
- **Wondering who's behind this?** [`GOVERNANCE.md`](GOVERNANCE.md): a
  personal project, maintained by one person in their spare time.

## What to include

The bug report form asks for all of this; the same list makes a question
answerable too:

- **`sharerr doctor` output.** It checks credentials, reachability, the tag,
  and path mapping, which answers most "is this configured right" questions
  on its own. Redact hostnames, API keys and paths freely; what matters is
  which check failed and why.
- **How you run it**: the image tag or build (`v*` or `sha-*` GHCR tag, or
  built from source) and the host's OS and architecture.
- **Which pieces are in play**: the *arr app (or a plain directory), the
  torrent client, and the deployment layout, since most problems sit between
  two of them.
- **What you saw and what you expected**, with the command or UI action that
  shows it.

## What to expect

sharerr is experimental and maintained by one person in their spare time.
Issues and discussions are read, but there is no response-time commitment,
and a reply can take a while. A report with the details above is much more
likely to get a quick answer than one that needs a round of questions first.
Security reports follow their own timeline, in
[`SECURITY.md`](SECURITY.md#what-happens-after-a-report).
