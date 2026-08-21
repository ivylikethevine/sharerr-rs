//! The BitTorrent HTTP tracker protocol, without the HTTP.
//!
//! Everything here is pure: parsing an announce query into a request, keeping the
//! swarm, and rendering a bencoded response. The axum handlers that carry it live
//! in the binary crate, which is what keeps this testable without a listener.
//!
//! # Why the query string is parsed by hand
//!
//! `info_hash` and `peer_id` are **20 raw bytes**, percent-encoded. They are not
//! text and are usually not valid UTF-8. Every convenient parser — `serde_urlencoded`,
//! `form_urlencoded`, axum's `Query` — decodes to `String` and replaces the invalid
//! sequences with U+FFFD, which silently corrupts the one field the whole protocol
//! is keyed on. So the query is split at the byte level and percent-decoded to
//! `Vec<u8>`.
//!
//! # What this tracker deliberately is not
//!
//! It answers only for info hashes the caller vouches for, and it never introduces
//! peers across swarms. There is no scrape-everything, no torrent registration by
//! announce, and no peer list for a hash sharerr is not sharing — an open tracker
//! on a home connection is a liability, not a feature.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use sharerr_core::endpoint::is_private_ip;
use tokio::sync::RwLock;

/// How long a client is told to wait before re-announcing.
const INTERVAL: Duration = Duration::from_secs(1800);

/// The floor a well-behaved client will respect for manual re-announces.
const MIN_INTERVAL: Duration = Duration::from_secs(900);

/// A peer is forgotten after two missed announces.
///
/// Not one: a single dropped request or a client restarting mid-interval would
/// otherwise evict a peer that is still seeding, and the friend's client would
/// stop being told about the one host that has the data.
const PEER_TTL: Duration = Duration::from_secs(INTERVAL.as_secs() * 2 + 60);

/// Most clients ask for 50; this is the ceiling when they ask for more.
const MAX_NUMWANT: usize = 50;

const HASH_LEN: usize = 20;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AnnounceError {
    #[error("missing required parameter {0:?}")]
    Missing(&'static str),

    #[error("{field} must be exactly {HASH_LEN} bytes, got {len}")]
    BadLength { field: &'static str, len: usize },

    #[error("{field} is not a number")]
    NotANumber { field: &'static str },

    /// Returned for a hash sharerr is not sharing. Deliberately does not say
    /// whether it is unknown or merely withdrawn.
    #[error("this tracker does not serve that torrent")]
    UnknownTorrent,

    #[error("invalid announce token")]
    BadToken,
}

/// What a client reports when it announces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnounceRequest {
    pub info_hash: InfoHash,
    pub peer_id: PeerId,
    pub port: u16,
    /// Bytes still to download. Zero means a seeder.
    pub left: u64,
    pub event: Event,
    pub compact: bool,
    pub numwant: usize,
    /// The `ip` parameter, if the client supplied one. Only honoured for private
    /// addresses — see [`AnnounceRequest::resolve_addr`].
    pub declared_ip: Option<IpAddr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Started,
    Stopped,
    Completed,
    /// A periodic re-announce.
    None,
}

impl Event {
    fn parse(raw: &[u8]) -> Self {
        match raw {
            b"started" => Self::Started,
            b"stopped" => Self::Stopped,
            b"completed" => Self::Completed,
            _ => Self::None,
        }
    }
}

impl AnnounceRequest {
    /// Parse a raw query string (everything after `?`), as bytes.
    pub fn parse(query: &[u8]) -> Result<Self, AnnounceError> {
        let params = parse_query(query);

        let get = |key: &'static str| params.get(key).ok_or(AnnounceError::Missing(key));
        let hash = |key: &'static str| -> Result<InfoHash, AnnounceError> {
            let raw = get(key)?;
            InfoHash::try_from(raw.as_slice()).map_err(|_| AnnounceError::BadLength {
                field: key,
                len: raw.len(),
            })
        };
        let number = |key: &'static str, default: u64| -> Result<u64, AnnounceError> {
            match params.get(key) {
                None => Ok(default),
                Some(raw) => std::str::from_utf8(raw)
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .ok_or(AnnounceError::NotANumber { field: key }),
            }
        };

        let port = u16::try_from(number("port", 0)?)
            .map_err(|_| AnnounceError::NotANumber { field: "port" })?;
        if port == 0 {
            return Err(AnnounceError::Missing("port"));
        }

        Ok(Self {
            info_hash: hash("info_hash")?,
            peer_id: hash("peer_id")?,
            port,
            left: number("left", 0)?,
            event: params.get("event").map_or(Event::None, |e| Event::parse(e)),
            // Compact is the default: virtually every client sets `compact=1`;
            // the non-compact form is only a fallback.
            compact: params.get("compact").is_none_or(|v| v != b"0"),
            numwant: usize::try_from(number("numwant", MAX_NUMWANT as u64)?)
                .unwrap_or(MAX_NUMWANT)
                .min(MAX_NUMWANT),
            declared_ip: params
                .get("ip")
                .and_then(|raw| std::str::from_utf8(raw).ok())
                .and_then(|s| s.parse().ok()),
        })
    }

    /// The address to record for this peer.
    ///
    /// The socket's remote address wins over the client's `ip` parameter, because
    /// a client behind NAT reports its own private address and recording that
    /// would hand every other peer an unroutable destination. The exception is a
    /// peer that reaches us from a private address already — a friend on the same
    /// LAN, or traffic arriving through a reverse proxy — where the parameter is
    /// the more informed answer.
    pub fn resolve_addr(&self, remote: IpAddr) -> SocketAddr {
        let ip = match self.declared_ip {
            Some(declared) if is_private_ip(remote) => declared,
            _ => remote,
        };
        SocketAddr::new(ip, self.port)
    }
}

