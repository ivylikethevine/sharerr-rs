# Roadmap

Where sharerr is and where it is going. Status is honest: _Done_ means implemented
and covered by tests, not merely written.

sharerr is **experimental**. Nothing below is a release commitment, and the
ordering is a judgement about value, not a schedule.

## Status

| Milestone | Scope                                                                       | Status |
| --------- | --------------------------------------------------------------------------- | ------ |
| —         | The lighthouse: semi-anonymous endpoint rendezvous, its own image and port  | Next   |
| —         | Ratio and bandwidth control                                                 | Next   |

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

**Per-peer announce tokens.** The tracker's announce token is one shared secret,
so an announce carries no peer identity — which is why peer endpoint memory
cannot attribute a torrent client's address from a direct announce and has to
learn it from gossip instead. Per-peer tokens would close that gap and make
"cut this friend off" reach the tracker too, not just the feed.

**Ratio and bandwidth control.** No upload limits, no seeding goals. Sharing a
library with no cap on what it costs you is a real deterrent to running this.

**Request flow.** The original design brief wanted a friend's Sonarr/Radarr to
_request_ content. Today discovery is one-way: they find what you already share.
An inbound request queue with an approve step is the other half of that idea.

**Cross-seed awareness.** The brief called for preserving existing torrents rather
than creating duplicates. If a file is already seeding in qBittorrent under another
torrent, sharerr should recognise it rather than adding a second entry for the same
bytes.

**Health and history in the UI.** The store already records run history
(`recent_runs`), and the status page shows only the latest runs. Per-item state —
why _this_ file is not shared — is the question the UI cannot currently answer.

**Notifications.** Webhook/Discord/Apprise on sync failure or a peer going quiet.
Standard for the *arr ecosystem this lives in.

---

## Ease of use

**Setup wizard.** Settings is a wall of fields. A guided first-run — services, then
paths, then tracker, validating each step — matches how the *arr apps onboard and
would catch misconfiguration at the point it is introduced.

**Auto-detect path mappings.** sharerr knows what Sonarr reports and what it can
see; proposing the mapping instead of asking the operator to derive it removes the
single most error-prone configuration step.

---

## The lighthouse

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
