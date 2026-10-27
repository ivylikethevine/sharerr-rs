-- When a share ends on its own, as Unix seconds: the start of the chosen day
-- in the container's time zone. NULL for a share with no end.
--
-- From that moment each sync pass unshares the item (as the Unshare button
-- does, never touching the file) and leaves it unshared while the date
-- stands, even though the file is still tagged. Clearing the date lets the
-- next pass share it again. The *arr app's tag is never edited: a tag covers
-- a whole series, and this is one file.
ALTER TABLE shared_items ADD COLUMN share_until INTEGER;
