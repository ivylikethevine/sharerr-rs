-- A hybrid v1/v2 torrent (BEP 52) announces under two identities: its v1 info
-- hash, which `info_hash` already holds and every client reports it by, and the
-- truncated SHA-256 of the same info dictionary. The tracker admits an announce
-- by either, so the second is kept beside the first. NULL for a v1 torrent.
ALTER TABLE shared_items ADD COLUMN info_hash_v2 TEXT;
CREATE INDEX idx_shared_items_info_hash_v2 ON shared_items (info_hash_v2);
