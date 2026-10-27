//! RFC 6238 time-based one-time passwords, the web UI's second sign-in factor.
//!
//! SHA-1, six digits, 30-second steps: what every authenticator app assumes
//! when an `otpauth://` URI names nothing else. HMAC (RFC 2104) and base32
//! (RFC 4648) are written out here rather than pulled in as crates: each is a
//! few lines over the `sha1` the torrent code already uses, and the RFC test
//! vectors below pin both.

use sha1::{Digest, Sha1};

const STEP_SECS: i64 = 30;
const DIGITS: usize = 6;
/// A code one step either side of now is accepted, for a phone clock that
/// has drifted.
const SKEW_STEPS: i64 = 1;
/// 160 bits, RFC 4226's recommended secret length for HMAC-SHA1.
const SECRET_BYTES: usize = 20;
/// The issuer an authenticator app files the entry under.
const ISSUER: &str = "sharerr";

const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// A fresh random secret, base32 without padding.
pub fn generate_secret() -> Result<String, String> {
    Ok(base32_encode(
        &crate::secrets::random_bytes::<SECRET_BYTES>()?,
    ))
}

/// The 30-second step `code` is valid for at Unix time `now`, or `None`.
///
/// Spaces are ignored, since apps display codes as `123 456`. The caller
/// still has to claim the step (`Store::claim_totp_step`) so a code is
/// accepted once.
pub fn verify(secret: &str, code: &str, now: i64) -> Option<i64> {
    let code: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    if code.len() != DIGITS || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let key = base32_decode(secret)?;
    let current = now.div_euclid(STEP_SECS);
    (current - SKEW_STEPS..=current + SKEW_STEPS).find(|&step| {
        u64::try_from(step).is_ok_and(|counter| {
            let expected = format!(
                "{:0DIGITS$}",
                hotp(&key, counter) % 10u32.pow(DIGITS as u32)
            );
            constant_time_eq(expected.as_bytes(), code.as_bytes())
        })
    })
}

/// Whether `secret` is one [`generate_secret`] could have made: what the
/// enrolment form carries back must not become a weaker key on the way.
pub fn is_valid_secret(secret: &str) -> bool {
    base32_decode(secret).is_some_and(|key| key.len() == SECRET_BYTES)
}

/// The `otpauth://` URI an authenticator app enrols from.
pub fn otpauth_uri(username: &str, secret: &str) -> String {
    format!(
        "otpauth://totp/{ISSUER}:{}?secret={secret}&issuer={ISSUER}",
        percent_encode(username)
    )
}

/// `uri` as an inline SVG QR code, rendered here so the page loads nothing.
pub fn qr_svg(uri: &str) -> Result<String, String> {
    let code = qrcode::QrCode::new(uri.as_bytes()).map_err(|err| err.to_string())?;
    Ok(code
        .render::<qrcode::render::svg::Color<'_>>()
        .min_dimensions(200, 200)
        .quiet_zone(true)
        .build())
}

/// The code an authenticator app shows for `secret` at Unix time `now`.
#[cfg(test)]
pub(crate) fn code_at(secret: &str, now: i64) -> String {
    let key = base32_decode(secret).unwrap_or_default();
    let counter = u64::try_from(now.div_euclid(STEP_SECS)).unwrap_or_default();
    format!(
        "{:0DIGITS$}",
        hotp(&key, counter) % 10u32.pow(DIGITS as u32)
    )
}

/// RFC 4226's dynamically truncated HOTP value, before reduction to digits.
fn hotp(key: &[u8], counter: u64) -> u32 {
    let mac = hmac_sha1(key, &counter.to_be_bytes());
    let offset = usize::from(mac[19] & 0x0f);
    u32::from_be_bytes([
        mac[offset] & 0x7f,
        mac[offset + 1],
        mac[offset + 2],
        mac[offset + 3],
    ])
}

/// RFC 2104 HMAC over SHA-1's 64-byte block.
fn hmac_sha1(key: &[u8], message: &[u8]) -> [u8; 20] {
    const BLOCK: usize = 64;
    let mut block = [0u8; BLOCK];
    if key.len() > BLOCK {
        block[..20].copy_from_slice(&Sha1::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha1::new();
    inner.update(block.map(|b| b ^ 0x36));
    inner.update(message);
    let inner = inner.finalize();
    let mut outer = Sha1::new();
    outer.update(block.map(|b| b ^ 0x5c));
    outer.update(inner);
    let mut mac = [0u8; 20];
    mac.copy_from_slice(&outer.finalize());
    mac
}

/// Equal length is public (six digits); the bytes are compared without an
/// early exit.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// RFC 4648 base32, without padding, as `otpauth://` URIs carry it.
fn base32_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let (mut buffer, mut bits) = (0u32, 0u32);
    for &byte in bytes {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(ALPHABET[((buffer >> bits) & 31) as usize]));
        }
        buffer &= (1 << bits) - 1;
    }
    if bits > 0 {
        out.push(char::from(ALPHABET[((buffer << (5 - bits)) & 31) as usize]));
    }
    out
}

