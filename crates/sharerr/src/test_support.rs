//! Test-only helpers shared by more than one module's `#[cfg(test)] mod
//! tests`, and the trait doubles those modules drive. `vault_in` was
//! hand-rolled byte-for-byte in four places (`gossip`, `lighthouse_client`,
//! `sync::tests`, `commands::doctor`) before landing here.
//!
//! The doubles live here rather than beside the tests that use them so the
//! coverage report can leave them out: `scripts/check.sh` ignores this file,
//! and a stub method no test calls (an `unimplemented!()` arm, a `Debug`
//! impl) is scaffolding, not product code a line of coverage should be
//! charged for.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use secrecy::SecretString;
use sharerr_client::{AddRequest, ClientKind, TorrentClient, TorrentFileEntry, TorrentSummary};
use url::Url;

/// A freshly opened vault backed by a throwaway file under `dir`, keyed by a
/// fixed master password that never touches the process env — see
/// `CLAUDE.md`'s "no tier-1 fixture opens a real vault". Anything under test
/// that takes a plain `&Vault` rather than going through `ServeState` (e.g.
/// `build_arr`/`build_client`/`build_tracker`) can be exercised directly
/// against one of these, no `SHARERR_MASTER_KEY` required.
pub(crate) fn vault_in(dir: &tempfile::TempDir) -> sharerr_store::Vault {
    trace();
    sharerr_store::Vault::open(dir.path().join("vault.bin"), &SecretString::from("master"))
        .expect("opening a fresh vault file cannot fail")
}

/// A pending episode row, for a test to override what it is about.
pub(crate) fn shared_item() -> sharerr_core::model::SharedItem {
    use sharerr_core::model::{ExternalIds, MediaSource, MediaSpec, ShareState, SharedItem};
    SharedItem {
        id: None,
        source: MediaSource::Sonarr,
        source_id: 1,
        file_id: 1,
        spec: MediaSpec::Episode {
            series_title: "Lanternwick Hollow".to_owned(),
            season: 1,
            episode: 1,
        },
        release_title: "Lanternwick.Hollow.S01E01.WEB-DL.x264-SHARERR".to_owned(),
        arr_path: std::path::PathBuf::from("/tv/s01e01.mkv"),
        size: 1,
        ids: ExternalIds::default(),
        media: None,
        info_hash: None,
        announce_token_fp: None,
        created_by_sharerr: true,
        private: true,
        state: ShareState::Pending,
        last_error: None,
        created_at: None,
        achieved_ratio: None,
        ratio_limit_reported: None,
    }
}

/// A friend who has never connected, for a test to override what it is about.
pub(crate) fn peer(label: &str, scope: sharerr_store::PeerScope) -> sharerr_store::Peer {
    sharerr_store::Peer {
        id: 1,
        label: label.to_owned(),
        created_at: 0,
        last_seen_at: None,
        revoked_at: None,
        scope,
        pubkey: None,
        gossip_url: None,
        key_hash: "hash".to_owned(),
        downloaded: 0,
        uploaded: 0,
        max_size_bytes: None,
        max_height: None,
    }
}

/// Install a process-wide `tracing` subscriber at `TRACE`, once, capturing
/// output per test the way `cargo test` captures `println!`.
///
/// Without a subscriber every `tracing::warn!(..)` and friend short-circuits
/// before evaluating its fields, so the `Err(err) => tracing::warn!(...)`
/// arms this suite drives on purpose execute only their first line. Every
/// fixture that builds a `ServeState`, `Vault`, `Seeder` or sync harness
/// calls this, so those arms run in full — and a field expression that
/// panics (a `Display` impl on a poisoned value, say) fails the test that
/// reached it instead of passing silently.
pub(crate) fn trace() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_test_writer()
        .try_init();
}

/// A tracker that always reports ready, standing in for the real one a
/// syncer built by `state::tests::ready` has no use for — nothing here ever runs a
/// sync pass, so it only has to satisfy the trait.
#[derive(Debug, Default)]
pub(crate) struct AlwaysReadyTracker;

#[async_trait::async_trait]
impl sharerr_torrent::TrackerProvider for AlwaysReadyTracker {
    async fn ensure_ready(&self) -> sharerr_torrent::Result<()> {
        Ok(())
    }
    async fn announce_set(&self) -> sharerr_torrent::Result<sharerr_torrent::AnnounceSet> {
        Ok(sharerr_torrent::AnnounceSet::single(
            url::Url::parse("http://sharerr.example/announce").unwrap(),
        ))
    }
}

