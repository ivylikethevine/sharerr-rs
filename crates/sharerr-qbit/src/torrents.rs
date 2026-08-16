//! Torrent listing, inspection, and adding.

use reqwest::Method;
use reqwest::multipart::{Form, Part};

use crate::client::QbitClient;
use crate::error::{QbitError, Result};
use sharerr_client::AddRequest;

use crate::models::{TorrentFile, TorrentInfo};

/// qBittorrent wants the part typed as a real torrent, not `application/octet-stream`.
const TORRENT_MIME: &str = "application/x-bittorrent";

impl QbitClient {
    /// `GET /api/v2/torrents/info`.
    ///
    /// `category` and `tag` narrow the result server-side; passing `None` for both
    /// lists everything, which is what existing-torrent detection needs.
    pub async fn torrents_info(
        &self,
        category: Option<&str>,
        tag: Option<&str>,
    ) -> Result<Vec<TorrentInfo>> {
        let build = move |rb: reqwest::RequestBuilder| {
            let mut query: Vec<(&str, &str)> = Vec::new();
            if let Some(category) = category {
                query.push(("category", category));
            }
            if let Some(tag) = tag {
                query.push(("tag", tag));
            }
            rb.query(&query)
        };
        self.send_json(Method::GET, "torrents/info", &build).await
    }

    /// `GET /api/v2/torrents/files` — the contents of one torrent.
    ///
    /// Paths are relative to that torrent's `save_path`; join them against
    /// [`TorrentInfo::save_path`] to compare against a file on disk.
    pub async fn torrent_files(&self, hash: &str) -> Result<Vec<TorrentFile>> {
        let build = move |rb: reqwest::RequestBuilder| rb.query(&[("hash", hash)]);
        self.send_json(Method::GET, "torrents/files", &build).await
    }

    /// `POST /api/v2/torrents/add` — start seeding content that already exists.
    ///
    /// Two invariants are enforced here rather than left to callers, because
    /// getting either wrong causes qBittorrent to **relocate the user's files** —
    /// the one outcome this project must never produce:
    ///
    /// * `autoTMM=false`, unconditionally and with no way to override it.
    ///   Automatic Torrent Management moves content to a category-derived path the
    ///   moment the torrent is added.
    /// * `savepath` is the directory the content already occupies, so qBittorrent
    ///   finds it in place and has no reason to move anything.
    pub async fn add_torrent(&self, request: &AddRequest<'_>) -> Result<()> {
        let build = move |rb: reqwest::RequestBuilder| {
            // Rebuilt per attempt: a multipart Form cannot be cloned for a retry.
            let part = Part::bytes(request.data.to_vec())
                .file_name(request.filename.to_owned())
                .mime_str(TORRENT_MIME)
                .unwrap_or_else(|_| Part::bytes(request.data.to_vec()));

            let mut form = Form::new()
                .part("torrents", part)
                .text("savepath", request.save_path.to_owned())
                // Never configurable. See the doc comment above.
                .text("autoTMM", "false")
                .text("skip_checking", bool_str(request.skip_checking))
                .text("stopped", bool_str(request.stopped))
                // `paused` is the pre-5.0 spelling of `stopped`; sending both keeps
                // one client working across the versions people actually run.
                .text("paused", bool_str(request.stopped));

            if let Some(category) = request.category {
                form = form.text("category", category.to_owned());
            }
            if let Some(tags) = request.tags {
                form = form.text("tags", tags.to_owned());
            }

            rb.multipart(form)
        };

        let body = self.send_ok(Method::POST, "torrents/add", &build).await?;

        // As with login, a rejected torrent can still arrive as HTTP 200.
        if body.trim().eq_ignore_ascii_case("Fails.") {
            return Err(QbitError::InvalidTorrent {
                name: request.filename.to_owned(),
            });
        }

        tracing::info!(
            file = request.filename,
            save_path = request.save_path,
            "handed torrent to qBittorrent"
        );
        Ok(())
    }

    /// `POST /api/v2/torrents/delete` — stop seeding.
    ///
    /// `delete_files` is hardcoded `false`. sharerr shares files it does not own;
    /// removing a share must never remove the user's media.
    pub async fn remove_torrent(&self, hash: &str) -> Result<()> {
        let build = move |rb: reqwest::RequestBuilder| {
            rb.form(&[("hashes", hash), ("deleteFiles", "false")])
        };
        self.send_ok(Method::POST, "torrents/delete", &build)
            .await?;
        Ok(())
    }
}

/// qBittorrent parses these as strings, not JSON booleans.
fn bool_str(value: bool) -> String {
    if value {
        "true".to_owned()
    } else {
        "false".to_owned()
    }
}
