-- A second sign-in factor: an RFC 6238 time-based one-time password.
--
-- In this table rather than the vault, for the reason 0002 gives for the
-- password hash: sign-in must work on an instance that has no master key yet.
-- Unlike the hash, the secret is stored readable, because checking a code
-- means computing one from it, so a copy of this database exposes it. TOTP
-- here defends against a guessed or reused password, not against someone who
-- already holds the data directory.
--
-- `totp_secret` is the base32 secret the authenticator app was given, NULL
-- while the factor is off. `totp_last_step` is the 30-second step of the last
-- code accepted; a code is only accepted for a later step, so one seen over
-- someone's shoulder cannot be replayed.
ALTER TABLE users ADD COLUMN totp_secret TEXT;
ALTER TABLE users ADD COLUMN totp_last_step INTEGER;
