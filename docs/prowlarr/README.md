# A Prowlarr definition

[`sharerr.yml`](sharerr.yml) is a Cardigann definition for one friend's
sharerr: Prowlarr then lists it as **sharerr**, with its categories, search
modes and API key field already filled in, instead of a _Generic Torznab_
indexer configured by hand. The feed behind it is the same either way.

## Contents

- [Installing it](#installing-it)
- [Why it is per host](#why-it-is-per-host)

## Installing it

1. Copy `sharerr.yml` and replace the one URL under `links:` with the address
   of the sharerr you were given a key for, ending in `/`
   (`https://sharerr.alice.example/`). The feed is at `api` under it.
2. Put the file in Prowlarr's `Definitions/Custom/` directory, beside its
   database (`/config/Definitions/Custom/` in the common container images),
   and restart Prowlarr.
3. Add an indexer, pick **sharerr**, and paste the key.

Two friends' instances need two copies, each with its own `id` and file name
(`sharerr-alice`, matching), since Prowlarr keys definitions by `id`.

## Why it is per host

Prowlarr offers a Cardigann indexer only the base URLs its definition lists,
as a choice rather than a field, and URL-encodes every variable templated
into a search path, so a definition cannot carry an address typed in later.
A single listing in Prowlarr's catalog would need Prowlarr to accept a custom
base URL for Cardigann indexers; until then each friend keeps a local copy.
