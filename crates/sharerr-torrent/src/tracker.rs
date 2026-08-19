//! Where sharerr's torrents announce: sharerr's own tracker.
//!
//! There used to be a second backend here — qBittorrent's embedded tracker — and
//! it was removed on purpose. Two backends meant two independently built announce
//! URLs, and every change to how the endpoint is resolved had to be made and
//! tested twice. The builtin tracker works whichever torrent client seeds, so it
//! is now the only one.
//!
//! The announce URL itself is one path appended to whatever base
//! [`AdvertisedEndpoint`] currently resolves — the same resolver every feed link
//! uses, so the two cannot drift, and the same live value the gluetun poller
//! updates, so a rotated forwarded port flows into the next torrent built.
//!
//! This module is only the *provider* side: what URL to embed in a new torrent and
//! what has to be true before doing so. Serving announces is a separate concern
//! with a separate home — the protocol lives in [`crate::announce`] and the HTTP
//! handlers in the binary crate.

use std::sync::Arc;

use async_trait::async_trait;
use sharerr_core::endpoint::{AdvertisedEndpoint, join_path};
use url::Url;

use crate::error::{Result, TorrentError};

/// The announce URLs one torrent carries.
///
/// `primary` is the current endpoint; `tiers` spans the recently held ones,
/// current first, so a friend's client whose primary goes stale after a VPN
/// reconnect has older tiers to fall back through. `tiers` always contains
/// `primary`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnounceSet {
    pub primary: Url,
    pub tiers: Vec<Url>,
}

impl AnnounceSet {
    /// A set with no fallback tiers — the static-endpoint case.
    pub fn single(primary: Url) -> Self {
        Self {
            tiers: vec![primary.clone()],
            primary,
        }
    }

    /// The BEP 12 announce-list — one URL per tier, in order — or `None` when
    /// only the primary exists. Clients ignore `announce` the moment a list is
    /// present, so a one-entry list would add bytes and change nothing.
    pub fn tier_list(&self) -> Option<Vec<Vec<String>>> {
        if self.tiers.len() <= 1 {
            return None;
        }
        Some(self.tiers.iter().map(|url| vec![url.to_string()]).collect())
    }
}

#[async_trait]
/// Decides the announce URLs a newly built torrent carries.
///
/// Still a trait even with one production implementation: the sync loop's tests
/// substitute a fixed-URL provider.
pub trait TrackerProvider: Send + Sync + std::fmt::Debug {
    /// Make the tracker serve, if it does not already.
    ///
    /// Must be idempotent: it runs at the start of every sync.
    async fn ensure_ready(&self) -> Result<()>;

    /// The announce URLs to embed in new torrents.
    async fn announce_set(&self) -> Result<AnnounceSet>;
}

/// sharerr's own tracker.
///
/// There is nothing to turn on: the announce endpoint is part of `sharerr serve`
/// and is serving whenever the process is. That is also the one caveat — a
/// one-shot `sharerr sync` builds correct torrents whose announces fail until
/// `serve` is running, which is why `doctor` says so.
#[derive(Debug)]
pub struct BuiltinTracker {
    /// The live externally reachable endpoint. Shared, not copied: the gluetun
    /// poller updates the same value this reads, so a rotated port is reflected
    /// in the very next announce URL without rebuilding anything.
    endpoint: Arc<AdvertisedEndpoint>,
    /// Shared secret embedded in the announce path, so that possessing a torrent
    /// file is what grants the right to announce.
    token: Option<String>,
}

impl BuiltinTracker {
    pub fn new(endpoint: Arc<AdvertisedEndpoint>, token: Option<&str>) -> Self {
        Self {
            endpoint,
            token: token.map(str::to_owned),
        }
    }
}

#[async_trait]
impl TrackerProvider for BuiltinTracker {
    async fn ensure_ready(&self) -> Result<()> {
        // Refuse before any torrent is built: with no endpoint at all — nothing
        // configured, nothing observed from gluetun yet — every torrent would
        // announce to nowhere. `serve`'s recovery loop retries, so an endpoint
        // that arrives seconds later is picked up without a restart.
        let Some(base) = self.endpoint.current() else {
            return Err(TorrentError::NoAdvertisedHost);
        };
        tracing::debug!(
            base = %base,
            token = self.token.is_some(),
            "builtin tracker ready"
        );
        Ok(())
    }

    async fn announce_set(&self) -> Result<AnnounceSet> {
        let current = self
            .endpoint
            .current()
            .ok_or(TorrentError::NoAdvertisedHost)?;
        let primary = announce_url(&current, self.token.as_deref())?;

        let mut tiers = Vec::new();
        for base in self.endpoint.recent() {
            tiers.push(announce_url(&base, self.token.as_deref())?);
        }
        if tiers.is_empty() {
            tiers.push(primary.clone());
        }

        Ok(AnnounceSet { primary, tiers })
    }
}

/// The URL paths peers announce and scrape on.
///
/// These cross a crate boundary: this crate writes them into every announce URL
/// it builds, and the binary's router mounts its handlers on the same constants
/// — the same keep-them-adjacent discipline as `torrent_download_path`, but for
/// the path that, if drifted, would send every torrent announcing to a 404 that
/// clients retry forever (refusals are deliberately HTTP 200, so nothing would
/// ever say so).
pub const ANNOUNCE_PATH: &str = "/announce";
pub const SCRAPE_PATH: &str = "/scrape";

