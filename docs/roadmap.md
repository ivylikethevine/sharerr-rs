# Roadmap

Where sharerr is and where it is going. Status is honest: _Done_ means implemented
and covered by tests, not merely written.

sharerr is **experimental**. Nothing below is a release commitment, and the
ordering is a judgement about value, not a schedule.

## Status

| Milestone | Scope                                                                       | Status      |
| --------- | --------------------------------------------------------------------------- | ----------- |
| —         | The lighthouse: semi-anonymous endpoint rendezvous, its own image and port  | Done        |
| —         | Ratio and bandwidth control: per-torrent upload cap and seed-ratio goal     | Done        |

---

### Library sources (where tagged content comes from)

Today: **Sonarr**, **Radarr**, **Lidarr**, **Readarr** and **Whisparr** via
tag-driven discovery, and the **plain tagged directory** (`[[library]]`). Both
shapes sit behind the `LibrarySource` seam, which is where a future source would
plug in — none is currently planned. A media-server-backed source (Jellyfin,
Emby, Plex) was tried and removed: the *arr apps and a plain directory cover
the two shapes of "where content lives" this project actually wants to support.

### Torrent clients (what actually seeds)

Today: **qBittorrent** and **Transmission**, behind the `TorrentClient` trait in
`sharerr-client`. That trait is deliberately narrow, which is what made the
second client tractable — clients disagree about almost everything except "add
this torrent, with the data already at this path". Announces always go to
sharerr's own tracker, so a client needs no tracker of its own.

Adding a third is now mostly writing one file. What a new client must answer
honestly: whether it can remove a torrent _without_ deleting the data, and how
it replaces a torrent's tracker list in place (`set_trackers`, for endpoint
rotation).

| Client                            | Notes                                                                                                                |
| --------------------------------- | -------------------------------------------------------------------------------------------------------------------- |
| **rTorrent / ruTorrent**          | XML-RPC. Popular on seedboxes, which is exactly where someone would want to share a large library.                   |
| **Transmission-compatible forks** | Anything speaking the Transmission RPC should already work — the client is the same. Untested, and cheap to confirm. |

### Indexers (what consumes the feed)

Today: **Prowlarr** (_Generic Torznab_), **Jackett**-shaped URLs, and
**Sonarr/Radarr direct** (confirmed against a real Sonarr in the tier-2 suite).

| Consumer                  | Notes                                                                                           |
| ------------------------- | ----------------------------------------------------------------------------------------------- |
| **Lidarr/Readarr direct** | Follows from library-source support, since the caps document gates what they will even ask for. |

One earlier "should already work" assumption turned out not to (Sonarr direct
rejected the feed over a missing `pubDate`) — worth remembering the next time
something in this table looks done just because the shape matches.

## Functionality

**Per-peer announce tokens.** Stage 1 done: a magnet built by the Torznab feed
now carries the requesting friend's own `Peer.key_hash` as its announce token,
rather than the one shared instance secret — reusing a value that already
existed (the sha256 of their issued API key), not a new credential with its
own settings page. A real announce presenting a peer's own token is now
attributed to them in peer endpoint memory (`EndpointKind::Client`,
`ObservedVia::Direct` — `crates/sharerr/src/tracker.rs`'s
`authenticate_token`/`record_client_sighting`), closing the "cannot attribute
a torrent client's address from a direct announce" gap for the common case.
Revoking a peer — already possible, for the feed — now also, with no extra
step, revokes their tracker access: their `key_hash` simply stops resolving.
The instance's original shared token keeps working forever alongside this,
unattributed, so nothing seeded before this existed ever breaks.

Two follow-on stages, deliberately not built yet:

