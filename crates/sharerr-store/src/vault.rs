//! Encrypted credential vault.
//!
//! # Threat model
//!
//! The vault protects API keys **at rest** — in the data volume, in backups, and
//! in snapshots of the container filesystem. It does *not* protect against an
//! attacker who can already read the process environment or memory, because the
//! master key has to reach a headless container somehow. That is the honest
//! ceiling for an unattended service, and it is stated plainly in the operator
//! docs rather than implied away.
//!
//! # Format
//!
//! ```text
//! byte 0        version (currently 1)
//! bytes 1..17   Argon2id salt (16 bytes)
//! bytes 17..    records, repeated:
//!                 u16 LE  key length
//!                 bytes   key (UTF-8)
//!                 24      XChaCha20-Poly1305 nonce
//!                 u32 LE  ciphertext length
//!                 bytes   ciphertext || 16-byte Poly1305 tag
//! ```
//!
//! Each record is sealed with its own random 192-bit nonce — random nonces are
//! safe at this width, which is the reason for XChaCha20 over ChaCha20. The
//! record's key is passed as AAD, so a value cannot be moved from one key to
//! another without detection.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use secrecy::{ExposeSecret, SecretBox, SecretString};
use zeroize::{Zeroize, Zeroizing};

const VERSION: u8 = 1;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const KEY_LEN: usize = 32;

/// Environment variable holding the master key directly.
pub const ENV_MASTER_KEY: &str = "SHARERR_MASTER_KEY";
/// Environment variable pointing at a file holding the master key (Docker secret).
pub const ENV_MASTER_KEY_FILE: &str = "SHARERR_MASTER_KEY_FILE";

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error(
        "no master key: set {ENV_MASTER_KEY} or {ENV_MASTER_KEY_FILE}. \
         sharerr will not fall back to storing credentials in plaintext"
    )]
    NoMasterKey,

    #[error("master key is empty")]
    EmptyMasterKey,

    #[error("could not read master key file {path}: {source}")]
    MasterKeyFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("vault I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("vault is corrupt or truncated: {0}")]
    Corrupt(&'static str),

    #[error("unsupported vault version {found} (this build understands {VERSION})")]
    UnsupportedVersion { found: u8 },

    #[error(
        "could not decrypt the vault — the master key is wrong, or the vault \
         belongs to a different deployment"
    )]
    WrongKey,

    #[error("key derivation failed: {0}")]
    Kdf(String),

    #[error("value for {key:?} is not valid UTF-8")]
    NotUtf8 { key: String },
}

type Result<T> = std::result::Result<T, VaultError>;

/// Read the master key from the environment.
///
/// `SHARERR_MASTER_KEY` takes precedence; otherwise `SHARERR_MASTER_KEY_FILE` is
/// read (trailing newline trimmed, so `echo secret > keyfile` works as expected).
pub fn master_key_from_env() -> Result<SecretString> {
    master_key_from(
        std::env::var(ENV_MASTER_KEY).ok(),
        std::env::var(ENV_MASTER_KEY_FILE).ok(),
    )
}

/// The env-var logic, split out so it is testable without mutating the process
/// environment (which no parallel test can do safely).
fn master_key_from(inline: Option<String>, file: Option<String>) -> Result<SecretString> {
    // A variable set to the empty string counts as unset. `SHARERR_MASTER_KEY:
    // ${SHARERR_MASTER_KEY}` in a compose file with the host variable undefined
    // produces exactly that, and treating it as "present but empty" would mask a
    // perfectly good SHARERR_MASTER_KEY_FILE and report the wrong problem.
    if let Some(raw) = inline.filter(|v| !v.trim().is_empty()) {
        return normalize_master_key(&raw);
    }

    if let Some(path) = file.filter(|v| !v.trim().is_empty()) {
        let path = PathBuf::from(path);
        let raw = std::fs::read_to_string(&path).map_err(|source| VaultError::MasterKeyFile {
            path: path.clone(),
            source,
        })?;
        return normalize_master_key(&raw);
    }

    Err(VaultError::NoMasterKey)
}