/// The inverse, ignoring case, spaces and `=` padding; `None` on any other
/// character.
fn base32_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 5 / 8);
    let (mut buffer, mut bits) = (0u32, 0u32);
    for c in text.chars().filter(|c| !c.is_whitespace() && *c != '=') {
        let value = ALPHABET
            .iter()
            .position(|&a| char::from(a) == c.to_ascii_uppercase())?;
        buffer = (buffer << 5) | value as u32;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// RFC 3986 percent-encoding of everything but the unreserved characters,
/// for the URI's label.
fn percent_encode(text: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// RFC 4226 and 6238's shared test secret, as base32.
    fn rfc_secret() -> String {
        base32_encode(b"12345678901234567890")
    }

    #[test]
    fn hmac_matches_rfc_2202() {
        assert_eq!(
            hex::encode(hmac_sha1(&[0x0b; 20], b"Hi There")),
            "b617318655057264e28bc0b6fb378c8ef146be00"
        );
        // A key longer than the block is hashed first.
        assert_eq!(
            hex::encode(hmac_sha1(
                &[0xaa; 80],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "aa4ae5e15272d00e95705637ce8a3b55ed402112"
        );
    }

    #[test]
    fn hotp_matches_rfc_4226() {
        let expected = [
            755_224, 287_082, 359_152, 969_429, 338_314, 254_676, 287_922, 162_583, 399_871,
            520_489,
        ];
        for (counter, want) in expected.into_iter().enumerate() {
            assert_eq!(
                hotp(b"12345678901234567890", counter as u64) % 1_000_000,
                want,
                "counter {counter}"
            );
        }
    }

    #[test]
    fn totp_matches_rfc_6238() {
        let secret = rfc_secret();
        // The RFC's eight-digit SHA-1 values, cut to the six this uses.
        for (time, code) in [
            (59, "287082"),
            (1_111_111_109, "081804"),
            (1_111_111_111, "050471"),
            (1_234_567_890, "005924"),
            (2_000_000_000, "279037"),
            (20_000_000_000, "353130"),
        ] {
            assert_eq!(
                verify(&secret, code, time),
                Some(time.div_euclid(STEP_SECS)),
                "at {time}"
            );
        }
    }

    #[test]
    fn a_code_is_accepted_one_step_either_side_and_no_further() {
        let secret = rfc_secret();
        // 287082 is the code for step 1 (t = 30..60).
        assert_eq!(verify(&secret, "287082", 59 + 30), Some(1));
        assert_eq!(verify(&secret, "287082", 59 - 30), Some(1));
        assert_eq!(verify(&secret, "287082", 59 + 60), None);
    }

    #[test]
    fn malformed_codes_and_secrets_are_refused() {
        let secret = rfc_secret();
        assert_eq!(
            verify(&secret, "287 082", 59),
            Some(1),
            "spaces are ignored"
        );
        for bad in ["", "28708", "2870822", "28708a", "287082\u{663}"] {
            assert_eq!(verify(&secret, bad, 59), None, "{bad:?}");
        }
        assert_eq!(verify("not base32!", "287082", 59), None);
    }

    #[test]
    fn base32_matches_rfc_4648_and_round_trips() {
        for (raw, encoded) in [
            ("", ""),
            ("f", "MY"),
            ("fo", "MZXQ"), // spellchecker: disable-line
            ("foo", "MZXW6"),
            ("foob", "MZXW6YQ"),
            ("fooba", "MZXW6YTB"),
            ("foobar", "MZXW6YTBOI"),
        ] {
            assert_eq!(base32_encode(raw.as_bytes()), encoded);
            assert_eq!(base32_decode(encoded).unwrap(), raw.as_bytes());
        }
        assert_eq!(base32_decode("mzxw 6ytb oi==").unwrap(), b"foobar");
    }

    #[test]
    fn a_generated_secret_is_160_bits_and_fresh() {
        let a = generate_secret().unwrap();
        assert_eq!(base32_decode(&a).unwrap().len(), SECRET_BYTES);
        assert_ne!(a, generate_secret().unwrap());
    }

    #[test]
    fn the_enrolment_uri_and_qr_carry_the_secret_and_an_encoded_label() {
        let uri = otpauth_uri("ops team", "JBSWY3DPEHPK3PXP");
        assert_eq!(
            uri,
            "otpauth://totp/sharerr:ops%20team?secret=JBSWY3DPEHPK3PXP&issuer=sharerr"
        );
        let svg = qr_svg(&uri).unwrap();
        assert!(svg.starts_with("<?xml") || svg.starts_with("<svg"), "{svg}");
    }
}