/// The 20-byte SHA-1 that identifies a torrent, and the 20 self-chosen bytes that
/// identify a client within one swarm. Same width, entirely different things —
/// named so the maps below say which is which.
pub type InfoHash = [u8; HASH_LEN];
type PeerId = [u8; HASH_LEN];

/// One swarm: the peers currently announcing for a single torrent.
type Swarm = HashMap<PeerId, Peer>;

/// One peer in one swarm.
#[derive(Debug, Clone, Copy)]
struct Peer {
    addr: SocketAddr,
    /// Zero means a seeder, which is what `complete` counts.
    left: u64,
    last_seen: Instant,
}

impl Peer {
    /// Whether this peer is still inside its TTL as of `now` — the single
    /// liveness check `announce`, `stats`, and `scrape` all apply.
    fn is_live(&self, now: Instant) -> bool {
        now.duration_since(self.last_seen) < PEER_TTL
    }
}

/// Every swarm this tracker is serving, keyed by info hash.
///
/// In memory only. A restart loses the peer lists, and every client re-announces
/// within one interval and repopulates them — the same tradeoff qBittorrent's own
/// embedded tracker makes. Persisting them would mean writing on every announce to
/// preserve state that is stale within half an hour.
#[derive(Debug, Default)]
pub struct Swarms {
    inner: RwLock<HashMap<InfoHash, Swarm>>,
}

/// Live totals across every swarm this tracker serves right now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SwarmStats {
    /// Swarms with at least one live peer.
    pub swarms: usize,
    /// Live peers across all swarms, seeders included.
    pub peers: usize,
    /// The subset of those with the whole thing.
    pub seeders: usize,
}

/// The answer to one announce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnounceResponse {
    pub peers: Vec<SocketAddr>,
    /// Peers with the whole thing.
    pub complete: usize,
    /// Peers still downloading.
    pub incomplete: usize,
}

