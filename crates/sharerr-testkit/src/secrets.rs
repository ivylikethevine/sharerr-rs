//! Throwaway credential generators with no HTTP or database involvement.
//!
//! Split out of [`crate::mock`] — that module's own doc comment scopes it to
//! "shared wiremock scaffolding," which [`fresh_password`] never was: it was
//! added later for the CodeQL hard-coded-cryptographic-value sweep and has
//! nothing to do with an HTTP mock. `sharerr-store`'s login/account tests are
//! the reason this module exists separately — they have no wiremock story of
//! their own, and depending on `mock` just for this one function was the
//! whole reason the doc comment there stopped being true.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// A throwaway account password, fresh per call: an atomic counter
/// concatenated with a nanosecond timestamp, so two calls in the same test
/// binary never collide even if the clock does not advance between them.
///
/// A literal handed to `create_user`/`verify_password` reaches Argon2 and is
/// a `rust/hard-coded-cryptographic-value` finding — the same reasoning
/// `mock::rpc_credentials` documents for a Basic Auth pair reaching
/// `basic_auth`. Always well over any real minimum-length check — a
/// hex-encoded nanosecond timestamp is never under 8 characters in this era
/// — but that is a property of the current epoch, not a compile-time
/// guarantee.
///
/// `unwrap_or_default` on a clock read failure, deliberately over `expect`:
/// uniqueness never rested on the clock in the first place — the counter is
/// concatenated into every value regardless — and unlike a literal fallback
/// (the same taint shape this function exists to avoid), `unwrap_or_default()`
/// leaves no literal in the expression for a taint query to anchor on.
pub fn fresh_password() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);

    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    format!("{stamp:x}{n:x}")
}
