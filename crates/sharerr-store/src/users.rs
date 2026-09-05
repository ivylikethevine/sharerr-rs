//! Web UI login accounts.
//!
//! Passwords are stored as Argon2id PHC strings in SQLite, never in the vault —
//! see the comment atop `migrations/0002_users.sql` for why that is deliberate.
//!
//! Every hashing operation runs on [`tokio::task::spawn_blocking`]. Argon2 at the
//! default parameters is ~19 MiB of memory and tens of milliseconds of solid CPU;
//! a container pinned to one CPU has exactly one runtime worker, and doing this
//! inline would stall `/health` for the duration of a login attempt.

use std::sync::LazyLock;
use std::time::Duration;

use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::{Algorithm, Argon2, Params, Version};
use secrecy::{ExposeSecret, SecretString};
use sqlx::Row;
use tokio::sync::Semaphore;
use zeroize::Zeroizing;

use sharerr_core::endpoint::now_epoch;

use crate::db::{Store, StoreError};

type Result<T> = std::result::Result<T, StoreError>;

/// Length of the raw salt before base64 encoding. 16 bytes is the Argon2
/// reference implementation's recommendation and matches the vault's salt.
const SALT_LEN: usize = 16;

/// Argon2id cost parameters for login password hashing. Spelled out
/// explicitly for the same reason as `sharerr-store::vault`'s identical
/// constants — these currently equal the argon2 crate's own default, but a
/// crate-bump default is implicit where this is not. Unlike the vault's
/// parameters, these are safe to change freely: a PHC string embeds the
/// parameters it was hashed with, and verification reads that embedded
/// value rather than trusting whatever `hasher()` below is configured with
/// — see `blocking_verify`'s doc comment. A change only affects passwords
/// hashed *after* it lands.
const ARGON2_M_COST_KIB: u32 = 19 * 1024;
const ARGON2_T_COST: u32 = 2;
const ARGON2_P_COST: u32 = 1;

/// Built once, at compile time: `Params::new` only fails outside its own
/// MIN/MAX bounds, which the fixed constants above never approach, so a
/// bound violation here is a compile error rather than something that could
/// ever surface at runtime.
const ARGON2_PARAMS: Params =
    match Params::new(ARGON2_M_COST_KIB, ARGON2_T_COST, ARGON2_P_COST, None) {
        Ok(params) => params,
        Err(_) => panic!("hard-coded Argon2 params must satisfy argon2's own bounds"),
    };

fn hasher() -> Argon2<'static> {
    Argon2::new(Algorithm::Argon2id, Version::V0x13, ARGON2_PARAMS.clone())
}

impl Store {
    /// How many accounts exist. Zero means the instance is unclaimed and the
    /// first-run setup page should be reachable.
    pub async fn user_count(&self) -> Result<i64> {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM users")
            .fetch_one(self.pool())
            .await?;
        Ok(row.try_get::<i64, _>("n")?)
    }

    /// Create an account. Fails if the username is taken.
    pub async fn create_user(&self, username: &str, password: &SecretString) -> Result<i64> {
        let username = validate_username(username)?;
        validate_password(password)?;
        let hash = hash_password(password).await?;
        let now = now_epoch();

        // The UNIQUE constraint is what actually decides this, not a prior SELECT:
        // two setup form submissions racing each other would both pass a check-then-
        // insert, and only one can win the constraint.
        let row = sqlx::query(
            "INSERT INTO users (username, password_hash, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?3) RETURNING id",
        )
        .bind(&username)
        .bind(&hash)
        .bind(now)
        .fetch_one(self.pool())
        .await
        .map_err(|err| match &err {
            sqlx::Error::Database(db) if db.is_unique_violation() => StoreError::UserExists {
                username: username.clone(),
            },
            _ => StoreError::Sql(err),
        })?;

        Ok(row.try_get::<i64, _>("id")?)
    }

    /// Check a login. `false` covers both a wrong password and an unknown user —
    /// the caller must not distinguish them in what it renders.
    pub async fn verify_password(&self, username: &str, password: &SecretString) -> Result<bool> {
        let row = sqlx::query("SELECT password_hash FROM users WHERE username = ?1")
            .bind(username)
            .fetch_optional(self.pool())
            .await?;

        let stored = match &row {
            Some(row) => Some(row.try_get::<String, _>("password_hash")?),
            None => None,
        };

        // An unknown username still pays for a full Argon2 verification against a
        // throwaway hash. Returning early would make "no such user" measurably
        // faster than "wrong password", which turns the login form into a way to
        // enumerate account names. The decoy is substituted inside the blocking
        // task, so forcing it never costs the runtime worker a hash.
        let verified = verify_password(password, stored).await?;
        Ok(verified && row.is_some())
    }