impl Swarms {
    /// Record the announce and return the peers to tell this client about.
    pub async fn announce(&self, request: &AnnounceRequest, addr: SocketAddr) -> AnnounceResponse {
        let mut swarms = self.inner.write().await;
        let now = Instant::now();
        let swarm = swarms.entry(request.info_hash).or_default();

        // Expiry happens here rather than on a timer: a swarm nobody announces to
        // is a swarm nobody is asking about, and sweeping it costs nothing to defer.
        swarm.retain(|_, peer| peer.is_live(now));

        if request.event == Event::Stopped {
            swarm.remove(&request.peer_id);
        } else {
            swarm.insert(
                request.peer_id,
                Peer {
                    addr,
                    left: request.left,
                    last_seen: now,
                },
            );
        }

        let (mut complete, mut incomplete) = (0, 0);
        let mut peers = Vec::new();
        for (peer_id, peer) in swarm.iter() {
            if peer.left == 0 {
                complete += 1;
            } else {
                incomplete += 1;
            }
            // Never hand a client its own address back; it would connect to itself.
            if peer_id != &request.peer_id && peers.len() < request.numwant {
                peers.push(peer.addr);
            }
        }

        // An empty swarm must not linger: it would grow the map by one entry per
        // torrent ever announced and never shrink.
        if swarm.is_empty() {
            swarms.remove(&request.info_hash);
        }

        AnnounceResponse {
            peers,
            complete,
            incomplete,
        }
    }

    /// Live totals across every swarm, for the status page's one-glance line.
    ///
    /// Counts only peers inside their TTL — the map itself is swept lazily on
    /// announce, so entries past their TTL may still be present and must not be
    /// reported as connected.
    pub async fn stats(&self) -> SwarmStats {
        let swarms = self.inner.read().await;
        let now = Instant::now();

        let mut stats = SwarmStats::default();
        for swarm in swarms.values() {
            let live = swarm.values().filter(|peer| peer.is_live(now));
            let mut any = false;
            for peer in live {
                any = true;
                stats.peers += 1;
                if peer.left == 0 {
                    stats.seeders += 1;
                }
            }
            if any {
                stats.swarms += 1;
            }
        }
        stats
    }

    /// Seeder and leecher counts for one hash, for `/scrape`.
    pub async fn scrape(&self, info_hash: &InfoHash) -> (usize, usize) {
        let swarms = self.inner.read().await;
        let Some(swarm) = swarms.get(info_hash) else {
            return (0, 0);
        };

        let now = Instant::now();
        swarm.values().filter(|peer| peer.is_live(now)).fold(
            (0, 0),
            |(complete, incomplete), peer| {
                if peer.left == 0 {
                    (complete + 1, incomplete)
                } else {
                    (complete, incomplete + 1)
                }
            },
        )
    }

    /// How many swarms are currently tracked.
    #[cfg(test)]
    async fn len(&self) -> usize {
        self.inner.read().await.len()
    }
}

// ---------------------------------------------------------------------------
// Bencode
// ---------------------------------------------------------------------------

/// Bencode a tracker response.
///
/// Written by hand rather than through serde. The format has four constructs, and
/// the one that matters — `peers` as a byte string of packed addresses, not a
/// UTF-8 string — is precisely the thing a serde derive makes awkward.
impl AnnounceResponse {
    /// Render as the bencoded response a BitTorrent client expects.
    ///
    /// `compact` selects the packed 6-bytes-per-peer form, which is what modern
    /// clients ask for.
    pub fn to_bencode(&self, compact: bool) -> Vec<u8> {
        let mut out = Vec::with_capacity(128);
        out.push(b'd');

        // Keys must be in lexicographic order for a well-formed bencoded dict.
        put_int(&mut out, "complete", self.complete as i64);
        put_int(&mut out, "incomplete", self.incomplete as i64);
        put_int(&mut out, "interval", INTERVAL.as_secs() as i64);
        put_key(&mut out, "min interval");
        put_raw_int(&mut out, MIN_INTERVAL.as_secs() as i64);

        let (v4, v6): (Vec<&SocketAddr>, Vec<&SocketAddr>) =
            self.peers.iter().partition(|a| a.is_ipv4());

        put_key(&mut out, "peers");
        if compact {
            put_bytes(&mut out, &pack_compact(&v4));
        } else {
            out.push(b'l');
            for addr in &v4 {
                out.push(b'd');
                put_key(&mut out, "ip");
                put_bytes(&mut out, addr.ip().to_string().as_bytes());
                put_key(&mut out, "port");
                put_raw_int(&mut out, i64::from(addr.port()));
                out.push(b'e');
            }
            out.push(b'e');
        }

        // Only emitted when there is something to put in it. An empty `peers6` is
        // harmless but some older clients are happier without the key at all.
        if compact && !v6.is_empty() {
            put_key(&mut out, "peers6");
            put_bytes(&mut out, &pack_compact(&v6));
        }

        out.push(b'e');
        out
    }
}

