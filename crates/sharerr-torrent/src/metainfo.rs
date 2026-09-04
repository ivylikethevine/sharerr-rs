//! The `.torrent` file (BEP 3 metainfo), read and written just far enough for
//! sharerr's needs.
//!
//! The info dictionary is kept as the decoded [`Value`] it arrived as and
//! re-encoded canonically, never rebuilt from typed fields, so a parse-and-
//! re-encode round trip cannot move the info hash. Only the announce fields
//! are ever rewritten; everything else at the top level rides along untouched.

use std::collections::BTreeMap;

use sha1::{Digest, Sha1};

use crate::bencode::{BencodeError, Value};

/// A parsed `.torrent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Torrent {
    /// The primary tracker URL (`announce`).
    pub announce: Option<String>,
    /// Fallback tiers (`announce-list`, BEP 12), outermost list first.
    pub announce_list: Option<Vec<Vec<String>>>,
    /// The `info` dictionary, verbatim. Its canonical encoding is what the
    /// info hash is computed over.
    pub info: BTreeMap<Vec<u8>, Value>,
    /// Every other top-level field (`comment`, `created by`, ...), verbatim.
    pub extra: BTreeMap<Vec<u8>, Value>,
}

/// Why bytes are not a usable `.torrent`.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MetainfoError {
    #[error("not valid bencode: {0}")]
    Bencode(#[from] BencodeError),
    #[error("the top-level value is not a dictionary")]
    NotADictionary,
    #[error("there is no \"info\" dictionary")]
    MissingInfo,
    #[error("\"announce\" is not a string")]
    AnnounceNotAString,
    #[error("\"announce-list\" is not a list of lists of strings")]
    AnnounceListMalformed,
}

impl Torrent {
    /// Parse a `.torrent` from its bytes.
    pub fn read_from_bytes(bytes: &[u8]) -> Result<Torrent, MetainfoError> {
        let Value::Dict(mut top) = Value::decode(bytes)? else {
            return Err(MetainfoError::NotADictionary);
        };
        let info = match top.remove(b"info".as_slice()) {
            Some(Value::Dict(info)) => info,
            _ => return Err(MetainfoError::MissingInfo),
        };
        let announce = match top.remove(b"announce".as_slice()) {
            None => None,
            Some(value) => Some(
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or(MetainfoError::AnnounceNotAString)?,
            ),
        };
        let announce_list = match top.remove(b"announce-list".as_slice()) {
            None => None,
            Some(value) => Some(parse_announce_list(&value)?),
        };
        Ok(Torrent {
            announce,
            announce_list,
            info,
            extra: top,
        })
    }

    /// Canonical bytes for this torrent.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut top = self.extra.clone();
        if let Some(announce) = &self.announce {
            top.insert(b"announce".to_vec(), Value::string(announce));
        }
        if let Some(tiers) = &self.announce_list {
            top.insert(
                b"announce-list".to_vec(),
                Value::List(
                    tiers
                        .iter()
                        .map(|tier| Value::List(tier.iter().map(|u| Value::string(u)).collect()))
                        .collect(),
                ),
            );
        }
        top.insert(b"info".to_vec(), Value::Dict(self.info.clone()));
        Value::Dict(top).encode()
    }

    /// Lowercase hex SHA-1 of the canonical info dictionary (BEP 3).
    ///
    /// SHA-1 here is the protocol's identifier, not a security decision.
    #[must_use]
    pub fn info_hash(&self) -> String {
        hex::encode(Sha1::digest(Value::Dict(self.info.clone()).encode()))
    }

    fn info_field(&self, key: &str) -> Option<&Value> {
        self.info.get(key.as_bytes())
    }

    /// The `name` inside the info dictionary: the filename a client looks for.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.info_field("name").and_then(Value::as_str)
    }

    /// Total content length. For a single-file torrent this is `length`; for
    /// a multi-file one, the sum of each file's `length`.
    #[must_use]
    pub fn length(&self) -> Option<i64> {
        if let Some(length) = self.info_field("length").and_then(Value::as_int) {
            return Some(length);
        }
        let files = self.info_field("files")?.as_list()?;
        files
            .iter()
            .map(|file| file.as_dict()?.get(b"length".as_slice())?.as_int())
            .try_fold(0i64, |sum, length| sum.checked_add(length?))
    }

    /// True when the info dictionary describes one file (no `files` list).
    #[must_use]
    pub fn is_single_file(&self) -> bool {
        self.info_field("files").is_none()
    }

    /// The BEP 27 private flag.
    #[must_use]
    pub fn is_private(&self) -> bool {
        self.info_field("private").and_then(Value::as_int) == Some(1)
    }

    /// How many 20-byte SHA-1 entries the `pieces` string holds.
    #[must_use]
    pub fn piece_count(&self) -> usize {
        self.info_field("pieces")
            .and_then(Value::as_bytes)
            .map_or(0, |pieces| pieces.len() / 20)
    }
}