    /// Replace an account's password. Returns whether the account existed.
    pub async fn set_password(&self, username: &str, password: &SecretString) -> Result<bool> {
        validate_password(password)?;
        let hash = hash_password(password).await?;

        let affected =
            sqlx::query("UPDATE users SET password_hash = ?1, updated_at = ?2 WHERE username = ?3")
                .bind(&hash)
                .bind(now_epoch())
                .bind(username)
                .execute(self.pool())
                .await?
                .rows_affected();

        Ok(affected > 0)
    }
}

/// A valid Argon2 hash of a password nobody has. Verifying against it costs the
/// same as verifying against a real one, which is the entire point.
static DECOY_HASH: LazyLock<String> = LazyLock::new(|| {
    // A failure here would only degrade the timing defence, never break login, so
    // it falls back to a string the PHC parser inside `verify_password` rejects
    // rather than panicking. Deliberately not a real hash: this string is chosen
    // specifically because that parser rejects it, so nothing can ever verify
    // against it. It is reached only when hashing the decoy password above
    // already failed.
    blocking_hash("decoy — matches nothing").unwrap_or_else(|_| "$argon2id$invalid".to_owned())
});

fn validate_username(username: &str) -> Result<String> {
    crate::db::validate_name(
        username,
        "username must not be blank",
        "username must be 64 characters or fewer",
    )
}

fn validate_password(password: &SecretString) -> Result<()> {
    // A floor, not a policy — the web layer enforces a longer minimum with a
    // message a human can act on. This only guarantees the store never persists a
    // hash of nothing.
    if password.expose_secret().is_empty() {
        return Err(StoreError::InvalidUser("password must not be empty"));
    }
    Ok(())
}

/// Caps how many Argon2 operations run at once, across every caller —
/// `hash_password` and `verify_password` below are the only two ways this
/// crate ever hashes or verifies a password, so every entry point (login,
/// setup, change-password) funnels through here whether or not the caller
/// knows it exists.
///
/// A flood of unauthenticated login POSTs would otherwise turn the blocking
/// pool itself into the amplification surface: each attempt costs ~19 MiB
/// and tens of milliseconds of real CPU (see this module's header comment),
/// including one against an unknown username via [`DECOY_HASH`]. Tokio's
/// blocking pool defaults to 512 threads, so nothing else naturally bounds
/// how many of those run at once; this does. `max(4, _)` so a small container
/// still has enough headroom for a handful of genuinely simultaneous
/// logins — the goal is bounding an unbounded flood, not making ordinary
/// light concurrency (this crate's own parallel test run included) queue on
/// a knife's edge against [`HASH_SLOT_WAIT`].
static HASH_SLOTS: LazyLock<Semaphore> = LazyLock::new(|| {
    let cpus = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    Semaphore::new(cpus.max(4))
});

/// How long a caller waits for a slot before giving up. Bounded on purpose —
/// the point is a request eventually failing under sustained load, not
/// building an unbounded queue behind the blocking pool — but generous
/// enough that ordinary contention among a handful of legitimate concurrent
/// attempts (each tens of milliseconds of real work) clears well within it
/// rather than spuriously refusing one.
const HASH_SLOT_WAIT: Duration = Duration::from_secs(3);

/// The wait-then-fail-fast logic, over any semaphore — split out so tests can
/// exercise it against a small throwaway one instead of racing every other
/// test in this binary for a slot on the real, process-wide [`HASH_SLOTS`].
async fn acquire_slot(
    semaphore: &Semaphore,
    wait: Duration,
) -> Result<tokio::sync::SemaphorePermit<'_>> {
    match tokio::time::timeout(wait, semaphore.acquire()).await {
        Ok(Ok(permit)) => Ok(permit),
        // `close()` is never called on this semaphore, so this arm is
        // unreachable in practice; treated the same as exhaustion rather
        // than unwrapped, since the workspace lints against that.
        Ok(Err(_)) | Err(_) => Err(StoreError::TooBusy),
    }
}

async fn acquire_hash_slot() -> Result<tokio::sync::SemaphorePermit<'static>> {
    acquire_slot(&HASH_SLOTS, HASH_SLOT_WAIT).await
}

async fn hash_password(password: &SecretString) -> Result<String> {
    let _permit = acquire_hash_slot().await?;
    let password = clone_secret(password);
    tokio::task::spawn_blocking(move || blocking_hash(&password))
        .await
        .map_err(|_| StoreError::PasswordHash("hashing task panicked".to_owned()))?
}

