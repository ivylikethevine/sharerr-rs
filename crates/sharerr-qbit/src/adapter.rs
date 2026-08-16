//! [`TorrentClient`] for qBittorrent.
//!
//! A thin translation rather than a rewrite: this crate's own types predate the
//! trait and stay as they are, because they mirror qBittorrent's wire format and
//! that is what makes them easy to check against its documentation. The mapping
//! lives here so neither side has to compromise.

use async_trait::async_trait;
use sharerr_client::{
    AddRequest, ClientError, ClientKind, Result, TorrentClient, TorrentFileEntry, TorrentSummary,
};

use crate::QbitClient;
use crate::error::QbitError;

/// Translate a qBittorrent error into the shared shape.
///
/// The two predicates this crate already exposes are exactly the distinction the
/// shared error preserves, so nothing is lost — an unreachable service and a
/// rejected password stay apart, and everything else becomes an API error carrying
/// the original text.
impl QbitClient {
    fn translate(&self, err: QbitError) -> ClientError {
        let kind = ClientKind::QBittorrent;
        if err.is_auth_failure() {
            return ClientError::AuthRejected { kind };
        }
        if err.is_unreachable() {
            return ClientError::Unreachable {
                kind,
                url: self.base_url().to_string(),
                detail: err.to_string(),
            };
        }
        ClientError::Api {
            kind,
            detail: err.to_string(),
        }
    }
}

#[async_trait]
impl TorrentClient for QbitClient {
    fn kind(&self) -> ClientKind {
        ClientKind::QBittorrent
    }

    async fn login(&self) -> Result<()> {
        QbitClient::login(self).await.map_err(|e| self.translate(e))
    }

    async fn version(&self) -> Result<String> {
        QbitClient::version(self)
            .await
            .map_err(|e| self.translate(e))
    }

    async fn list(&self, category: Option<&str>) -> Result<Vec<TorrentSummary>> {
        let torrents = self
            .torrents_info(category, None)
            .await
            .map_err(|e| self.translate(e))?;

        Ok(torrents
            .into_iter()
            .map(|t| TorrentSummary {
                is_seeding: t.is_seeding(),
                tags: t.tag_list().into_iter().map(str::to_owned).collect(),
                hash: t.hash,
                name: t.name,
                save_path: t.save_path,
                content_path: t.content_path,
                category: t.category,
            })
            .collect())
    }

    async fn files(&self, hash: &str) -> Result<Vec<TorrentFileEntry>> {
        let files = self
            .torrent_files(hash)
            .await
            .map_err(|e| self.translate(e))?;

        Ok(files
            .into_iter()
            .map(|f| TorrentFileEntry {
                name: f.name,
                size: f.size,
            })
            .collect())
    }

    async fn add(&self, request: &AddRequest<'_>) -> Result<()> {
        self.add_torrent(request)
            .await
            .map_err(|e| self.translate(e))
    }

    async fn remove(&self, hash: &str) -> Result<()> {
        // `remove_torrent` already passes `deleteFiles=false`; the media is the
        // operator's and predates sharerr knowing about it.
        self.remove_torrent(hash)
            .await
            .map_err(|e| self.translate(e))
    }

    async fn embedded_tracker_port(&self) -> Result<Option<u16>> {
        // qBittorrent is the one client in this project that *has* one, which is
        // why the trait's return type is an Option at all.
        self.ensure_embedded_tracker()
            .await
            .map(Some)
            .map_err(|e| self.translate(e))
    }
}