- **Stage 2 — attribute `.torrent` file downloads too.** Today `GET
  /torrents/{hash}.torrent` is open (no peer check, only "is this torrent
  served") and serves one static file, so it still carries the shared legacy
  token for whoever downloads that way instead of by magnet. Closing this
  means peer-authenticating that endpoint and rewriting its embedded announce
  URL per requester in memory (`sharerr_torrent::rewrite_announce` already
  exists for this) rather than caching a variant per peer on disk.
- **Stage 3 — graceful rotation of the shared legacy token itself.** Per-peer
  tokens already make expelling one specific friend surgical and instant, with
  zero effect on anyone else — no rollout needed. What is still missing is a
  safe way to rotate or retire the *shared fallback* (e.g. if it leaked, or
  eventually to sunset it once every peer is believed to have moved off it):
  hold the old and new legacy token valid together, extend the existing
  `announce_token_fp` fingerprinting to track who is still on the old one, and
  let the operator finalize once satisfied. This is not a substitute for
  Stage 1's per-peer revocation — a purely shared, rotating token can never
  stop an *already-connected* bad actor's live announces, since every peer
  holding the current value is indistinguishable from any other to the
  tracker; only a genuinely per-peer credential can do that.

**Ratio and bandwidth control.** Done for the two goals that map identically
across both clients: a per-torrent upload-speed cap and a seed-ratio goal,
configured once in Settings → Seeding limits (`[seeding]` in `sharerr.toml`)
and applied the moment sharerr hands a torrent to the client — qBittorrent
inline on `torrents/add` (`upLimit`/`ratioLimit`), Transmission via a
follow-up `torrent-set` naming the just-added hash, since `torrent-add`
itself takes neither argument. Neither client is polled or re-configured by
sharerr afterward; each one's own already-running seeding engine does the
continuous enforcement, matching `sharerr-client`'s "ratios belong to the
client" design. A time-based seeding goal is deliberately not included:
qBittorrent's equivalent is total time seeded, but Transmission only offers
*idle*-time, a different condition, and a field that meant two different
things per backend would be a footgun rather than a fix.

**Request flow.** The original design brief wanted a friend's Sonarr/Radarr to
_request_ content. Today discovery is one-way: they find what you already share.
An inbound request queue with an approve step is the other half of that idea.

---

## Ease of use

**Setup wizard.** Settings is a wall of fields. A guided first-run — services, then
paths, then tracker, validating each step — matches how the *arr apps onboard and
would catch misconfiguration at the point it is introduced.

---

## The lighthouse

**Where this stands:** done, both halves. The rendezvous service described below
is implemented — `crates/sharerr-lighthouse` — as its own binary and image
(`Dockerfile.lighthouse`) with report/lookup routes, signed-record verification,
and the deterministic decoy fabrication the privacy property depends on. sharerr
can also run it embedded on its own frontend or tracker port, toggled from
Settings → Lighthouse or via `[lighthouse]` in `sharerr.toml` directly, for an
operator who would rather not run a second container. And a sharerr instance now
reports its own endpoint to every lighthouse named in `lighthouse.urls` — one
report per active friend's issued-key hash, since a lighthouse indexes by key
hash alone — and queries the same list for any friend who has gone quiet,
`crates/sharerr/src/lighthouse_client.rs`, on the same interval gossip itself
runs on. A lookup result is only ever trusted, and fed into peer endpoint memory
as `ObservedVia::Lighthouse` (ranked below both `Direct` and `Gossip`), once it
both verifies and names the friend's already trust-on-first-use-bound pubkey —
a friend never gossiped with has nothing to check a lighthouse's answer
against, so is skipped rather than guessed at. Reporting and querying are
independent of running the embedded service (`lighthouse.enabled`): an operator
can consume a friend's lighthouse without hosting one, or host one for friends
without using it themselves.

Gossip only helps peers who can still reach _somebody_; two friends whose
addresses both rotated while neither was watching have no path back to each
other. The lighthouse is the rendezvous for that case: a tiny separate service,
deliberately knowing nothing but `key hash → latest IP and port`, that a sharerr
instance reports its endpoint to and a friend queries with the API key that peer
issued them. The privacy property is the point and shapes the whole design: a
request without a valid key gets a _plausible fabricated_ IP and port rather
than an error, so an unauthenticated probe cannot be distinguished from a valid
lookup — the lighthouse never confirms that an instance exists, and scraping it
yields only noise. That makes semi-anonymous tracking of sharerr instances
possible without any instance exposing its IP publicly. It ships as its own
docker image on its own port — not another route on sharerr's listener — so it
can be self-hosted by anyone, placed on neutral ground away from any particular
library, and carries no database worth stealing: key hashes and last-seen
addresses only. A sharerr instance treats it as one more observation source
feeding peer endpoint memory, ranked below a direct sighting of the same peer.

The fabricated answers create the opposite problem for the _legitimate_ caller:
a friend holding a valid key must be able to tell a real record from a decoy, or
the noise defeats them too. So a genuine record is verifiable — the natural shape
is the same signed endpoint record gossip uses, signed by the peer it describes
when that peer reported in, so the lighthouse relays proof it could not forge
and a JWT-style signature check separates record from decoy. A decoy carries
random bytes where the signature would be: identical on the wire to an observer
without the peer's public key, and never verifying for anyone. The deterministic
fallback where signing is unavailable: derive the decoy from a keyed hash of the
queried key hash, so decoys are at least stable across probes rather than fresh
noise that flags itself by changing.
