-- What narrows one friend's feed beyond their scope.
--
-- Caps on the friend: the largest file and the highest resolution they are
-- offered. NULL means no cap.
ALTER TABLE peers ADD COLUMN max_size_bytes INTEGER;
ALTER TABLE peers ADD COLUMN max_height INTEGER;

-- Which friends an item is for, when it carries only `<tag>-<friend>` tags: rows
-- here mean those friends alone, and no rows mean everyone their scope admits.
-- `friend` is the name in the tag rather than a peer id, because a tag can name a
-- friend not yet added, and the *arr app is the source of truth on each pass.
CREATE TABLE item_audience (
    source  TEXT NOT NULL,
    file_id INTEGER NOT NULL,
    friend  TEXT NOT NULL,
    PRIMARY KEY (source, file_id, friend)
);
