//! rTorrent XML-RPC client.
//!
//! sharerr uses rTorrent for exactly one thing: seeding files that already
//! exist, from where they already are. Everything in this crate is shaped by
//! the requirement that adding a share must never move, re-link, or delete
//! media — see [`RtorrentClient::add`] for the mechanism that enforces it.
//!
//! # Why the configured URL is the RPC endpoint itself
//!
//! qBittorrent and Transmission each have exactly one HTTP API, so those two
//! sibling crates take a *base* URL and append a fixed, well-known path.
//! rTorrent has no HTTP server of its own — it speaks XML-RPC over SCGI, and
//! everything that reaches it over plain HTTP does so through a reverse
//! proxy an operator put in front of it. There is no single standard path for
//! that proxy (`/RPC2` and ruTorrent's `/plugins/httprpc/action.php` both see
//! real use), so unlike its siblings this client takes the *exact* RPC URL —
//! whatever the operator's proxy answers XML-RPC POSTs on — rather than
//! guessing a suffix.
//!
//! # Authentication
//!
//! rTorrent's own XML-RPC has no concept of a credential. The username and
//! password this client takes are sent as HTTP Basic Auth on every request,
//! for the common case where the reverse proxy in front of the RPC endpoint
//! is what enforces access — the standard way ruTorrent's `httprpc` plugin is
//! secured. A proxy with no such gate simply ignores the header, so an
//! operator in that position can put any placeholder values in Settings.
//!
//! # Category and tags
//!
//! rTorrent has no notion of a category. Like the Transmission client, both
//! sharerr's category and its tags collapse into one value — `d.custom1`,
//! rTorrent's free-text per-download slot ruTorrent itself uses for exactly
//! this purpose.
//!
//! # What rTorrent cannot do
//!
//! Two of this trait's optional behaviours have no rTorrent equivalent, and
//! rather than fake either, [`RtorrentClient::add`] and
//! [`RtorrentClient::set_trackers`] warn and do the closest honest thing:
//!
//! - **No skip-checking.** rTorrent always verifies a torrent's data against
//!   its piece hashes when a download starts; there is no documented
//!   command that bypasses this, the same limitation
//!   `sharerr_transmission` already has.
//! - **No seed-ratio limit.** rTorrent's ratio enforcement is a `.rtorrent.rc`
//!   schedule keyed to a *view*, not a per-torrent XML-RPC setting — there is
//!   nothing this trait's `ratio_limit` can attach to. `upload_limit_kib`
//!   *is* honoured, via a per-torrent named throttle
//!   (`throttle.up = name,rate_kib` to define it, `d.throttle_name.set` to
//!   attach it). Note the shape: `throttle.up` takes the rate in **KiB/s**
//!   and `throttle.up.max` is a *getter* with no `.set` variant — an earlier
//!   version of this crate called `throttle.up.max.set` with a bytes/s
//!   value, which faulted after the torrent had already loaded, so every
//!   item was recorded failed while it was actually live.
//! - **No tracker removal.** rTorrent's XML-RPC API has never grown a way to
//!   remove a tracker from an already-loaded torrent (tracked upstream as
//!   [rakshasa/rtorrent#165](https://github.com/rakshasa/rtorrent/issues/165),
//!   open since 2013) — only `d.tracker.insert` to add one. So
//!   [`RtorrentClient::set_trackers`] cannot *replace* a torrent's trackers
//!   the way the qBittorrent and Transmission clients do: it can only insert
//!   the new ones as an additional tier ahead of whatever is already there.
//!   That still serves the purpose an endpoint rotation needs — the torrent
//!   keeps announcing somewhere alive — it just also keeps announcing to the
//!   stale address alongside it, forever, which is harmless beyond a wasted
//!   announce attempt per interval.
//!
//! # A hand-mocked server proves the parser, not the protocol
//!
//! Every call this crate makes is verified against a hand-mocked XML-RPC
//! server in the tests below, which proves this crate parses the requests
//! and responses it expects — not that those are the requests and responses
//! a real rTorrent sends. `run_docker_tests.sh --rtorrent` covers that half:
//! the same tier-2 suite qBittorrent and Transmission run, against a real
//! `crazymax/rtorrent-rutorrent` container. It is what actually caught two
//! bugs the mock could not have: `d.multicall2` needs a leading empty
//! parameter or a real rTorrent rejects it outright, and an empty result
//! comes back as a self-closing `<data/>` rather than `<data></data>`.
//!
//! # Module layout
//!
//! - [`client`] — [`RtorrentClient`] itself: construction and the raw
//!   `call`/`call_str`/`call_multi` machinery every operation is built on.
//! - [`adapter`] — the [`sharerr_client::TorrentClient`] trait implementation:
//!   what each operation actually asks rTorrent for, and how its untyped
//!   XML-RPC scalars become sharerr's typed fields.
//! - [`xmlrpc`] — the wire format: building a request body, decoding a
//!   response into an [`xmlrpc::XmlValue`]. Knows nothing about
//!   `RtorrentClient` or what a call means.

mod adapter;
mod client;
mod xmlrpc;

pub use client::RtorrentClient;

/// Which client this crate's errors and messages are about.
pub(crate) const KIND: sharerr_client::ClientKind = sharerr_client::ClientKind::Rtorrent;