/// An in-process `TorrentClient` double — no HTTP, so `Seeder`'s own logic
/// (as opposed to any one client's wire format) can be exercised alone.
/// `files_result` is a closure so a test can fail for one hash and succeed
/// for another, the way `sync::seed::Seeder::find_existing`'s multi-candidate loop needs.
pub(crate) type FilesFn =
    dyn Fn(&str) -> sharerr_client::Result<Vec<TorrentFileEntry>> + Send + Sync;

#[derive(Default)]
pub(crate) struct StubClient {
    pub(crate) files_result: Option<Box<FilesFn>>,
    pub(crate) set_trackers_calls: std::sync::Mutex<Vec<(String, Vec<Url>)>>,
    pub(crate) add_trackers_calls: std::sync::Mutex<Vec<(String, Vec<Url>)>>,
    pub(crate) add_calls: std::sync::Mutex<Vec<(String, Vec<u8>)>>,
    /// Each add's save path and multi-file root, in the same order.
    pub(crate) add_locations: std::sync::Mutex<Vec<(String, Option<String>)>>,
    pub(crate) set_limits_calls: std::sync::Mutex<Vec<(String, sharerr_client::SeedingLimits)>>,
    /// `.torrent` bytes `export` hands back, or `None` for a client that
    /// has no such call — the Transmission and rTorrent case.
    pub(crate) export_result: Option<Vec<u8>>,
    /// Refuse the named call with a `ClientError`: the arms behind a client
    /// that will not remove a torrent, apply limits or add a tracker, which
    /// a client that answers never reaches.
    pub(crate) fail_remove: bool,
    pub(crate) fail_set_limits: bool,
    pub(crate) fail_add_trackers: bool,
}

fn refused(call: &str) -> sharerr_client::Result<()> {
    Err(sharerr_client::ClientError::Config(format!(
        "{call} refused by the stub"
    )))
}

impl std::fmt::Debug for StubClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StubClient").finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl TorrentClient for StubClient {
    fn kind(&self) -> ClientKind {
        ClientKind::QBittorrent
    }
    async fn login(&self) -> sharerr_client::Result<()> {
        Ok(())
    }
    async fn version(&self) -> sharerr_client::Result<String> {
        Ok("stub".to_owned())
    }
    async fn list(&self, _category: Option<&str>) -> sharerr_client::Result<Vec<TorrentSummary>> {
        Ok(Vec::new())
    }
    async fn files(&self, hash: &str) -> sharerr_client::Result<Vec<TorrentFileEntry>> {
        match &self.files_result {
            Some(f) => f(hash),
            None => Ok(Vec::new()),
        }
    }
    async fn add(&self, request: &AddRequest<'_>) -> sharerr_client::Result<()> {
        self.add_calls
            .lock()
            .unwrap()
            .push((request.info_hash.to_owned(), request.data.to_vec()));
        self.add_locations.lock().unwrap().push((
            request.save_path.to_owned(),
            request.root_folder.map(str::to_owned),
        ));
        Ok(())
    }
    async fn remove(&self, _hash: &str) -> sharerr_client::Result<()> {
        if self.fail_remove {
            return refused("remove");
        }
        Ok(())
    }
    async fn set_trackers(&self, hash: &str, urls: &[Url]) -> sharerr_client::Result<()> {
        self.set_trackers_calls
            .lock()
            .unwrap()
            .push((hash.to_owned(), urls.to_vec()));
        Ok(())
    }
    async fn add_trackers(&self, hash: &str, urls: &[Url]) -> sharerr_client::Result<()> {
        if self.fail_add_trackers {
            return refused("add_trackers");
        }
        self.add_trackers_calls
            .lock()
            .unwrap()
            .push((hash.to_owned(), urls.to_vec()));
        Ok(())
    }
    async fn apply_limits(
        &self,
        hash: &str,
        limits: &sharerr_client::SeedingLimits,
    ) -> sharerr_client::Result<()> {
        if self.fail_set_limits {
            return refused("set_limits");
        }
        self.set_limits_calls
            .lock()
            .unwrap()
            .push((hash.to_owned(), *limits));
        Ok(())
    }
    async fn export(&self, _hash: &str) -> sharerr_client::Result<Option<Vec<u8>>> {
        Ok(self.export_result.clone())
    }
}
