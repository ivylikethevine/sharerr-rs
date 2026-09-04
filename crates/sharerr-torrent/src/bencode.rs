//! A minimal bencode encoder and decoder (BEP 3).
//!
//! sharerr needs exactly two things from bencode: to write a `.torrent` whose
//! info dictionary hashes the same way every other implementation would hash
//! it, and to read one back far enough to rewrite its announce fields without
//! disturbing that dictionary. Both rest on one property: dictionary keys are
//! written in sorted byte order, which is what makes the encoding canonical
//! and the info hash stable across implementations.
//!
//! Decoding is lenient about key order (a hand-edited or foreign `.torrent` is
//! still readable) and strict about everything else that could be ambiguous:
//! integers must be well-formed, strings must be complete, the input must end
//! where the value ends, and nesting is bounded so malicious input cannot
//! overflow the stack.

use std::collections::BTreeMap;

/// A decoded bencode value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Int(i64),
    Bytes(Vec<u8>),
    List(Vec<Value>),
    /// Keys are raw byte strings, ordered the way BEP 3 requires them written.
    Dict(BTreeMap<Vec<u8>, Value>),
}

/// Why a byte sequence is not valid bencode.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BencodeError {
    #[error("input ended in the middle of a value")]
    Truncated,
    #[error("unexpected byte {byte:#04x} at offset {offset}")]
    Unexpected { byte: u8, offset: usize },
    #[error("malformed integer at offset {offset}")]
    MalformedInteger { offset: usize },
    #[error("malformed string length at offset {offset}")]
    MalformedLength { offset: usize },
    #[error("dictionary key at offset {offset} is not a string")]
    KeyNotAString { offset: usize },
    #[error("values nest deeper than {limit} levels")]
    TooDeep { limit: usize },
    #[error("{trailing} trailing byte(s) after the value")]
    TrailingBytes { trailing: usize },
}

/// Deeper than any real `.torrent` ever nests; shallow enough that a crafted
/// input cannot recurse the parser off the stack.
const MAX_DEPTH: usize = 32;

impl Value {
    /// Canonical encoding: dictionaries sorted by raw key bytes.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut Vec<u8>) {
        match self {
            Value::Int(n) => {
                out.push(b'i');
                out.extend_from_slice(n.to_string().as_bytes());
                out.push(b'e');
            }
            Value::Bytes(bytes) => write_bytes(bytes, out),
            Value::List(items) => {
                out.push(b'l');
                for item in items {
                    item.write(out);
                }
                out.push(b'e');
            }
            Value::Dict(entries) => {
                out.push(b'd');
                // BTreeMap<Vec<u8>, _> iterates in raw-byte order, which is
                // exactly BEP 3's "sorted as raw strings".
                for (key, value) in entries {
                    write_bytes(key, out);
                    value.write(out);
                }
                out.push(b'e');
            }
        }
    }

    /// Decode one complete value; anything after it is an error.
    pub fn decode(input: &[u8]) -> Result<Value, BencodeError> {
        let mut parser = Parser { input, pos: 0 };
        let value = parser.value(0)?;
        if parser.pos != input.len() {
            return Err(BencodeError::TrailingBytes {
                trailing: input.len() - parser.pos,
            });
        }
        Ok(value)
    }

    #[must_use]
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(n) => Some(*n),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        self.as_bytes().and_then(|b| std::str::from_utf8(b).ok())
    }

    #[must_use]
    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Value::List(items) => Some(items),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_dict(&self) -> Option<&BTreeMap<Vec<u8>, Value>> {
        match self {
            Value::Dict(entries) => Some(entries),
            _ => None,
        }
    }

    /// A `Value::Bytes` from a `&str`, the common case for keys and URLs.
    #[must_use]
    pub fn string(s: &str) -> Value {
        Value::Bytes(s.as_bytes().to_vec())
    }
}