/// Trim surrounding whitespace and reject an empty result.
///
/// Trimming matters for the file form: `echo secret > keyfile` leaves a trailing
/// newline, and silently deriving a different key from it would be baffling.
fn normalize_master_key(raw: &str) -> Result<SecretString> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(VaultError::EmptyMasterKey);
    }
    Ok(SecretString::from(trimmed.to_owned()))
}

/// A sealed record as held in memory. Values stay encrypted until `get` is called.
#[derive(Clone)]
struct Record {
    nonce: [u8; NONCE_LEN],
    ciphertext: Vec<u8>,
}

/// The encrypted credential store.
///
/// Holds the secrets sharerr must *replay* to other services, which is why they
/// are recoverable rather than hashed. Opening one costs an Argon2 derivation, so
/// callers should open it once and reuse it.
pub struct Vault {
    path: PathBuf,
    salt: [u8; SALT_LEN],
    cipher: XChaCha20Poly1305,
    records: BTreeMap<String, Record>,
}

// Manual impl: the derived one would print the cipher's key material.
impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vault")
            .field("path", &self.path)
            .field("keys", &self.records.keys().collect::<Vec<_>>())
            .field("cipher", &"<redacted>")
            .finish()
    }
}

impl Vault {
    /// Open an existing vault, or create an empty one if the file is absent.
    pub fn open(path: impl Into<PathBuf>, master: &SecretString) -> Result<Self> {
        let path = path.into();

        let raw = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut salt = [0u8; SALT_LEN];
                getrandom::fill(&mut salt)
                    .map_err(|e| VaultError::Kdf(format!("salt generation failed: {e}")))?;
                let cipher = derive_cipher(master, &salt)?;
                return Ok(Self {
                    path,
                    salt,
                    cipher,
                    records: BTreeMap::new(),
                });
            }
            Err(source) => return Err(VaultError::Io { path, source }),
        };

        let (salt, records) = parse(&raw)?;
        let cipher = derive_cipher(master, &salt)?;
        let vault = Self {
            path,
            salt,
            cipher,
            records,
        };

        // Verify the master key now rather than at first use, so a wrong key is a
        // clear startup failure instead of a confusing error mid-sync.
        if let Some(key) = vault.records.keys().next() {
            vault.get(key)?;
        }

        Ok(vault)
    }

    /// Decrypt a single value.
    pub fn get(&self, key: &str) -> Result<Option<SecretString>> {
        let Some(record) = self.records.get(key) else {
            return Ok(None);
        };

        let plaintext = Zeroizing::new(
            self.cipher
                .decrypt(
                    &XNonce::from(record.nonce),
                    Payload {
                        msg: &record.ciphertext,
                        aad: key.as_bytes(),
                    },
                )
                .map_err(|_| VaultError::WrongKey)?,
        );

        let text = std::str::from_utf8(&plaintext).map_err(|_| VaultError::NotUtf8 {
            key: key.to_owned(),
        })?;
        Ok(Some(SecretString::from(text.to_owned())))
    }

    /// Store a value, replacing any existing one, and persist immediately.
    pub fn put(&mut self, key: &str, value: &SecretString) -> Result<()> {
        let record = self.seal(key, value.expose_secret().as_bytes())?;
        self.records.insert(key.to_owned(), record);
        self.persist()
    }

    /// Remove a value. Returns whether it was present.
    pub fn remove(&mut self, key: &str) -> Result<bool> {
        let existed = self.records.remove(key).is_some();
        if existed {
            self.persist()?;
        }
        Ok(existed)
    }

    /// Every key stored, sorted. Never the values.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.records.keys().map(String::as_str)
    }

    /// The names of the stored secrets, without opening the vault.
    ///
    /// Record keys are cleartext in the file format by design (see the module
    /// header), so listing them needs no master key and, crucially, no Argon2
    /// derivation — which is ~16ms of solid CPU and 19 MiB. The web UI asks this
    /// question on every settings render purely to show "stored" or "not set"
    /// beside each field; going through [`Self::open`] for it made drawing a page
    /// as expensive as authenticating.
    ///
    /// Deliberately reports names even when the master key is absent or wrong: a
    /// secret that cannot currently be decrypted is still *stored*, and telling
    /// the operator otherwise would invite them to overwrite it.
    pub fn key_names(path: &Path) -> Result<Vec<String>> {
        let raw = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(VaultError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };

        Ok(parse(&raw)?.1.into_keys().collect())
    }

    /// Whether anything is stored yet — true on a fresh instance.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Re-encrypt every record under a new master key, with a fresh salt.
    pub fn rotate_master_key(&mut self, new_master: &SecretString) -> Result<()> {
        // Decrypt everything under the old key first; if any record fails we
        // abort before touching disk.
        let mut plaintexts: Vec<(String, SecretString)> = Vec::new();
        for key in self.records.keys().cloned().collect::<Vec<_>>() {
            let value = self
                .get(&key)?
                .ok_or(VaultError::Corrupt("record vanished mid-rotation"))?;
            plaintexts.push((key, value));
        }

        let mut salt = [0u8; SALT_LEN];
        getrandom::fill(&mut salt)
            .map_err(|e| VaultError::Kdf(format!("salt generation failed: {e}")))?;
        let cipher = derive_cipher(new_master, &salt)?;

        self.salt = salt;
        self.cipher = cipher;
        self.records.clear();
        for (key, value) in plaintexts {
            let record = self.seal(&key, value.expose_secret().as_bytes())?;
            self.records.insert(key, record);
        }
        self.persist()
    }

    fn seal(&self, key: &str, plaintext: &[u8]) -> Result<Record> {
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce)
            .map_err(|e| VaultError::Kdf(format!("nonce generation failed: {e}")))?;

        let ciphertext = self
            .cipher
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: plaintext,
                    aad: key.as_bytes(),
                },
            )
            .map_err(|_| VaultError::Corrupt("encryption failed"))?;

        Ok(Record { nonce, ciphertext })
    }

    /// Serialize and write atomically: a crash mid-write leaves the old vault intact.
    fn persist(&self) -> Result<()> {
        let mut out = Vec::with_capacity(64 + self.records.len() * 128);
        out.push(VERSION);
        out.extend_from_slice(&self.salt);

        for (key, record) in &self.records {
            let key_bytes = key.as_bytes();
            let key_len = u16::try_from(key_bytes.len())
                .map_err(|_| VaultError::Corrupt("secret key name too long"))?;
            let ct_len = u32::try_from(record.ciphertext.len())
                .map_err(|_| VaultError::Corrupt("secret value too long"))?;

            out.extend_from_slice(&key_len.to_le_bytes());
            out.extend_from_slice(key_bytes);
            out.extend_from_slice(&record.nonce);
            out.extend_from_slice(&ct_len.to_le_bytes());
            out.extend_from_slice(&record.ciphertext);
        }

        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| VaultError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, &out).map_err(|source| VaultError::Io {
            path: tmp.clone(),
            source,
        })?;
        restrict_permissions(&tmp)?;
        std::fs::rename(&tmp, &self.path).map_err(|source| VaultError::Io {
            path: self.path.clone(),
            source,
        })?;

        out.zeroize();
        Ok(())
    }
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|source| {
        VaultError::Io {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

fn derive_cipher(master: &SecretString, salt: &[u8; SALT_LEN]) -> Result<XChaCha20Poly1305> {
    use argon2::{Algorithm, Argon2, Params, Version};

    let params = Params::default();
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);

    let mut key = SecretBox::new(Box::new([0u8; KEY_LEN]));
    argon
        .hash_password_into(
            master.expose_secret().as_bytes(),
            salt,
            secrecy::ExposeSecretMut::expose_secret_mut(&mut key),
        )
        .map_err(|e| VaultError::Kdf(e.to_string()))?;

    Ok(XChaCha20Poly1305::new(key.expose_secret().into()))
}

fn parse(raw: &[u8]) -> Result<([u8; SALT_LEN], BTreeMap<String, Record>)> {
    if raw.len() < 1 + SALT_LEN {
        return Err(VaultError::Corrupt("file shorter than header"));
    }
    if raw[0] != VERSION {
        return Err(VaultError::UnsupportedVersion { found: raw[0] });
    }

    let mut salt = [0u8; SALT_LEN];
    salt.copy_from_slice(&raw[1..1 + SALT_LEN]);

    let mut records = BTreeMap::new();
    let mut cursor = 1 + SALT_LEN;

    while cursor < raw.len() {
        let key_len = read_u16(raw, &mut cursor)? as usize;
        let key_bytes = take(raw, &mut cursor, key_len)?;
        let key = std::str::from_utf8(key_bytes)
            .map_err(|_| VaultError::Corrupt("record key is not UTF-8"))?
            .to_owned();

        let nonce_bytes = take(raw, &mut cursor, NONCE_LEN)?;
        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(nonce_bytes);

        let ct_len = read_u32(raw, &mut cursor)? as usize;
        let ciphertext = take(raw, &mut cursor, ct_len)?.to_vec();

        records.insert(key, Record { nonce, ciphertext });
    }

    Ok((salt, records))
}

fn take<'a>(raw: &'a [u8], cursor: &mut usize, len: usize) -> Result<&'a [u8]> {
    let end = cursor
        .checked_add(len)
        .ok_or(VaultError::Corrupt("length overflow"))?;
    let slice = raw
        .get(*cursor..end)
        .ok_or(VaultError::Corrupt("record truncated"))?;
    *cursor = end;
    Ok(slice)
}

