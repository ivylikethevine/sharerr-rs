//! Application preferences, and the embedded tracker that rides on them.

use reqwest::Method;

use crate::client::QbitClient;
use crate::error::Result;
use crate::models::Preferences;

impl QbitClient {
    /// `GET /api/v2/app/preferences`.
    pub async fn preferences(&self) -> Result<Preferences> {
        self.send_json(Method::GET, "app/preferences", &|rb| rb)
            .await
    }

    /// `POST /api/v2/app/preferences` with a partial JSON document.
    ///
    /// qBittorrent merges what it is given, so only the keys being changed need to
    /// be sent — important, since round-tripping the whole preference set would
    /// rewrite settings sharerr does not model.
    async fn set_preferences(&self, patch: &serde_json::Value) -> Result<()> {
        let body = patch.to_string();
        let build = move |rb: reqwest::RequestBuilder| rb.form(&[("json", body.as_str())]);
        self.send_ok(Method::POST, "app/preferences", &build)
            .await?;
        Ok(())
    }

    /// Ensure the embedded tracker is running and report the port it announces on.
    ///
    /// Idempotent: when the tracker is already enabled this is a single GET and
    /// changes nothing, so it is safe to call on every sync.
    pub async fn ensure_embedded_tracker(&self) -> Result<u16> {
        let prefs = self.preferences().await?;
        if prefs.enable_embedded_tracker {
            return Ok(prefs.embedded_tracker_port);
        }

        tracing::info!("enabling qBittorrent's embedded tracker");
        self.set_preferences(&serde_json::json!({ "enable_embedded_tracker": true }))
            .await?;

        // Re-read rather than assuming: qBittorrent assigns the port itself when
        // one was never configured, and an announce URL with the wrong port
        // produces torrents nobody can seed to.
        let prefs = self.preferences().await?;
        Ok(prefs.embedded_tracker_port)
    }
}
