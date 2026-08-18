-- Where each friend has recently been seen, and the identity that lets friends
-- vouch for each other's addresses.
--
-- A peer row is a credential — label, key hash, scope, last seen — and carried no
-- address at all, so sharerr could say that a friend turned up but not where they
-- are. This table keeps a short, timestamped history of observed addresses per
-- peer, with the *kind* of address recorded separately: a friend on a dual-VPN
-- setup has their API/feed traffic and their torrent client behind different
-- exits while both belong to one sharerr, and collapsing them into one column
-- would make each sighting overwrite the other forever.
--
-- Kinds: 'api' (the source address of an authenticated feed or gossip request),
-- 'client' (their torrent client, as their gossip reports it), 'tracker' (their
-- sharerr's announce endpoint, as their gossip reports it).
--
-- 'via' says how the sighting arrived: 'direct' (we saw the connection) or
-- 'gossip' (a mutual friend relayed a record the subject signed). Direct
-- sightings are first-hand; gossip is only ever accepted with a valid signature,
-- but ranking the two still matters when they disagree.
--
-- History rather than one row per kind, on purpose: a reconnect that briefly
-- returns an old exit is *remembered* rather than trusted — the newest row wins,
-- but the previous addresses are still there to fall back through.
CREATE TABLE peer_endpoints (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    peer_id     INTEGER NOT NULL REFERENCES peers (id) ON DELETE CASCADE,
    kind        TEXT    NOT NULL,
    addr        TEXT    NOT NULL,
    observed_at INTEGER NOT NULL,
    via         TEXT    NOT NULL DEFAULT 'direct',

    -- Re-observing a known address refreshes its timestamp instead of growing
    -- the table; the store also prunes each (peer, kind) list to a handful of
    -- newest rows on insert.
    UNIQUE (peer_id, kind, addr)
);

CREATE INDEX idx_peer_endpoints_peer ON peer_endpoints (peer_id, kind, observed_at DESC);

-- The peer's gossip identity: their Ed25519 public key, lowercase hex.
--
-- Learned on trust-on-first-use from the first *self*-record they present over
-- their authenticated API key — from then on it is the join key gossip records
-- are verified against, and a record about a pubkey no peer row carries is
-- ignored rather than stored (we do not learn about strangers). NULL until the
-- friend's sharerr has spoken to ours at all.
ALTER TABLE peers ADD COLUMN pubkey TEXT;

-- The outbound half of a friendship, which never existed before: where *their*
-- sharerr is, so ours can pull gossip from it. NULL means we only ever answer.
-- The key they issued us lives in the vault under `peer.gossip.<id>`, not here —
-- unlike our own peers' key *hashes*, that one is a secret we replay.
ALTER TABLE peers ADD COLUMN gossip_url TEXT;

-- The latest *verified* self-record this peer presented, raw JSON, kept so it
-- can be relayed to mutual friends exactly as signed. Gossip can only pass on
-- proof, never restate it: a record sharerr rewrote would no longer verify, so
-- the original bytes are what gets stored and forwarded.
ALTER TABLE peers ADD COLUMN gossip_record TEXT;
