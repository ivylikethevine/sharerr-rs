//! Where sharerr's torrents announce.
//!
//! Two backends, both configurable, only one of which works in milestone 1:
//!
//! * [`QbitEmbeddedTracker`] — qBittorrent has a tracker built in. Turning it on is
//!   a preferences write, and it is then serving immediately. Fully implemented.
//! * [`BuiltinTracker`] — sharerr's own tracker. The URL scheme is settled so that
//!   configuration written today stays valid, but the server arrives in milestone 2
//!   and [`TrackerProvider::ensure_ready`] says so plainly rather than pretending.

use std::sync::Arc;

use async_trait::async_trait;
use sharerr_qbit::QbitClient;
use tokio::sync::Mutex;
use url::Url;

use crate::error::{Result, TorrentError};

#[async_trait]
pub trait TrackerProvider: Send + Sync + std::fmt::Debug {
    /// Make the tracker serve, if it does not already.
    ///
    /// Must be idempotent: it runs at the start of every sync.
    async fn ensure_ready(&self) -> Result<()>;

    /// The announce URL to embed in new torrents.
    async fn announce_url(&self) -> Result<Url>;
}

/// qBittorrent's embedded tracker.
#[derive(Debug)]
pub struct QbitEmbeddedTracker {
    qbit: Arc<QbitClient>,
    /// The address **friends** reach the tracker on. Cannot be inferred: the
    /// container's own view of its address is almost never the one that works from
    /// outside, and a wrong guess yields torrents nobody can announce to.
    advertised_host: String,
    /// Set to override the port qBittorrent reports, for the common case where a
    /// published docker port differs from the internal one.
    port_override: Option<u16>,
    /// Resolved once per process. `ensure_embedded_tracker` is cheap but not free,
    /// and a sync may build hundreds of torrents.
    port: Mutex<Option<u16>>,
}

impl QbitEmbeddedTracker {
    pub fn new(
        qbit: Arc<QbitClient>,
        advertised_host: Option<&str>,
        port_override: Option<u16>,
    ) -> Result<Self> {
        let advertised_host = advertised_host
            .map(str::to_owned)
            .ok_or(TorrentError::NoAdvertisedHost)?;

        Ok(Self {
            qbit,
            advertised_host,
            port_override,
            port: Mutex::new(None),
        })
    }

    /// Turn the tracker on if it is off, and report the port qBittorrent serves on.
    ///
    /// Note this runs **regardless of `port_override`**. The override says what to
    /// advertise; it says nothing about whether the tracker is running, and
    /// treating it as a reason to skip this step means the tracker never gets
    /// enabled at all — every torrent then announces to a closed port.
    async fn served_port(&self) -> Result<u16> {
        let mut cached = self.port.lock().await;
        if let Some(port) = *cached {
            return Ok(port);
        }

        let served = self.qbit.ensure_embedded_tracker().await?;
        // Only fatal without an override to fall back on: an operator who named a
        // published port knows more about the deployment than qBittorrent does.
        if served == 0 && self.port_override.is_none() {
            return Err(TorrentError::NoTrackerPort);
        }

        *cached = Some(served);
        Ok(served)
    }

    /// The port to put in announce URLs: what the operator published, or failing
    /// that, what qBittorrent reports.
    fn advertised_port(&self, served: u16) -> u16 {
        self.port_override.unwrap_or(served)
    }
}

#[async_trait]
impl TrackerProvider for QbitEmbeddedTracker {
    async fn ensure_ready(&self) -> Result<()> {
        // Cleared first so every sync re-verifies. qBittorrent can be restarted or
        // reconfigured underneath a long-running `serve`, and one preferences call
        // per pass is nothing next to silently seeding to a dead tracker.
        *self.port.lock().await = None;

        let served = self.served_port().await?;
        tracing::debug!(
            host = %self.advertised_host,
            served,
            advertised = self.advertised_port(served),
            "embedded tracker ready"
        );
        Ok(())
    }

    async fn announce_url(&self) -> Result<Url> {
        let served = self.served_port().await?;
        announce_url(&self.advertised_host, self.advertised_port(served), None)
    }
}

/// sharerr's own tracker. Milestone 2.
#[derive(Debug)]
pub struct BuiltinTracker {
    advertised_host: String,
    port: u16,
    /// Shared secret embedded in the announce path, so that possessing a torrent
    /// file is what grants the right to announce.
    token: Option<String>,
}

impl BuiltinTracker {
    pub fn new(advertised_host: Option<&str>, port: u16, token: Option<&str>) -> Result<Self> {
        let advertised_host = advertised_host
            .map(str::to_owned)
            .ok_or(TorrentError::NoAdvertisedHost)?;

        Ok(Self {
            advertised_host,
            port,
            token: token.map(str::to_owned),
        })
    }
}

#[async_trait]
impl TrackerProvider for BuiltinTracker {
    async fn ensure_ready(&self) -> Result<()> {
        // Refusing here is the point. Producing torrents that announce into the
        // void would look like success and fail silently at the friend's end.
        Err(TorrentError::BuiltinTrackerUnavailable)
    }

    async fn announce_url(&self) -> Result<Url> {
        announce_url(&self.advertised_host, self.port, self.token.as_deref())
    }
}

fn announce_url(host: &str, port: u16, token: Option<&str>) -> Result<Url> {
    let raw = match token {
        Some(token) => format!("http://{host}:{port}/announce/{token}"),
        None => format!("http://{host}:{port}/announce"),
    };

    Url::parse(&raw).map_err(|source| TorrentError::AnnounceUrl {
        host: host.to_owned(),
        port,
        source,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[tokio::test]
    async fn the_builtin_tracker_refuses_to_pretend_it_is_ready() {
        let tracker = BuiltinTracker::new(Some("sharerr.example"), 8477, None).unwrap();

        let err = tracker.ensure_ready().await.unwrap_err();
        assert!(
            matches!(err, TorrentError::BuiltinTrackerUnavailable),
            "got {err:?}"
        );
        // The message has to point at the backend that does work today.
        assert!(err.to_string().contains("qbittorrent-embedded"), "{err}");
    }

    /// Configuration written for the builtin backend today must stay valid when the
    /// server lands, so the URL scheme is settled now.
    #[tokio::test]
    async fn the_builtin_tracker_still_produces_its_announce_url() {
        let tracker = BuiltinTracker::new(Some("sharerr.example"), 8477, None).unwrap();
        assert_eq!(
            tracker.announce_url().await.unwrap().as_str(),
            "http://sharerr.example:8477/announce"
        );

        let with_token =
            BuiltinTracker::new(Some("sharerr.example"), 8477, Some("s3cret")).unwrap();
        assert_eq!(
            with_token.announce_url().await.unwrap().as_str(),
            "http://sharerr.example:8477/announce/s3cret"
        );
    }

    #[test]
    fn an_unset_advertised_host_is_refused_at_construction() {
        let err = BuiltinTracker::new(None, 8477, None).unwrap_err();
        assert!(matches!(err, TorrentError::NoAdvertisedHost), "got {err:?}");
    }

    #[test]
    fn announce_urls_survive_hosts_that_are_bare_addresses() {
        assert_eq!(
            announce_url("192.0.2.10", 9000, None).unwrap().as_str(),
            "http://192.0.2.10:9000/announce"
        );
    }
}