/// The announce URL for one base endpoint: `{base}/announce[/{token}]`, keeping
/// any path prefix the base carries.
pub fn announce_url(base: &Url, token: Option<&str>) -> Result<Url> {
    let raw = match token {
        Some(token) => format!("{}/{token}", join_path(base, ANNOUNCE_PATH)),
        None => join_path(base, ANNOUNCE_PATH),
    };

    Url::parse(&raw).map_err(|source| TorrentError::AnnounceUrl {
        base: base.to_string(),
        source,
    })
}

/// The token segment embedded in an announce URL [`announce_url`] built, or
/// `None` when it carries no token — the inverse of that function, for a
/// caller that has a URL and wants to know what it actually grants.
///
/// Reads the literal path rather than assuming the caller's own token still
/// applies: this is what lets `sharerr doctor` and the items page answer
/// "is this *specific* torrent still announcing with the current token"
/// rather than merely "is a token configured".
pub fn token_from_announce_url(url: &Url) -> Option<String> {
    let mut segments = url.path_segments()?;
    loop {
        let segment = segments.next()?;
        if segment == ANNOUNCE_PATH.trim_start_matches('/') {
            return segments.next().map(str::to_owned);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn url(raw: &str) -> Url {
        Url::parse(raw).unwrap()
    }

    fn endpoint(raw: &str) -> Arc<AdvertisedEndpoint> {
        Arc::new(AdvertisedEndpoint::new(Some(url(raw))))
    }

    #[tokio::test]
    async fn the_builtin_tracker_is_ready_without_any_setup() {
        // There is no preferences write and no port to discover — the endpoint
        // ships with the server.
        let tracker = BuiltinTracker::new(endpoint("http://sharerr.example:8477"), None);
        assert!(tracker.ensure_ready().await.is_ok());
    }

    /// Configuration written before the resolver existed must keep producing the
    /// same URLs, so the scheme is frozen.
    #[tokio::test]
    async fn the_builtin_tracker_still_produces_its_announce_url() {
        let tracker = BuiltinTracker::new(endpoint("http://sharerr.example:8477"), None);
        let set = tracker.announce_set().await.unwrap();
        assert_eq!(set.primary.as_str(), "http://sharerr.example:8477/announce");
        assert_eq!(set.tiers, vec![set.primary.clone()]);
        assert_eq!(set.tier_list(), None, "one endpoint means no announce-list");

        let with_token =
            BuiltinTracker::new(endpoint("http://sharerr.example:8477"), Some("s3cret"));
        assert_eq!(
            with_token.announce_set().await.unwrap().primary.as_str(),
            "http://sharerr.example:8477/announce/s3cret"
        );
    }

    /// The reverse-proxy shape the resolver exists for: the prefix must survive
    /// into the announce URL, or every announce lands on the proxy's 404.
    #[tokio::test]
    async fn a_path_prefixed_base_keeps_its_prefix_in_the_announce_url() {
        let tracker = BuiltinTracker::new(endpoint("https://proxy.example/sharerr"), None);
        assert_eq!(
            tracker.announce_set().await.unwrap().primary.as_str(),
            "https://proxy.example/sharerr/announce"
        );
    }

    /// The gluetun story, from the provider's side: an observed endpoint becomes
    /// the primary, and the history becomes fallback tiers.
    #[tokio::test]
    async fn observed_endpoints_become_the_primary_and_the_tiers() {
        let live = endpoint("http://static.example:8477");
        live.observe(url("http://203.0.113.9:41234"));

        let tracker = BuiltinTracker::new(Arc::clone(&live), None);
        let set = tracker.announce_set().await.unwrap();

        assert_eq!(set.primary.as_str(), "http://203.0.113.9:41234/announce");
        assert_eq!(
            set.tiers,
            vec![
                url("http://203.0.113.9:41234/announce"),
                url("http://static.example:8477/announce"),
            ]
        );
        assert_eq!(
            set.tier_list().unwrap().len(),
            2,
            "two endpoints must produce a two-tier announce-list"
        );
    }

    /// No endpoint at all — nothing configured, nothing observed — must refuse
    /// before any torrent gets built.
    #[tokio::test]
    async fn an_empty_endpoint_is_refused() {
        let tracker = BuiltinTracker::new(Arc::new(AdvertisedEndpoint::new(None)), None);
        assert!(matches!(
            tracker.ensure_ready().await.unwrap_err(),
            TorrentError::NoAdvertisedHost
        ));
        assert!(matches!(
            tracker.announce_set().await.unwrap_err(),
            TorrentError::NoAdvertisedHost
        ));
    }

    #[test]
    fn announce_urls_survive_hosts_that_are_bare_addresses() {
        assert_eq!(
            announce_url(&url("http://192.0.2.10:9000"), None)
                .unwrap()
                .as_str(),
            "http://192.0.2.10:9000/announce"
        );
    }

    #[test]
    fn token_from_announce_url_round_trips_through_announce_url() {
        let base = url("http://sharerr.example:8477");
        let with_token = announce_url(&base, Some("s3cret")).unwrap();
        assert_eq!(
            token_from_announce_url(&with_token).as_deref(),
            Some("s3cret")
        );

        let without_token = announce_url(&base, None).unwrap();
        assert_eq!(token_from_announce_url(&without_token), None);
    }

    /// The path-prefixed case a reverse proxy produces — the token is still the
    /// segment right after `announce`, wherever that lands in the path.
    #[test]
    fn token_from_announce_url_survives_a_path_prefix() {
        let base = url("https://proxy.example/sharerr");
        let with_token = announce_url(&base, Some("tok")).unwrap();
        assert_eq!(token_from_announce_url(&with_token).as_deref(), Some("tok"));
    }

    #[test]
    fn token_from_announce_url_is_none_for_an_unrelated_path() {
        assert_eq!(
            token_from_announce_url(&url("http://sharerr.example/other/thing")),
            None
        );
    }
}