fn parse_announce_list(value: &Value) -> Result<Vec<Vec<String>>, MetainfoError> {
    value
        .as_list()
        .ok_or(MetainfoError::AnnounceListMalformed)?
        .iter()
        .map(|tier| {
            tier.as_list()
                .ok_or(MetainfoError::AnnounceListMalformed)?
                .iter()
                .map(|url| {
                    url.as_str()
                        .map(str::to_owned)
                        .ok_or(MetainfoError::AnnounceListMalformed)
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    const MINIMAL: &[u8] =
        b"d8:announce17:http://t/announce4:infod6:lengthi3e4:name5:a.mkv12:piece lengthi16384e6:pieces20:00000000000000000000ee";

    #[test]
    fn parses_the_fields_sharerr_reads() {
        let torrent = Torrent::read_from_bytes(MINIMAL).unwrap();
        assert_eq!(torrent.announce.as_deref(), Some("http://t/announce"));
        assert_eq!(torrent.announce_list, None);
        assert_eq!(torrent.name(), Some("a.mkv"));
        assert_eq!(torrent.length(), Some(3));
        assert!(torrent.is_single_file());
        assert!(!torrent.is_private());
        assert_eq!(torrent.piece_count(), 1);
    }

    #[test]
    fn re_encoding_is_byte_identical_for_canonical_input() {
        let torrent = Torrent::read_from_bytes(MINIMAL).unwrap();
        assert_eq!(torrent.encode(), MINIMAL);
    }

    #[test]
    fn rewriting_announce_fields_leaves_the_info_hash_alone() {
        let mut torrent = Torrent::read_from_bytes(MINIMAL).unwrap();
        let before = torrent.info_hash();
        torrent.announce = Some("http://elsewhere/announce".to_owned());
        torrent.announce_list = Some(vec![
            vec!["http://elsewhere/announce".to_owned()],
            vec!["http://t/announce".to_owned()],
        ]);
        let again = Torrent::read_from_bytes(&torrent.encode()).unwrap();
        assert_eq!(again.info_hash(), before);
        assert_eq!(again.announce_list.unwrap().len(), 2);
    }

    #[test]
    fn unknown_top_level_fields_survive_a_round_trip() {
        let with_comment =
            b"d8:announce17:http://t/announce7:comment5:hello4:infod6:lengthi3e4:name5:a.mkv12:piece lengthi16384e6:pieces20:00000000000000000000ee";
        let torrent = Torrent::read_from_bytes(with_comment).unwrap();
        assert_eq!(
            torrent
                .extra
                .get(b"comment".as_slice())
                .and_then(Value::as_str),
            Some("hello")
        );
        assert_eq!(torrent.encode(), with_comment);
    }

    #[test]
    fn multi_file_length_is_the_sum() {
        let multi = b"d4:infod5:filesld6:lengthi5e4:pathl1:aeed6:lengthi7e4:pathl1:beee4:name1:d12:piece lengthi16384e6:pieces20:00000000000000000000ee";
        let torrent = Torrent::read_from_bytes(multi).unwrap();
        assert!(!torrent.is_single_file());
        assert_eq!(torrent.length(), Some(12));
    }

    #[test]
    fn rejects_what_is_not_a_torrent() {
        assert!(matches!(
            Torrent::read_from_bytes(b"not a torrent"),
            Err(MetainfoError::Bencode(_))
        ));
        assert_eq!(
            Torrent::read_from_bytes(b"li1ee"),
            Err(MetainfoError::NotADictionary)
        );
        assert_eq!(
            Torrent::read_from_bytes(b"d8:announce3:urle"),
            Err(MetainfoError::MissingInfo)
        );
        assert_eq!(
            Torrent::read_from_bytes(b"d8:announcei1e4:infodee"),
            Err(MetainfoError::AnnounceNotAString)
        );
        assert_eq!(
            Torrent::read_from_bytes(b"d13:announce-listl3:urle4:infodee"),
            Err(MetainfoError::AnnounceListMalformed)
        );
    }
}
