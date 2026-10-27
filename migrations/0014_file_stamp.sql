-- The shared file's inode and modification time, as sharerr saw them when it
-- built the item's torrent.
--
-- A torrent's pieces hash the file's bytes at build time. A file rewritten in
-- place since (a tag editor, a remux, a "replace with better" that kept the
-- name) still sits at the same path, so nothing else notices, and every
-- friend downloading it fails the piece checks. Each sync pass compares these
-- with the file on disk and notifies on a difference (`Syncer::share`'s fast
-- path, the `file_changed` trigger); nothing is moved or rebuilt.
--
-- NULL for an adopted torrent, which sharerr did not build, and for a row
-- that predates this column until the first pass records what it finds.
-- `file_inode` is also NULL on a platform without inodes.
ALTER TABLE shared_items ADD COLUMN file_inode INTEGER;
ALTER TABLE shared_items ADD COLUMN file_mtime_ns INTEGER;
