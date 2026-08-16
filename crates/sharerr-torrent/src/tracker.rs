//! Where sharerr's torrents announce. Two working backends:
//!
//! * [`QbitEmbeddedTracker`] — qBittorrent has a tracker built in. Turning it on is
//!   a preferences write, and it is then serving immediately. The default, because
//!   it needs nothing from the operator.
//! * [`BuiltinTracker`] — sharerr serves `/announce` itself, from the same process
//!   and port as everything else. The protocol lives in [`crate::announce`] and the
//!   HTTP handlers in the binary crate.
//!
//! This module is only the *provider* side: what URL to embed in a new torrent and
//! what has to be true before doing so. Serving announces is a separate concern
//! with a separate home.

use std::sync::Arc;

use async_trait::async_trait;
use sharerr_client::TorrentClient;
use tokio::sync::Mutex;
use url::Url;

use crate::error::{Result, TorrentError};

#[async_trait]
/// Decides the announce URL a newly built torrent carries.
///
/// A trait because the choice is a configuration decision — qBittorrent's embedded
/// tracker or sharerr's own — made once and then applied to every torrent.
pub trait TrackerProvider: Send + Sync + std::fmt::Debug {
    /// Make the tracker serve, if it does not already.
    ///
    /// Must be idempotent: it runs at the start of every sync.
    async fn ensure_ready(&self) -> Result<()>;

    /// The announce URL to embed in new torrents.
    async fn announce_url(&self) -> Result<Url>;
}

/// The torrent client's own embedded tracker.
///
/// Only qBittorrent has one. The provider is written against the client trait
/// rather than qBittorrent specifically, so selecting a client without a tracker
/// fails with a sentence naming the fix instead of a type error — see
/// [`Self::served_port`].
#[derive(Debug)]
pub struct QbitEmbeddedTracker {
    qbit: Arc<dyn TorrentClient>,
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
        qbit: Arc<dyn TorrentClient>,
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

        let served = match self.qbit.embedded_tracker_port().await? {
            Some(port) => port,
            // Transmission and most other clients have no embedded tracker. This is
            // a configuration mistake rather than a fault, and the message has to
            // name the fix: switch the tracker backend to sharerr's own.
            None => {
                return Err(TorrentError::NoEmbeddedTracker {
                    client: self.qbit.kind().as_str(),
                });
            }
        };
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

/// sharerr's own tracker.
///
/// Unlike the qBittorrent backend there is nothing to turn on: the announce
/// endpoint is part of `sharerr serve` and is serving whenever the process is.
/// That is also the one caveat — a one-shot `sharerr sync` builds correct torrents
/// whose announces fail until `serve` is running, which is why `doctor` says so.
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
        // Nothing to enable or verify. The endpoint is mounted on the same router
        // that is answering this process's requests, so if anything is calling
        // this, the tracker is up.
        tracing::debug!(
            host = %self.advertised_host,
            port = self.port,
            token = self.token.is_some(),
            "builtin tracker ready"
        );
        Ok(())
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
    async fn the_builtin_tracker_is_ready_without_any_setup() {
        // Unlike the qBittorrent backend there is no preferences write and no port
        // to discover — the endpoint ships with the server.
        let tracker = BuiltinTracker::new(Some("sharerr.example"), 8477, None).unwrap();
        assert!(tracker.ensure_ready().await.is_ok());
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
