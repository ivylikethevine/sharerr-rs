-- What each friend shares with us, as their own feed would list it: the
-- torrents their sharerr seeds that our key may see. Pulled first-hand from
-- the friend over the key they issued us, never relayed, and replaced whole
-- on every pull.
--
-- Read before a build: a file of ours that matches a friend's entry by ids,
-- spec and size joins the friend's swarm under their info hash instead of
-- starting a second swarm for the same bytes.
CREATE TABLE peer_catalog (
    peer_id      INTEGER NOT NULL REFERENCES peers (id) ON DELETE CASCADE,
    info_hash    TEXT    NOT NULL,
    spec_json    TEXT    NOT NULL,
    ids_json     TEXT    NOT NULL,
    size         INTEGER NOT NULL,
    private      INTEGER NOT NULL,
    -- The friend's own link to the `.torrent`, as their feed renders it for us.
    download_url TEXT    NOT NULL,

    PRIMARY KEY (peer_id, info_hash)
);

CREATE INDEX idx_peer_catalog_size ON peer_catalog (size);

-- Where a torrent sharerr did not build came from, and the tracker tiers it
-- came with: a friend's, for an item seeding under a friend's info hash, or
-- the operator's own (`peer_id` NULL) for one sharerr adopted from the client.
-- Our own announce tiers go first whenever the client's list is rewritten, and
-- these follow, so an endpoint change never drops them. A friend's tiers carry
-- our token at their tracker, so they reach the torrent client and never the
-- cached `.torrent` the feed serves.
CREATE TABLE item_origin (
    source        TEXT    NOT NULL,
    file_id       INTEGER NOT NULL,
    peer_id       INTEGER REFERENCES peers (id) ON DELETE CASCADE,
    upstream_json TEXT    NOT NULL DEFAULT '[]',

    PRIMARY KEY (source, file_id)
);
