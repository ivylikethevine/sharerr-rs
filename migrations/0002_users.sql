-- Login accounts for the web UI.
--
-- Deliberately in SQLite rather than the encrypted vault. The vault cannot be
-- opened without SHARERR_MASTER_KEY, and the whole point of the web UI is that a
-- container which has not been given that variable yet can still be reached,
-- logged into, and told what is missing. A password hash in the vault would make
-- first-run login impossible on exactly the instance that needs it most.
--
-- That is not a downgrade: `password_hash` is an Argon2id PHC string, which is
-- already a one-way function. The vault protects *recoverable* secrets — API keys
-- sharerr must replay to other services — and a password hash is not one.
CREATE TABLE users (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    -- Compared case-sensitively, as typed. Usernames are chosen in this same UI,
    -- so there is no external system whose casing has to be matched.
    username      TEXT    NOT NULL UNIQUE,
    -- Full PHC string ($argon2id$v=19$m=...,t=...,p=...$salt$hash), not a bare
    -- digest: it carries the salt and the cost parameters, so raising the cost
    -- later does not invalidate existing rows.
    password_hash TEXT    NOT NULL,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
);
