//! Torrent listing, inspection, and adding.

use reqwest::Method;
use reqwest::multipart::{Form, Part};

use crate::client::QbitClient;
use crate::error::{QbitError, Result};
use sharerr_client::AddRequest;

use crate::models::{Category, TorrentFile, TorrentInfo, TrackerEntry};

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
    ///
    /// `upLimit` and `ratioLimit` — both documented `torrents/add` parameters
    /// (WebAPI 2.8.1+ / qBittorrent 4.4+) — ride the same request when a
    /// seeding goal is configured, so applying one costs no extra round
    /// trip. A qBittorrent that predates them ignores the unrecognised form
    /// fields, the same tolerance the dual `stopped`/`paused` fields above
    /// already rely on.
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
                .text("skip_checking", request.skip_checking.to_string())
                .text("stopped", request.stopped.to_string())
                // `paused` is the pre-5.0 spelling of `stopped`; sending both keeps
                // one client working across the versions people actually run.
                .text("paused", request.stopped.to_string());

            if let Some(category) = request.category {
                form = form.text("category", category.to_owned());
            }
            if let Some(tags) = request.tags {
                form = form.text("tags", tags.to_owned());
            }
            if let Some(kib) = request.upload_limit_kib {
                // `upLimit` is bytes/s; sharerr's config is KiB/s, matching
                // qBittorrent's own UI convention.
                form = form.text("upLimit", (kib * 1024).to_string());
            }
            if let Some(ratio) = request.ratio_limit {
                form = form.text("ratioLimit", ratio.to_string());
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

    /// `GET /api/v2/torrents/categories` — every category qBittorrent currently
    /// knows, keyed by name. Used only by `sharerr doctor --fix` to tell "the
    /// configured category does not exist yet" from "it does" before creating it.
    pub async fn categories(&self) -> Result<std::collections::HashMap<String, Category>> {
        self.send_json(Method::GET, "torrents/categories", &|rb| rb)
            .await
    }

    /// `POST /api/v2/torrents/createCategory`.
    ///
    /// No `savePath` is sent: sharerr always adds torrents with `autoTMM=false`
    /// and an explicit `savepath` (see [`Self::add_torrent`]), so nothing here
    /// ever depends on a category's own save path — creating one is only about
    /// making the category selectable at all.
    pub async fn create_category(&self, name: &str) -> Result<()> {
        let build = move |rb: reqwest::RequestBuilder| rb.form(&[("category", name)]);
        self.send_ok(Method::POST, "torrents/createCategory", &build)
            .await?;
        Ok(())
    }

    /// `GET /api/v2/torrents/trackers` — one torrent's tracker list.
    pub async fn torrent_trackers(&self, hash: &str) -> Result<Vec<TrackerEntry>> {
        let build = move |rb: reqwest::RequestBuilder| rb.query(&[("hash", hash)]);
        self.send_json(Method::GET, "torrents/trackers", &build)
            .await
    }

    /// Replace one torrent's tracker list with `urls`.
    ///
    /// Add-then-remove, in that order, so the torrent is never trackerless in
    /// between — a client that announces during the gap would drop out of the
    /// swarm. The `** [DHT] **`-style pseudo-entries qBittorrent lists are left
    /// alone; they are not URLs and removing them is not possible anyway.
    pub async fn set_torrent_trackers(&self, hash: &str, urls: &[String]) -> Result<()> {
        let existing = self.torrent_trackers(hash).await?;

        let additions: Vec<&str> = urls
            .iter()
            .map(String::as_str)
            .filter(|url| !existing.iter().any(|t| t.url == *url))
            .collect();
        self.post_tracker_urls(hash, "torrents/addTrackers", &additions, "\n")
            .await?;

        let stale: Vec<&str> = existing
            .iter()
            .map(|t| t.url.as_str())
            .filter(|url| !url.starts_with("**") && !urls.iter().any(|u| u == url))
            .collect();
        self.post_tracker_urls(hash, "torrents/removeTrackers", &stale, "|")
            .await?;

        tracing::info!(hash, trackers = urls.len(), "replaced tracker list");
        Ok(())
    }

    /// Add `urls` to one torrent's tracker list, leaving everything already
    /// there in place — the additive half of [`Self::set_torrent_trackers`]
    /// without the removal half.
    ///
    /// Filtered against the current list first. qBittorrent ignores a
    /// duplicate `addTrackers` rather than doubling the entry, so this is not
    /// load-bearing for correctness, but it keeps the call off the wire
    /// entirely on the common repeat pass.
    pub async fn add_torrent_trackers(&self, hash: &str, urls: &[String]) -> Result<()> {
        let existing = self.torrent_trackers(hash).await?;
        let additions: Vec<&str> = urls
            .iter()
            .map(String::as_str)
            .filter(|url| !existing.iter().any(|t| t.url == *url))
            .collect();
        if additions.is_empty() {
            return Ok(());
        }
        let added = additions.len();
        self.post_tracker_urls(hash, "torrents/addTrackers", &additions, "\n")
            .await?;
        tracing::info!(hash, added, "added trackers, keeping the existing ones");
        Ok(())
    }

    /// `GET /api/v2/torrents/export` — the `.torrent` file itself, as
    /// qBittorrent holds it.
    ///
    /// Read as bytes, never as text: this is bencode, and a `String` round
    /// trip would mangle the binary `pieces` field and with it the infohash.
    pub async fn export_torrent(&self, hash: &str) -> Result<Vec<u8>> {
        let build = move |rb: reqwest::RequestBuilder| rb.query(&[("hash", hash)]);
        let response = self.send(Method::GET, "torrents/export", &build).await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(QbitError::Status {
                status: status.as_u16(),
                path: "torrents/export".to_owned(),
                body: crate::client::clamp_body(&body),
            });
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|source| QbitError::Unreachable {
                url: "torrents/export".to_owned(),
                detail: sharerr_client::error_chain(&source),
            })?;
        Ok(bytes.to_vec())
    }

    /// Shared body of the add/remove halves of [`set_torrent_trackers`]: they
    /// differ only in endpoint and how qBittorrent wants the URL list joined.
    /// A no-op when `urls` is empty, so callers don't need to check first.
    async fn post_tracker_urls(
        &self,
        hash: &str,
        endpoint: &str,
        urls: &[&str],
        sep: &str,
    ) -> Result<()> {
        if urls.is_empty() {
            return Ok(());
        }
        let joined = urls.join(sep);
        let build = move |rb: reqwest::RequestBuilder| {
            rb.form(&[("hash", hash), ("urls", joined.as_str())])
        };
        self.send_ok(Method::POST, endpoint, &build).await?;
        Ok(())
    }
}
