-- What the tracker's announces say has actually moved.
--
-- Every announce carries the client's cumulative `uploaded` and `downloaded`
-- for that torrent, and until this migration the tracker read them and threw
-- them away. `Swarms::announce` (crates/sharerr-torrent/src/announce.rs) now
-- turns each pair into a delta against the peer's previous announce in that
-- swarm, and `Store::record_transfer` adds the delta here. Only the deltas
-- are kept: the raw counters reset whenever a client restarts a torrent, so
-- summing them would count the same bytes again after every restart.
--
-- Two homes for the same deltas, because they answer two questions. The
-- per-friend columns answer "has sharing with Sam actually delivered
-- anything", and are only ever written for an announce the tracker could
-- attribute (one carrying that friend's own token). The single-row table
-- answers "how much has this instance served in total", and takes every
-- admitted announce, attributed or not -- the operator's own client and a
-- friend still on the shared token both land here and nowhere else. The
-- instance total therefore survives deleting a friend, which the per-friend
-- figure deliberately does not.
--
-- `downloaded` is the figure the status page calls "served": a friend's
-- client reporting bytes downloaded from a swarm this tracker serves is the
-- one first-hand measure of bytes leaving this instance. `uploaded` is what
-- that same client passed on to others, kept because it is free and tells a
-- friend who seeds back from one who only pulls.
ALTER TABLE peers ADD COLUMN downloaded INTEGER NOT NULL DEFAULT 0;
ALTER TABLE peers ADD COLUMN uploaded   INTEGER NOT NULL DEFAULT 0;

CREATE TABLE transfer_totals (
    -- Exactly one row, created here, updated in place.
    id         INTEGER PRIMARY KEY CHECK (id = 1),
    downloaded INTEGER NOT NULL DEFAULT 0,
    uploaded   INTEGER NOT NULL DEFAULT 0,
    -- When a non-zero delta last landed; NULL until one has.
    updated_at INTEGER
);
INSERT INTO transfer_totals (id) VALUES (1);
