# Roadmap

Where sharerr is going next.

sharerr is **experimental**. Nothing below is a release commitment, and the
ordering is a judgement about value, not a schedule. What has already shipped
lives in [the README](../README.md#what-works-today), not here — this page
tracks what is still ahead.

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
honestly: whether it can remove a torrent _without_ deleting the data, how it
replaces a torrent's tracker list in place (`set_trackers`, for endpoint
rotation), and how it expresses `AddRequest::upload_limit_kib`/`ratio_limit`
when either is set — the one deliberate exception to "ratios belong to the
client," a seeding goal stated once at add time through whatever native
mechanism the client offers for it, same as qBittorrent (inline on
`torrents/add`) and Transmission (a follow-up `torrent-set`) already do.

| Client                   | Notes                                                                                               |
| ------------------------ | ----------------------------------------------------------------------------------------------------- |
| **rTorrent / ruTorrent** | XML-RPC. Popular on seedboxes, which is exactly where someone would want to share a large library.  |

**Transmission-compatible forks:** not planned as separate work. `sharerr-transmission` has no
version pinning or fork detection — it only speaks the documented session-id handshake and
standard `torrent-*`/`session-get` methods, which is the same protocol surface a compatible fork
presents. Standing up a real fork for tier-2 was investigated and set aside: the one
actively-maintained candidate has no published Docker image and its own maintainers describe its
RPC compatibility as imperfect.

### Indexers (what consumes the feed)

Today: **Prowlarr** (_Generic Torznab_), **Jackett**-shaped URLs, and
**Sonarr/Radarr/Lidarr direct**, each confirmed against a real instance in the
tier-2 suite. **Readarr direct is explicitly out of scope**: this project
targets small-scale homelab media-file sharing, and books are a different,
much smaller scale of content than the audio/video files everything else here
shares — existing Readarr library-source support is unaffected, this is only
about the indexer direction. No further indexer work is currently planned.

## Functionality

**Per-peer announce tokens.** Stage 1 is done: a magnet built by the Torznab
feed carries the requesting friend's own `Peer.key_hash` as its announce
token instead of the one shared instance secret, so a real announce
attributes to that friend in peer endpoint memory, and revoking a peer now
also revokes their tracker access. The instance's original shared token keeps
working forever alongside this, unattributed, so nothing seeded before this
existed ever breaks.

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

Shipped — see [the README](../README.md#the-lighthouse) for how to use it. The
design rationale below is kept here because it explains *why* the rendezvous
works the way it does, which the README's usage-focused section does not
restate.

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
