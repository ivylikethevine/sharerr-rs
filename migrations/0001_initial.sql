-- Items sharerr has discovered and (attempted to) share.
--
-- The natural key is (source, file_id): an *arr file id is stable and identifies
-- exactly the thing that gets turned into a torrent. Reconciliation diffs against
-- this key, so a sync that discovers the same library twice is a no-op.
CREATE TABLE shared_items (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    source        TEXT    NOT NULL CHECK (source IN ('sonarr', 'radarr')),
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
    state         TEXT    NOT NULL CHECK (state IN ('pending', 'seeding', 'unshared', 'failed')),
    last_error    TEXT,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,

    UNIQUE (source, file_id)
);

CREATE INDEX idx_shared_items_state ON shared_items (state);
CREATE INDEX idx_shared_items_info_hash ON shared_items (info_hash) WHERE info_hash IS NOT NULL;

-- One row per reconciliation pass, for operator visibility.
CREATE TABLE sync_runs (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    started_at  INTEGER NOT NULL,
    finished_at INTEGER,
    discovered  INTEGER NOT NULL DEFAULT 0,
    added       INTEGER NOT NULL DEFAULT 0,
    unshared    INTEGER NOT NULL DEFAULT 0,
    failed      INTEGER NOT NULL DEFAULT 0,
    error       TEXT
);
