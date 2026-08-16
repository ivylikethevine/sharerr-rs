-- Friends this instance shares with, one row each.
--
-- Before this there was exactly one `torznab.api_key` in the vault, handed to
-- every friend. That made two ordinary things impossible: telling who is actually
-- using the feed, and cutting off one person without cutting off everyone. A peer
-- is therefore an identity, not just a secret.
--
-- ## Why the key is hashed here rather than stored in the vault
--
-- The vault holds secrets sharerr must *replay* to other services — it has to send
-- Sonarr's API key to Sonarr, so that value must be recoverable. A peer key is the
-- opposite direction: the peer presents it and sharerr only ever has to recognise
-- it. Nothing needs the original back, so nothing stores it. Losing a peer key
-- means issuing a new one, which is the correct behaviour for a bearer credential.
--
-- It also means the peers list works on an instance with no SHARERR_MASTER_KEY,
-- the same reasoning as `users` in 0002.
--
-- ## Why a plain SHA-256 and not Argon2
--
-- `users.password_hash` is Argon2id because a human password has perhaps 30 bits
-- of entropy and must be made expensive to guess. A peer key is 160 bits from the
-- system CSPRNG, so brute force is already off the table and a slow hash buys
-- nothing against the threat that matters.
--
-- It buys something real against a threat that does: authentication happens on
-- every Torznab query, and a slow hash cannot be looked up by index — it would
-- mean verifying the presented key against every peer row in turn, tens of
-- milliseconds each. A fast hash makes this one indexed equality lookup no matter
-- how many friends there are.
CREATE TABLE peers (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    -- What the operator calls this friend. UNIQUE so the list stays legible; it is
    -- the only handle they have on a credential they can never see again.
    label        TEXT    NOT NULL UNIQUE,
    -- Lowercase hex SHA-256 of the issued key. UNIQUE both to reject an
    -- astronomically unlikely collision and to give the lookup its index.
    key_hash     TEXT    NOT NULL UNIQUE,
    created_at   INTEGER NOT NULL,
    -- NULL until the peer's first authenticated request. This is the column that
    -- answers "is my friend actually set up?", which nothing could answer before.
    last_seen_at INTEGER,
    -- NULL while the peer is active. Revoking keeps the row so the label cannot be
    -- silently reused and the history stays readable; deleting is a separate,
    -- explicit action.
    revoked_at   INTEGER
);