/// `None` means no such account: a decoy hash is substituted so the cost matches.
async fn verify_password(password: &SecretString, stored: Option<String>) -> Result<bool> {
    let _permit = acquire_hash_slot().await?;
    let password = clone_secret(password);
    tokio::task::spawn_blocking(move || {
        let stored = stored.unwrap_or_else(|| DECOY_HASH.clone());
        blocking_verify(&password, &stored)
    })
    .await
    .map_err(|_| StoreError::PasswordHash("verification task panicked".to_owned()))
}

/// The plaintext has to be owned to cross into a blocking task; `Zeroizing` is
/// what keeps that copy from outliving the call in freed memory.
fn clone_secret(password: &SecretString) -> Zeroizing<String> {
    Zeroizing::new(password.expose_secret().to_owned())
}

fn blocking_hash(password: &str) -> Result<String> {
    let raw = crate::random_array::<SALT_LEN>()
        .map_err(|e| StoreError::PasswordHash(format!("salt generation failed: {e}")))?;

    hasher()
        .hash_password_with_salt(password.as_bytes(), &raw)
        .map(|hash| hash.to_string())
        .map_err(|e| StoreError::PasswordHash(e.to_string()))
}

/// Plain `bool`, not `Result`: verification has no failure mode distinct from
/// "no". A hash this build cannot parse is a rejected login, not an error --
/// the only ways to get one are a hand-edited database or a downgrade, and
/// neither should hand out a session. A `Result` here would invite a caller to
/// treat an unparsable hash as something to retry or surface.
fn blocking_verify(password: &str, stored: &str) -> bool {
    // `verify_password` parses the PHC string itself and returns `Err` for one
    // it cannot parse, which folds into the same "no" as a wrong password.
    // Verification is checked against the algorithm/version/params *encoded
    // in `stored`*, not `hasher()`'s own — so this reads correctly against a
    // hash produced under a since-changed `ARGON2_*` constant, same as it
    // always has under `Argon2::default()`.
    hasher()
        .verify_password(password.as_bytes(), stored)
        .is_ok()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::time::Instant;

    use super::*;
    use sharerr_testkit::secrets::fresh_password;

    fn secret(s: &str) -> SecretString {
        SecretString::from(s.to_owned())
    }

    async fn store() -> Store {
        Store::open_in_memory().await.expect("in-memory store")
    }

    #[tokio::test]
    async fn a_fresh_instance_has_no_users() {
        let store = store().await;
        assert_eq!(store.user_count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn an_account_round_trips_and_only_the_right_password_verifies() {
        let store = store().await;
        let password = fresh_password();
        store
            .create_user("operator", &secret(&password))
            .await
            .unwrap();

        assert_eq!(store.user_count().await.unwrap(), 1);
        assert!(
            store
                .verify_password("operator", &secret(&password))
                .await
                .unwrap()
        );
        assert!(
            !store
                .verify_password("operator", &secret(&fresh_password()))
                .await
                .unwrap()
        );
        // Deliberately empty, not generated: this is the one case that must
        // exercise the empty-password path specifically, which `fresh_password`
        // can never produce. CodeQL's hard-coded-cryptographic-value query
        // cannot tell that from a real credential reaching this parameter.
        assert!(
            !store
                .verify_password("operator", &secret(""))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn an_unknown_user_is_rejected_without_erroring() {
        let store = store().await;
        let password = fresh_password();
        store
            .create_user("operator", &secret(&password))
            .await
            .unwrap();

        // Must be a plain `false`, not an error — the login handler renders the
        // same message either way, and an error would leak the difference.
        assert!(
            !store
                .verify_password("nobody", &secret(&password))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn the_hash_is_not_the_password() {
        let store = store().await;
        let password = fresh_password();
        store
            .create_user("operator", &secret(&password))
            .await
            .unwrap();

        let stored: String = sqlx::query("SELECT password_hash FROM users")
            .fetch_one(store.pool())
            .await
            .unwrap()
            .try_get("password_hash")
            .unwrap();

        assert!(!stored.contains(&password));
        assert!(stored.starts_with("$argon2id$"), "got {stored}");
    }

    #[tokio::test]
    async fn the_same_password_hashes_differently_each_time() {
        // Per-user salting: two accounts sharing a password must not share a hash,
        // or the database reveals which accounts to attack together.
        let store = store().await;
        let password = fresh_password();
        store.create_user("a", &secret(&password)).await.unwrap();
        store.create_user("b", &secret(&password)).await.unwrap();

        let hashes: Vec<String> = sqlx::query("SELECT password_hash FROM users ORDER BY id")
            .fetch_all(store.pool())
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.try_get("password_hash").unwrap())
            .collect();

        assert_ne!(hashes[0], hashes[1]);
    }

    #[tokio::test]
    async fn a_duplicate_username_is_named_rather_than_a_raw_sql_error() {
        let store = store().await;
        store
            .create_user("operator", &secret(&fresh_password()))
            .await
            .unwrap();

        let err = store
            .create_user("operator", &secret(&fresh_password()))
            .await
            .unwrap_err();
        assert!(
            matches!(&err, StoreError::UserExists { username } if username == "operator"),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn usernames_are_trimmed_and_blanks_refused() {
        let store = store().await;
        let password = fresh_password();
        store
            .create_user("  operator  ", &secret(&password))
            .await
            .unwrap();
        assert!(
            store
                .verify_password("operator", &secret(&password))
                .await
                .unwrap()
        );

        for blank in ["", "   ", "\t\n"] {
            assert!(
                matches!(
                    store.create_user(blank, &secret(&fresh_password())).await,
                    Err(StoreError::InvalidUser(_))
                ),
                "{blank:?} should be refused"
            );
        }
        // Deliberately empty, same reasoning as the assertion above — this
        // is the empty-password rejection itself, not a stand-in credential.
        assert!(matches!(
            store.create_user("someone", &secret("")).await,
            Err(StoreError::InvalidUser(_))
        ));
    }

    #[tokio::test]
    async fn changing_a_password_invalidates_the_old_one() {
        let store = store().await;
        let old = fresh_password();
        let new = fresh_password();
        store.create_user("operator", &secret(&old)).await.unwrap();

        assert!(store.set_password("operator", &secret(&new)).await.unwrap());
        assert!(
            !store
                .verify_password("operator", &secret(&old))
                .await
                .unwrap()
        );
        assert!(
            store
                .verify_password("operator", &secret(&new))
                .await
                .unwrap()
        );

        assert!(
            !store
                .set_password("nobody", &secret(&fresh_password()))
                .await
                .unwrap(),
            "an absent account reports false rather than erroring"
        );
    }

    #[tokio::test]
    async fn user_count_tracks_every_account() {
        let store = store().await;
        store
            .create_user("first", &secret(&fresh_password()))
            .await
            .unwrap();
        store
            .create_user("second", &secret(&fresh_password()))
            .await
            .unwrap();
        assert_eq!(store.user_count().await.unwrap(), 2);
    }

    /// The mechanism `acquire_hash_slot` builds on, isolated to a throwaway
    /// semaphore rather than the real process-wide `HASH_SLOTS` — every other
    /// test above also hashes or verifies a password, so asserting against
    /// the shared static here would mean racing them for a slot.
    #[tokio::test]
    async fn a_saturated_semaphore_fails_fast_rather_than_queuing() {
        let semaphore = Semaphore::new(1);
        let _held = semaphore.acquire().await.unwrap();

        let start = Instant::now();
        let result = acquire_slot(&semaphore, Duration::from_millis(50)).await;

        assert!(matches!(result, Err(StoreError::TooBusy)), "{result:?}");
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "must fail fast once the wait expires, not hang: {:?}",
            start.elapsed()
        );
    }

    #[tokio::test]
    async fn a_freed_slot_lets_the_next_request_through() {
        let semaphore = Semaphore::new(1);
        {
            let _held = semaphore.acquire().await.unwrap();
            // Freed at the end of this block, before anyone else asks.
        }

        assert!(
            acquire_slot(&semaphore, Duration::from_millis(50))
                .await
                .is_ok()
        );
    }

    /// The semaphore must not break ordinary concurrent use — several logins
    /// at once, comfortably inside the real cap, all still resolve correctly
    /// rather than serialising into failures.
    #[tokio::test]
    async fn concurrent_correct_logins_all_succeed() {
        let store = store().await;
        let password = fresh_password();
        store
            .create_user("operator", &secret(&password))
            .await
            .unwrap();

        let attempts: Vec<_> = (0..8)
            .map(|_| {
                let store = store.clone();
                let password = secret(&password);
                tokio::spawn(async move { store.verify_password("operator", &password).await })
            })
            .collect();

        for attempt in attempts {
            assert!(
                attempt.await.unwrap().unwrap(),
                "every correct login must still verify"
            );
        }
    }
}
