-- How long the tracker's live swarm has actually been quiet, not just
-- whether it is quiet right now.
--
-- `Swarms` (crates/sharerr-torrent/src/announce.rs) is deliberately
-- in-memory only — it rebuilds within one announce interval, so persisting
-- it for correctness would be pointless, and its own doc comment says as
-- much. But that also means the status page's swarm tile can only ever
-- answer "right now": "nobody is in the swarm at the moment" and "nobody
-- has been in the swarm for a fortnight" are very different facts about a
-- sharing tool, and today they render identically.
--
-- A periodic sampler (`crate::swarm_history::poll_loop`) writes `SwarmStats`'
-- three totals here once an hour, so the difference becomes visible without
-- needing the correctness the in-memory map cannot offer anyway — a missed
-- sample, from a restart or a slow tick, is just one gap in a chart, never a
-- wrong number the way an admission decision would be.
CREATE TABLE swarm_samples (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    sampled_at INTEGER NOT NULL,
    swarms     INTEGER NOT NULL,
    peers      INTEGER NOT NULL,
    seeders    INTEGER NOT NULL
);

-- The store also prunes to the newest few hundred rows on every insert —
-- see `Store::record_swarm_sample` — so this index is for both that prune's
-- own ORDER BY and for reading the chart's window back out.
CREATE INDEX idx_swarm_samples_time ON swarm_samples (sampled_at DESC);
