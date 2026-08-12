//! Generating and comparing the secrets sharerr mints for itself.
//!
//! Three consumers needed the same two operations and had grown their own copies:
//! the session table and the settings page both minted random hex, and the tracker
//! and the Torznab endpoint both compared a supplied secret against a stored one.
//! They live here so the *properties* — a single entropy source, and a comparison
//! that does not short-circuit — hold everywhere rather than per call site.

/// A fresh secret: `bytes` bytes of entropy, hex encoded.
///
/// Same source the vault uses for its salts and nonces. Hex rather than base64 so
/// the result survives being pasted into a URL, a config file, or another app's
/// settings box without escaping.
pub fn random_hex(bytes: usize) -> Result<String, String> {
    let mut raw = vec![0u8; bytes];
    getrandom::fill(&mut raw).map_err(|err| format!("could not generate a secret: {err}"))?;
    Ok(hex::encode(raw))
}

/// Compare two secrets without short-circuiting on the first difference.
///
/// A timing attack against a tracker token over a home connection is not a
/// realistic threat, and this is not here because it is. It is here because `==`
/// on a secret is the kind of line that gets copied into somewhere it *does*
/// matter, and because the constant-time version costs nothing.
///
/// Note the length comparison is not constant-time and cannot be: differing
/// lengths must be rejected, and that fact is observable however it is written.
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |differences, (x, y)| differences | (x ^ y))
            == 0
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn a_generated_secret_is_hex_of_the_requested_width() {
        let key = random_hex(20).unwrap();
        assert_eq!(key.len(), 40, "one byte renders as two hex digits");
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));

        assert_eq!(random_hex(32).unwrap().len(), 64);
    }

    #[test]
    fn two_secrets_are_never_the_same() {
        assert_ne!(random_hex(32).unwrap(), random_hex(32).unwrap());
    }

    #[test]
    fn comparison_accepts_only_an_exact_match() {
        assert!(constant_time_eq("s3cret", "s3cret"));
        assert!(constant_time_eq("", ""));

        for wrong in ["", "s3cre", "s3crets", "S3CRET", "s3crey"] {
            assert!(!constant_time_eq("s3cret", wrong), "{wrong:?} was accepted");
        }
    }
}