/// A tracker error, in the form clients expect: HTTP 200 with a bencoded reason.
///
/// Returning a 4xx here is a common and costly mistake — many clients treat a
/// non-200 as a transport failure and retry forever without ever surfacing the
/// reason to the user.
pub fn failure_bencode(reason: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(reason.len() + 32);
    out.push(b'd');
    put_key(&mut out, "failure reason");
    put_bytes(&mut out, reason.as_bytes());
    out.push(b'e');
    out
}

/// Bencode a `/scrape` response for a set of hashes.
pub fn scrape_bencode(files: &[(InfoHash, usize, usize)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(64 + files.len() * 96);
    out.push(b'd');
    put_key(&mut out, "files");
    out.push(b'd');
    for (hash, complete, incomplete) in files {
        put_bytes(&mut out, hash);
        out.push(b'd');
        put_int(&mut out, "complete", *complete as i64);
        put_int(&mut out, "downloaded", *complete as i64);
        put_int(&mut out, "incomplete", *incomplete as i64);
        out.push(b'e');
    }
    out.push(b'e');
    out.push(b'e');
    out
}

fn put_key(out: &mut Vec<u8>, key: &str) {
    put_bytes(out, key.as_bytes());
}

fn put_bytes(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(value.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(value);
}

fn put_raw_int(out: &mut Vec<u8>, value: i64) {
    out.push(b'i');
    out.extend_from_slice(value.to_string().as_bytes());
    out.push(b'e');
}

fn put_int(out: &mut Vec<u8>, key: &str, value: i64) {
    put_key(out, key);
    put_raw_int(out, value);
}

// ---------------------------------------------------------------------------
// Query parsing
// ---------------------------------------------------------------------------

/// Split a query string into percent-decoded byte values.
///
/// Last value wins for a repeated key, matching every other tracker. Keys are
/// compared as UTF-8 because they always are; values never are.
pub fn parse_query(query: &[u8]) -> HashMap<String, Vec<u8>> {
    let mut params = HashMap::new();

    for pair in query.split(|b| *b == b'&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = match pair.iter().position(|b| *b == b'=') {
            Some(at) => (&pair[..at], &pair[at + 1..]),
            None => (pair, &[][..]),
        };

        let Ok(key) = std::str::from_utf8(key) else {
            continue;
        };
        params.insert(key.to_owned(), percent_decode(value));
    }

    params
}

/// Percent-decode to bytes, never to text.
///
/// `+` becomes a space: some clients still form-encode the query, and a peer_id
/// containing a literal `+` byte would otherwise not round-trip.
fn percent_decode(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;

    while i < raw.len() {
        match raw[i] {
            b'%' if i + 2 < raw.len() => {
                match (hex_nibble(raw[i + 1]), hex_nibble(raw[i + 2])) {
                    (Some(hi), Some(lo)) => {
                        out.push((hi << 4) | lo);
                        i += 3;
                    }
                    // A stray `%` is kept verbatim rather than dropped; dropping it
                    // would shorten a hash and produce a confusing length error.
                    _ => {
                        out.push(raw[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }

    out
}

/// Pack peers in BEP 23/7 compact form: address octets then a big-endian port,
/// 6 bytes per IPv4 peer and 18 per IPv6. The caller partitions by family —
/// `peers` and `peers6` are separate keys — and this packs whichever list it is
/// handed.
fn pack_compact(addrs: &[&SocketAddr]) -> Vec<u8> {
    let mut packed = Vec::with_capacity(addrs.len() * 18);
    for addr in addrs {
        match addr.ip() {
            IpAddr::V4(ip) => packed.extend_from_slice(&ip.octets()),
            IpAddr::V6(ip) => packed.extend_from_slice(&ip.octets()),
        }
        packed.extend_from_slice(&addr.port().to_be_bytes());
    }
    packed
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Parse a hex info hash — the form sharerr stores and the Torznab feed publishes.
pub fn info_hash_from_hex(raw: &str) -> Option<InfoHash> {
    let mut out = [0u8; HASH_LEN];
    hex::decode_to_slice(raw, &mut out).ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// Percent-encode raw bytes the way a client would — the inverse of
    /// [`percent_decode`]. Lives here because only these tests need it: they build
    /// realistic queries rather than pasting pre-encoded strings, which is what
    /// makes the round trip through the decoder worth asserting.
    fn percent_encode(raw: &[u8]) -> String {
        let mut out = String::with_capacity(raw.len() * 3);
        for byte in raw {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(*byte as char);
                }
                _ => out.push_str(&format!("%{byte:02x}")),
            }
        }
        out
    }

    fn hash(seed: u8) -> InfoHash {
        [seed; HASH_LEN]
    }

    /// A realistic query: the two 20-byte fields percent-encoded, everything else
    /// plain. Built rather than pasted so the encoding is exercised too.
    fn query(info_hash: [u8; 20], peer_id: [u8; 20], extra: &str) -> Vec<u8> {
        format!(
            "info_hash={}&peer_id={}&port=6881&uploaded=0&downloaded=0&left=0&compact=1{extra}",
            percent_encode(&info_hash),
            percent_encode(&peer_id),
        )
        .into_bytes()
    }

    #[test]
    fn a_binary_info_hash_survives_parsing() {
        // The bytes that break every UTF-8-based parser: 0xff is not valid UTF-8,
        // and a lossy decode would turn this into U+FFFD and lose the identity.
        let info_hash = [0xffu8; 20];
        let peer_id = *b"-qB5000-abcdefghijkl";

        let request = AnnounceRequest::parse(&query(info_hash, peer_id, "")).unwrap();

        assert_eq!(request.info_hash, info_hash);
        assert_eq!(request.peer_id, peer_id);
        assert_eq!(request.port, 6881);
        assert!(request.compact);
        assert_eq!(request.event, Event::None);
    }

    #[test]
    fn every_byte_value_round_trips_through_percent_coding() {
        let all: Vec<u8> = (0u8..=255).collect();
        assert_eq!(percent_decode(percent_encode(&all).as_bytes()), all);
    }

    #[test]
    fn events_and_numwant_are_read() {
        let r =
            AnnounceRequest::parse(&query(hash(1), hash(2), "&event=stopped&numwant=10")).unwrap();
        assert_eq!(r.event, Event::Stopped);
        assert_eq!(r.numwant, 10);

        let r = AnnounceRequest::parse(&query(hash(1), hash(2), "&event=completed")).unwrap();
        assert_eq!(r.event, Event::Completed);

        // An absurd numwant is clamped rather than honoured or rejected.
        let r = AnnounceRequest::parse(&query(hash(1), hash(2), "&numwant=100000")).unwrap();
        assert_eq!(r.numwant, MAX_NUMWANT);
    }

    #[test]
    fn missing_and_malformed_fields_are_named() {
        assert_eq!(
            AnnounceRequest::parse(b"peer_id=x&port=1"),
            Err(AnnounceError::Missing("info_hash"))
        );

        // A 19-byte hash is the classic symptom of a mangled percent-decode; the
        // error has to say so rather than reporting it as absent.
        let short = format!(
            "info_hash={}&peer_id={}&port=1",
            percent_encode(&[1u8; 19]),
            percent_encode(&hash(2))
        );
        assert_eq!(
            AnnounceRequest::parse(short.as_bytes()),
            Err(AnnounceError::BadLength {
                field: "info_hash",
                len: 19
            })
        );

        let no_port = format!(
            "info_hash={}&peer_id={}",
            percent_encode(&hash(1)),
            percent_encode(&hash(2))
        );
        assert_eq!(
            AnnounceRequest::parse(no_port.as_bytes()),
            Err(AnnounceError::Missing("port"))
        );
    }

    #[tokio::test]
    async fn two_peers_in_a_swarm_are_introduced_to_each_other() {
        let swarms = Swarms::default();
        let seeder = AnnounceRequest::parse(&query(hash(9), hash(1), "")).unwrap();
        let leecher = {
            let mut r = AnnounceRequest::parse(&query(hash(9), hash(2), "")).unwrap();
            r.left = 1024;
            r
        };

        let a: SocketAddr = "203.0.113.5:6881".parse().unwrap();
        let b: SocketAddr = "203.0.113.6:6882".parse().unwrap();

        let first = swarms.announce(&seeder, a).await;
        assert!(first.peers.is_empty(), "nobody else is here yet");
        assert_eq!((first.complete, first.incomplete), (1, 0));

        let second = swarms.announce(&leecher, b).await;
        assert_eq!(
            second.peers,
            vec![a],
            "the leecher must learn about the seeder"
        );
        assert_eq!((second.complete, second.incomplete), (1, 1));
    }

    #[tokio::test]
    async fn a_peer_is_never_told_about_itself() {
        let swarms = Swarms::default();
        let request = AnnounceRequest::parse(&query(hash(9), hash(1), "")).unwrap();
        let addr: SocketAddr = "203.0.113.5:6881".parse().unwrap();

        swarms.announce(&request, addr).await;
        let again = swarms.announce(&request, addr).await;

        assert!(again.peers.is_empty(), "got its own address back");
        assert_eq!(again.complete, 1, "and it is still counted once");
    }

    #[tokio::test]
    async fn swarms_do_not_leak_into_each_other() {
        let swarms = Swarms::default();
        let one = AnnounceRequest::parse(&query(hash(1), hash(1), "")).unwrap();
        let two = AnnounceRequest::parse(&query(hash(2), hash(2), "")).unwrap();

        swarms
            .announce(&one, "203.0.113.1:6881".parse().unwrap())
            .await;
        let other = swarms
            .announce(&two, "203.0.113.2:6881".parse().unwrap())
            .await;

        assert!(
            other.peers.is_empty(),
            "a different torrent is a different swarm"
        );
        assert_eq!(swarms.len().await, 2);
    }

    #[tokio::test]
    async fn stopping_removes_a_peer_and_empties_the_swarm() {
        let swarms = Swarms::default();
        let addr: SocketAddr = "203.0.113.5:6881".parse().unwrap();
        let start = AnnounceRequest::parse(&query(hash(9), hash(1), "")).unwrap();
        swarms.announce(&start, addr).await;
        assert_eq!(swarms.len().await, 1);

        let stop = AnnounceRequest::parse(&query(hash(9), hash(1), "&event=stopped")).unwrap();
        let response = swarms.announce(&stop, addr).await;

        assert_eq!((response.complete, response.incomplete), (0, 0));
        assert_eq!(
            swarms.len().await,
            0,
            "an emptied swarm must be dropped, or the map grows forever"
        );
    }

    #[tokio::test]
    async fn scrape_counts_seeders_and_leechers() {
        let swarms = Swarms::default();
        let seeder = AnnounceRequest::parse(&query(hash(9), hash(1), "")).unwrap();
        let mut leecher = AnnounceRequest::parse(&query(hash(9), hash(2), "")).unwrap();
        leecher.left = 500;

        swarms
            .announce(&seeder, "203.0.113.1:6881".parse().unwrap())
            .await;
        swarms
            .announce(&leecher, "203.0.113.2:6881".parse().unwrap())
            .await;

        assert_eq!(swarms.scrape(&hash(9)).await, (1, 1));
        assert_eq!(
            swarms.scrape(&hash(8)).await,
            (0, 0),
            "unknown hash is empty, not an error"
        );
    }

    #[test]
    fn compact_peers_are_six_bytes_each_big_endian() {
        let response = AnnounceResponse {
            peers: vec!["203.0.113.5:6881".parse().unwrap()],
            complete: 1,
            incomplete: 0,
        };

        let encoded = response.to_bencode(true);
        // 6881 = 0x1AE1, and the port is big-endian on the wire.
        let expected: &[u8] = &[203, 0, 113, 5, 0x1a, 0xe1];
        assert!(
            encoded.windows(6).any(|w| w == expected),
            "packed peer not found in {encoded:?}"
        );
        assert!(
            encoded.starts_with(b"d8:completei1e"),
            "keys must be sorted"
        );
        assert!(encoded.ends_with(b"e"));
    }

    #[test]
    fn the_non_compact_form_is_a_list_of_dicts() {
        let response = AnnounceResponse {
            peers: vec!["203.0.113.5:6881".parse().unwrap()],
            complete: 1,
            incomplete: 0,
        };

        let encoded = String::from_utf8(response.to_bencode(false)).unwrap();
        assert!(
            encoded.contains("5:peersld2:ip11:203.0.113.54:porti6881ee"),
            "{encoded}"
        );
    }

    #[test]
    fn ipv6_peers_go_in_their_own_key() {
        let response = AnnounceResponse {
            peers: vec![
                "203.0.113.5:6881".parse().unwrap(),
                "[2001:db8::1]:6881".parse().unwrap(),
            ],
            complete: 2,
            incomplete: 0,
        };

        let encoded = response.to_bencode(true);
        // 6 bytes for the v4 peer, 18 for the v6 one — never mixed in one key.
        assert!(
            encoded.windows(9).any(|w| w == b"5:peers6:"),
            "the v4 key must hold exactly one 6-byte peer"
        );
        assert!(
            encoded.windows(11).any(|w| w == b"6:peers618:"),
            "the v6 key must hold exactly one 18-byte peer"
        );
    }

    #[test]
    fn a_failure_is_a_bencoded_reason_not_an_http_error() {
        let encoded = failure_bencode("this tracker does not serve that torrent");
        assert_eq!(
            String::from_utf8(encoded).unwrap(),
            "d14:failure reason40:this tracker does not serve that torrente"
        );
    }

    #[test]
    fn scrape_encodes_each_file_under_its_raw_hash() {
        let encoded = scrape_bencode(&[(hash(0x41), 2, 1)]);
        let text = String::from_utf8_lossy(&encoded).into_owned();
        assert!(
            text.starts_with("d5:filesd20:AAAAAAAAAAAAAAAAAAAAd"),
            "{text}"
        );
        assert!(text.contains("8:completei2e"), "{text}");
        assert!(text.contains("10:incompletei1e"), "{text}");
    }

    #[test]
    fn a_nat_peer_is_recorded_at_the_address_it_reached_us_from() {
        let mut request = AnnounceRequest::parse(&query(hash(1), hash(2), "")).unwrap();
        request.declared_ip = Some("192.168.1.50".parse().unwrap());

        // Public remote: the declared private address is useless to other peers.
        let public: IpAddr = "203.0.113.9".parse().unwrap();
        assert_eq!(
            request.resolve_addr(public),
            "203.0.113.9:6881".parse::<SocketAddr>().unwrap()
        );

        // Private remote: we are on the same LAN or behind a proxy, so believe it.
        let lan: IpAddr = "10.0.0.2".parse().unwrap();
        assert_eq!(
            request.resolve_addr(lan),
            "192.168.1.50:6881".parse::<SocketAddr>().unwrap()
        );
    }

    #[test]
    fn hex_info_hashes_convert_both_ways() {
        let raw = hash(0xab);
        let hex = "ab".repeat(20);
        assert_eq!(info_hash_from_hex(&hex), Some(raw));
        assert_eq!(info_hash_from_hex("abc"), None, "wrong length");
        assert_eq!(info_hash_from_hex(&"zz".repeat(20)), None, "not hex");
    }
}
