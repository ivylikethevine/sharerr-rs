# A standalone lighthouse

`sharerr-lighthouse`, the rendezvous service friends' sharerr instances use
to find each other after an address changes, in its own compose project. It
is not sharerr and not one of the four layouts: no library mount, no torrent
client, no \*arr apps, no path mappings, no master key. Why it exists and
its privacy property are in [`docs/LIGHTHOUSE.md`](../../../docs/LIGHTHOUSE.md);
this page is the walkthrough. The compose files' header comments are the
long form of everything here.

## Contents

- [When to pick it](#when-to-pick-it)
- [Bring it up](#bring-it-up)
- [With TLS](#with-tls)
- [Gotchas](#gotchas)

## When to pick it

Pick it to self-host a lighthouse on neutral ground for a friend group that
would rather not each run one. Using a lighthouse and running one are
independent choices; a sharerr instance does not need its own. To stand the
same layout up on an AWS, Azure or Fly.io free tier instead, use
[`terraform/`](terraform/README.md). For sharerr itself, see
[the "Which one" table](../README.md#which-one).

## Bring it up

1. Optionally copy the environment file. Both variables in it (`TZ`,
   `RUST_LOG`) already have working defaults, and there is no secret to
   generate: the only state is a decoy secret the lighthouse mints on first
   run.

   ```bash
   cd docker/deploy/lighthouse
   cp .env.example .env
   ```

2. Start it:

   ```bash
   docker compose up -d
   ```

3. Check it. There is no `sharerr doctor` for a lighthouse; the image's own
   healthcheck polls its health endpoint, which `docker compose ps` reports,
   and you can ask it directly:

   ```bash
   docker compose ps
   curl --fail http://localhost:7878/lighthouse/v1/health
   ```

4. Give friends the URL. Each adds it under Settings → Lighthouse, or as
   `lighthouse.urls` in their `sharerr.toml`
   ([using one](../../../docs/LIGHTHOUSE.md#using-one)).

## With TLS

`compose.tls.yaml` layers Caddy in front, with a Let's Encrypt certificate
for a domain whose A record already points at this host:

```bash
DOMAIN=lighthouse.example.net docker compose -f compose.yaml -f compose.tls.yaml up -d
```

Only 80 and 443 then need reaching from outside. Friends' `lighthouse.urls`
should use the `https://` address.

## Gotchas

- **Put it behind TLS.** There is no login page or session cookie behind
  7878, but a lookup answer is only as trustworthy as the transport it
  travels over.
- **7878 stays published under the TLS override.** The base file still
  publishes it, so close it at the firewall (the Terraform security groups
  do) or drop the `ports:` block from a copy of `compose.yaml`.
- **Caddy's volume matters more than the lighthouse's.** Losing
  `lighthouse-data` only reshuffles fabricated answers after a restart.
  Losing `caddy-data` means a fresh certificate issuance, and Let's Encrypt
  rate-limits those.
- **Caddy retries until DNS is right.** If the A record does not point at
  this host yet, no certificate is issued and Caddy keeps trying rather than
  failing.
- **7878 is also Radarr's port** in [`direct/`](../direct/README.md), which
  publishes it on loopback. Running both on one host needs one of them moved.
