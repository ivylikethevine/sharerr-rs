//! Fault injection for tests, behind the `fault-injection` feature.
//!
//! A store that never opens, or whose pool a test closes, fails every call
//! at once. The arms this reaches are the ones behind a call that follows a
//! successful one on the same store: a password verified and then not
//! written, a run begun and then not finished, a peer touched and then not
//! recorded. Every clone of a [`Store`] shares one [`Fault`], so a plan set
//! on the handle a test holds applies to the one the code under test holds.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicI64, Ordering};

use sqlx::SqlitePool;

use crate::Store;

/// Countdown: positive counts down, zero fires, negative is disarmed.
const DISARMED: i64 = -1;

#[derive(Debug)]
pub(crate) struct Fault {
    /// Pool acquisitions still answered before every later one hits `dead`.
    pool_calls_left: AtomicI64,
    /// A pool closed before any query ran: every acquisition fails with
    /// `PoolClosed`, the same error a database that went away produces.
    dead: OnceLock<SqlitePool>,
    /// Password hashes still granted a slot before every later one reports
    /// `TooBusy`.
    hash_calls_left: AtomicI64,
}

impl Fault {
    pub(crate) fn new() -> Self {
        Self {
            pool_calls_left: AtomicI64::new(DISARMED),
            dead: OnceLock::new(),
            hash_calls_left: AtomicI64::new(DISARMED),
        }
    }

    /// The pool a query should run on: the real one, or the dead one once
    /// the countdown has fired.
    pub(crate) fn pool<'a>(&'a self, live: &'a SqlitePool) -> &'a SqlitePool {
        if tick(&self.pool_calls_left) {
            self.dead.get().unwrap_or(live)
        } else {
            live
        }
    }

    /// Whether the next password hash reports `TooBusy`.
    pub(crate) fn hash_slot_refused(&self) -> bool {
        tick(&self.hash_calls_left)
    }
}

/// Advance a countdown; `true` once it has reached zero. A disarmed
/// countdown never fires, and a fired one stays fired.
fn tick(counter: &AtomicI64) -> bool {
    let mut current = counter.load(Ordering::SeqCst);
    loop {
        if current <= 0 {
            return current != DISARMED;
        }
        match counter.compare_exchange(current, current - 1, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return false,
            Err(actual) => current = actual,
        }
    }
}

impl Store {
    /// Answer `answered` more pool acquisitions, then fail every later one
    /// as a closed pool would. `answered` counts acquisitions, which is one
    /// per query for every method on this type.
    pub async fn fail_after(&self, answered: usize) -> Result<(), crate::StoreError> {
        if self.fault.dead.get().is_none() {
            let dead = sqlx::sqlite::SqlitePoolOptions::new()
                .connect("sqlite::memory:")
                .await?;
            dead.close().await;
            let _ = self.fault.dead.set(dead);
        }
        self.fault.pool_calls_left.store(
            i64::try_from(answered).unwrap_or(i64::MAX),
            Ordering::SeqCst,
        );
        Ok(())
    }

    /// Grant `granted` more password hashes a slot, then report `TooBusy`
    /// on every later one, with no wait: the one way to reach that arm from
    /// a test, since the real slots are process-wide and shared with every
    /// other test in the binary.
    pub fn busy_after(&self, granted: usize) {
        self.fault
            .hash_calls_left
            .store(i64::try_from(granted).unwrap_or(i64::MAX), Ordering::SeqCst);
    }
}