fn read_u16(raw: &[u8], cursor: &mut usize) -> Result<u16> {
    let bytes = take(raw, cursor, 2)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32(raw: &[u8], cursor: &mut usize) -> Result<u32> {
    let bytes = take(raw, cursor, 4)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn secret(s: &str) -> SecretString {
        SecretString::from(s.to_owned())
    }

    fn vault_in(dir: &TempDir, master: &str) -> Vault {
        Vault::open(dir.path().join("vault.bin"), &secret(master)).expect("vault opens")
    }

    #[test]
    fn absent_file_yields_an_empty_vault() {
        let dir = TempDir::new().unwrap();
        let vault = vault_in(&dir, "master");
        assert!(vault.is_empty());
        assert!(vault.get("sonarr.api_key").unwrap().is_none());
    }

    #[test]
    fn values_round_trip_and_survive_reopening() {
        let dir = TempDir::new().unwrap();
        {
            let mut vault = vault_in(&dir, "correct horse");
            vault.put("sonarr.api_key", &secret("abc123")).unwrap();
            vault
                .put("qbittorrent.password", &secret("hunter2"))
                .unwrap();
        }

        // Simulates a container restart: same file, freshly derived key.
        let vault = vault_in(&dir, "correct horse");
        assert_eq!(
            vault
                .get("sonarr.api_key")
                .unwrap()
                .unwrap()
                .expose_secret(),
            "abc123"
        );
        assert_eq!(
            vault
                .get("qbittorrent.password")
                .unwrap()
                .unwrap()
                .expose_secret(),
            "hunter2"
        );
        assert_eq!(
            vault.keys().collect::<Vec<_>>(),
            vec!["qbittorrent.password", "sonarr.api_key"]
        );
    }

    #[test]
    fn key_names_are_readable_without_the_master_key() {
        // The settings page draws "stored"/"not set" from this on every render, so
        // it must not cost an Argon2 derivation — and it must still tell the truth
        // when the master key is missing, or the UI would invite an operator to
        // overwrite a secret it merely could not decrypt.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("vault.bin");

        assert!(
            Vault::key_names(&path).unwrap().is_empty(),
            "an absent vault has no keys, and is not an error"
        );

        let mut vault = vault_in(&dir, "master");
        vault.put("sonarr.api_key", &secret("v")).unwrap();
        vault.put("qbittorrent.password", &secret("v")).unwrap();

        assert_eq!(
            Vault::key_names(&path).unwrap(),
            vec!["qbittorrent.password", "sonarr.api_key"],
            "no master key involved"
        );
    }

    #[test]
    fn wrong_master_key_is_rejected_at_open() {
        let dir = TempDir::new().unwrap();
        vault_in(&dir, "right").put("k", &secret("v")).unwrap();

        let err = Vault::open(dir.path().join("vault.bin"), &secret("wrong"))
            .expect_err("a wrong master key must not open the vault");
        assert!(matches!(err, VaultError::WrongKey), "got {err:?}");
    }

    #[test]
    fn plaintext_never_appears_on_disk() {
        let dir = TempDir::new().unwrap();
        let mut vault = vault_in(&dir, "master");
        vault
            .put("sonarr.api_key", &secret("SUPERSECRETVALUE"))
            .unwrap();

        let raw = std::fs::read(dir.path().join("vault.bin")).unwrap();
        assert!(
            !raw.windows(16).any(|w| w == b"SUPERSECRETVALUE"),
            "secret value found in cleartext on disk"
        );
        // The key *name* is intentionally cleartext — it is not sensitive and
        // keeps the format inspectable without the master key.
        assert!(
            raw.windows(14).any(|w| w == b"sonarr.api_key"),
            "key names should remain readable"
        );
    }

    #[test]
    fn repeated_writes_of_the_same_value_produce_different_ciphertext() {
        let dir = TempDir::new().unwrap();
        let mut vault = vault_in(&dir, "master");

        vault.put("k", &secret("same")).unwrap();
        let first = vault.records.get("k").unwrap().clone();
        vault.put("k", &secret("same")).unwrap();
        let second = vault.records.get("k").unwrap().clone();

        assert_ne!(first.nonce, second.nonce, "nonce must be fresh per write");
        assert_ne!(first.ciphertext, second.ciphertext);
    }

    #[test]
    fn a_value_cannot_be_moved_between_keys() {
        // The record key is passed as AAD, so relocating a sealed value should
        // fail authentication rather than silently decrypt under the wrong name.
        let dir = TempDir::new().unwrap();
        let mut vault = vault_in(&dir, "master");
        vault
            .put("sonarr.api_key", &secret("sonarr-value"))
            .unwrap();
        vault
            .put("radarr.api_key", &secret("radarr-value"))
            .unwrap();

        let stolen = vault.records.get("sonarr.api_key").unwrap().clone();
        vault.records.insert("radarr.api_key".to_owned(), stolen);

        let err = vault
            .get("radarr.api_key")
            .expect_err("AAD mismatch must be detected");
        assert!(matches!(err, VaultError::WrongKey), "got {err:?}");
    }

    #[test]
    fn remove_deletes_and_reports_presence() {
        let dir = TempDir::new().unwrap();
        let mut vault = vault_in(&dir, "master");
        vault.put("k", &secret("v")).unwrap();

        assert!(vault.remove("k").unwrap());
        assert!(!vault.remove("k").unwrap(), "removing twice reports absent");
        assert!(
            vault_in(&dir, "master").get("k").unwrap().is_none(),
            "removal persisted"
        );
    }

    #[test]
    fn rotating_the_master_key_reencrypts_everything() {
        let dir = TempDir::new().unwrap();
        let mut vault = vault_in(&dir, "old master");
        vault.put("a", &secret("value-a")).unwrap();
        vault.put("b", &secret("value-b")).unwrap();
        let salt_before = vault.salt;

        vault.rotate_master_key(&secret("new master")).unwrap();
        assert_ne!(salt_before, vault.salt, "rotation must use a fresh salt");

        assert!(
            matches!(
                Vault::open(dir.path().join("vault.bin"), &secret("old master")),
                Err(VaultError::WrongKey)
            ),
            "old master key must stop working"
        );

        let rotated = vault_in(&dir, "new master");
        assert_eq!(
            rotated.get("a").unwrap().unwrap().expose_secret(),
            "value-a"
        );
        assert_eq!(
            rotated.get("b").unwrap().unwrap().expose_secret(),
            "value-b"
        );
    }

    #[test]
    fn truncated_and_mislabelled_files_are_detected() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("vault.bin");
        let mut vault = vault_in(&dir, "master");
        vault.put("k", &secret("v")).unwrap();

        let good = std::fs::read(&path).unwrap();

        std::fs::write(&path, &good[..good.len() - 4]).unwrap();
        assert!(matches!(
            Vault::open(&path, &secret("master")),
            Err(VaultError::Corrupt(_))
        ));

        let mut wrong_version = good.clone();
        wrong_version[0] = 99;
        std::fs::write(&path, &wrong_version).unwrap();
        assert!(matches!(
            Vault::open(&path, &secret("master")),
            Err(VaultError::UnsupportedVersion { found: 99 })
        ));

        std::fs::write(&path, b"xx").unwrap();
        assert!(matches!(
            Vault::open(&path, &secret("master")),
            Err(VaultError::Corrupt(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn vault_file_is_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let mut vault = vault_in(&dir, "master");
        vault.put("k", &secret("v")).unwrap();

        let mode = std::fs::metadata(dir.path().join("vault.bin"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o077,
            0,
            "vault must not be group- or world-accessible"
        );
    }

    #[test]
    fn debug_output_does_not_leak_key_material() {
        let dir = TempDir::new().unwrap();
        let mut vault = vault_in(&dir, "master");
        vault
            .put("sonarr.api_key", &secret("SUPERSECRETVALUE"))
            .unwrap();

        let rendered = format!("{vault:?}");
        assert!(!rendered.contains("SUPERSECRETVALUE"));
        assert!(rendered.contains("<redacted>"));
        assert!(
            rendered.contains("sonarr.api_key"),
            "key names are useful in logs"
        );
    }

    #[test]
    fn master_keys_are_trimmed_and_empties_rejected() {
        // `echo secret > keyfile` leaves a newline; it must derive the same key.
        assert_eq!(
            normalize_master_key("secret\n").unwrap().expose_secret(),
            "secret"
        );
        assert_eq!(
            normalize_master_key("  secret  ").unwrap().expose_secret(),
            "secret"
        );
        assert!(matches!(
            normalize_master_key("   \n"),
            Err(VaultError::EmptyMasterKey)
        ));
        assert!(matches!(
            normalize_master_key(""),
            Err(VaultError::EmptyMasterKey)
        ));
    }

    #[test]
    fn an_inline_master_key_is_preferred_over_a_file() {
        let key = master_key_from(Some("inline".to_owned()), Some("/nonexistent".to_owned()));
        assert_eq!(key.unwrap().expose_secret(), "inline");
    }

    /// `SHARERR_MASTER_KEY: ${SHARERR_MASTER_KEY}` in a compose file with the host
    /// variable undefined sets it to the empty string. Treating that as "present
    /// but empty" hid a correctly mounted docker secret behind the wrong error.
    #[test]
    fn an_empty_inline_key_falls_through_to_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("master.key");
        std::fs::write(&path, "from-the-file\n").unwrap();

        for blank in ["", "   ", "\n"] {
            let key = master_key_from(
                Some(blank.to_owned()),
                Some(path.to_string_lossy().into_owned()),
            );
            assert_eq!(
                key.unwrap().expose_secret(),
                "from-the-file",
                "a blank inline key ({blank:?}) should not mask the key file"
            );
        }
    }

    #[test]
    fn a_blank_file_path_is_treated_as_unset() {
        assert!(matches!(
            master_key_from(None, Some(String::new())),
            Err(VaultError::NoMasterKey)
        ));
        assert!(matches!(
            master_key_from(None, None),
            Err(VaultError::NoMasterKey)
        ));
    }

    #[test]
    fn a_missing_key_file_names_the_path_it_tried() {
        let err = master_key_from(None, Some("/nonexistent/master.key".to_owned())).unwrap_err();
        assert!(
            matches!(err, VaultError::MasterKeyFile { .. }),
            "got {err:?}"
        );
        assert!(err.to_string().contains("/nonexistent/master.key"), "{err}");
    }
}
