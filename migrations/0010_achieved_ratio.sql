-- What the torrent client itself reports for this torrent's ratio, refreshed
-- each sync pass by the reconciliation loop's fast (Unchanged) path -- see
-- Store::set_ratio. NULL before a torrent exists, for a row that predates
-- this column, or (ratio_limit_reported only) when the backend cannot
-- express a per-torrent limit (rTorrent) or none is set on this torrent.
ALTER TABLE shared_items ADD COLUMN achieved_ratio REAL;
ALTER TABLE shared_items ADD COLUMN ratio_limit_reported REAL;