fn write_bytes(bytes: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(bytes.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(bytes);
}

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Result<u8, BencodeError> {
        self.input
            .get(self.pos)
            .copied()
            .ok_or(BencodeError::Truncated)
    }

    fn value(&mut self, depth: usize) -> Result<Value, BencodeError> {
        if depth > MAX_DEPTH {
            return Err(BencodeError::TooDeep { limit: MAX_DEPTH });
        }
        match self.peek()? {
            b'i' => self.integer(),
            b'0'..=b'9' => self.bytes().map(Value::Bytes),
            b'l' => {
                self.pos += 1;
                let mut items = Vec::new();
                while self.peek()? != b'e' {
                    items.push(self.value(depth + 1)?);
                }
                self.pos += 1;
                Ok(Value::List(items))
            }
            b'd' => {
                self.pos += 1;
                let mut entries = BTreeMap::new();
                while self.peek()? != b'e' {
                    let key_offset = self.pos;
                    if !self.peek()?.is_ascii_digit() {
                        return Err(BencodeError::KeyNotAString { offset: key_offset });
                    }
                    let key = self.bytes()?;
                    let value = self.value(depth + 1)?;
                    entries.insert(key, value);
                }
                self.pos += 1;
                Ok(Value::Dict(entries))
            }
            byte => Err(BencodeError::Unexpected {
                byte,
                offset: self.pos,
            }),
        }
    }

    fn integer(&mut self) -> Result<Value, BencodeError> {
        let offset = self.pos;
        self.pos += 1; // 'i'
        let end = self.input[self.pos..]
            .iter()
            .position(|&b| b == b'e')
            .ok_or(BencodeError::Truncated)?;
        let digits = &self.input[self.pos..self.pos + end];
        let text =
            std::str::from_utf8(digits).map_err(|_| BencodeError::MalformedInteger { offset })?;
        // BEP 3 forbids leading zeros and "-0"; both would make two byte
        // sequences decode to one value, which is exactly the ambiguity a
        // canonical encoding exists to rule out.
        let canonical = text
            .parse::<i64>()
            .ok()
            .filter(|n| n.to_string() == text)
            .ok_or(BencodeError::MalformedInteger { offset })?;
        self.pos += end + 1;
        Ok(Value::Int(canonical))
    }

    fn bytes(&mut self) -> Result<Vec<u8>, BencodeError> {
        let offset = self.pos;
        let colon = self.input[self.pos..]
            .iter()
            .position(|&b| b == b':')
            .ok_or(BencodeError::Truncated)?;
        let digits = &self.input[self.pos..self.pos + colon];
        let len = std::str::from_utf8(digits)
            .ok()
            .and_then(|text| text.parse::<usize>().ok())
            .ok_or(BencodeError::MalformedLength { offset })?;
        let start = self.pos + colon + 1;
        let end = start.checked_add(len).ok_or(BencodeError::Truncated)?;
        let bytes = self
            .input
            .get(start..end)
            .ok_or(BencodeError::Truncated)?
            .to_vec();
        self.pos = end;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn dict(entries: &[(&str, Value)]) -> Value {
        Value::Dict(
            entries
                .iter()
                .map(|(k, v)| (k.as_bytes().to_vec(), v.clone()))
                .collect(),
        )
    }

    #[test]
    fn encodes_the_bep3_examples() {
        assert_eq!(Value::Int(3).encode(), b"i3e");
        assert_eq!(Value::Int(-3).encode(), b"i-3e");
        assert_eq!(Value::string("spam").encode(), b"4:spam");
        assert_eq!(
            Value::List(vec![Value::string("spam"), Value::string("eggs")]).encode(),
            b"l4:spam4:eggse"
        );
        assert_eq!(
            dict(&[
                ("cow", Value::string("moo")),
                ("spam", Value::string("eggs"))
            ])
            .encode(),
            b"d3:cow3:moo4:spam4:eggse"
        );
    }

    #[test]
    fn dictionary_keys_are_written_in_raw_byte_order_whatever_order_they_were_added() {
        // "piece length" sorts before "pieces" byte-wise (' ' < 's'), and
        // "Z" before "a"; a naive alphabetical sort would get both wrong.
        let value = dict(&[
            ("pieces", Value::Int(1)),
            ("piece length", Value::Int(2)),
            ("a", Value::Int(3)),
            ("Z", Value::Int(4)),
        ]);
        assert_eq!(
            value.encode(),
            b"d1:Zi4e1:ai3e12:piece lengthi2e6:piecesi1ee"
        );
    }

    #[test]
    fn round_trips_nested_values() {
        let value = dict(&[
            ("announce", Value::string("http://t.example/announce")),
            (
                "announce-list",
                Value::List(vec![Value::List(vec![
                    Value::string("http://a"),
                    Value::string("http://b"),
                ])]),
            ),
            (
                "info",
                dict(&[
                    ("length", Value::Int(1234)),
                    ("name", Value::string("x.mkv")),
                    ("pieces", Value::Bytes(vec![0u8, 255, 7])),
                    ("private", Value::Int(1)),
                ]),
            ),
        ]);
        assert_eq!(Value::decode(&value.encode()).unwrap(), value);
    }

    #[test]
    fn rejects_the_inputs_that_would_make_decoding_ambiguous() {
        assert_eq!(
            Value::decode(b"i03e"),
            Err(BencodeError::MalformedInteger { offset: 0 })
        );
        assert_eq!(
            Value::decode(b"i-0e"),
            Err(BencodeError::MalformedInteger { offset: 0 })
        );
        assert_eq!(Value::decode(b"i12"), Err(BencodeError::Truncated));
        assert_eq!(Value::decode(b"5:abc"), Err(BencodeError::Truncated));
        assert_eq!(
            Value::decode(b"i1ei2e"),
            Err(BencodeError::TrailingBytes { trailing: 3 })
        );
        assert_eq!(
            Value::decode(b"di1ei2ee"),
            Err(BencodeError::KeyNotAString { offset: 1 })
        );
        assert_eq!(
            Value::decode(b"x"),
            Err(BencodeError::Unexpected {
                byte: b'x',
                offset: 0
            })
        );
        assert_eq!(Value::decode(b""), Err(BencodeError::Truncated));
    }

    #[test]
    fn deeply_nested_input_is_refused_rather_than_overflowing_the_stack() {
        let input = vec![b'l'; 10_000];
        assert_eq!(
            Value::decode(&input),
            Err(BencodeError::TooDeep { limit: MAX_DEPTH })
        );
    }

    #[test]
    fn a_string_length_that_overflows_is_truncated_not_a_panic() {
        assert_eq!(
            Value::decode(b"99999999999999999999:x"),
            Err(BencodeError::MalformedLength { offset: 0 })
        );
        assert_eq!(
            Value::decode(b"18446744073709551615:x"),
            Err(BencodeError::Truncated)
        );
    }
}
