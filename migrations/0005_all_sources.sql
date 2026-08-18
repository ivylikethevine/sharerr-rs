-- Drop the CHECK constraints on shared_items.source and .state.
--
-- 0001 froze `source IN ('sonarr', 'radarr')` into the schema, and no later
-- migration touched it — so when Lidarr, Readarr and Whisparr discovery landed,
-- every one of their items failed at INSERT with a raw CHECK error while the
-- Rust side (which had its own copy of the same closed set) compiled clean.
-- The closed sets belong to `MediaSource` and `ShareState`, whose `parse`
-- round-trips are tested; a second copy in SQL is exactly the two-descriptions
-- drift that produced this migration. SQLite cannot drop a CHECK in place, so
-- the table is rebuilt.
CREATE TABLE shared_items_new (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    source        TEXT    NOT NULL,
    source_id     INTEGER NOT NULL,
    file_id       INTEGER NOT NULL,
    spec_json     TEXT    NOT NULL,
    release_title TEXT    NOT NULL,
    -- Stored exactly as the *arr app reported it, before any path mapping, so a
    -- remapping change does not orphan existing rows.
    arr_path      TEXT    NOT NULL,
    size          INTEGER NOT NULL,
    ids_json      TEXT    NOT NULL,
    info_hash     TEXT,
    state         TEXT    NOT NULL,
    last_error    TEXT,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,

    UNIQUE (source, file_id)
);

INSERT INTO shared_items_new
    SELECT id, source, source_id, file_id, spec_json, release_title, arr_path,
           size, ids_json, info_hash, state, last_error, created_at, updated_at
    FROM shared_items;

DROP TABLE shared_items;
ALTER TABLE shared_items_new RENAME TO shared_items;

CREATE INDEX idx_shared_items_state ON shared_items (state);
CREATE INDEX idx_shared_items_info_hash ON shared_items (info_hash) WHERE info_hash IS NOT NULL;
