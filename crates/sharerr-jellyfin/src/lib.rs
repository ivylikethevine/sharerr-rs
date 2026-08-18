//! Jellyfin (and Emby) as a library source.
//!
//! Not everyone runs the *arr apps. A media-server-backed source lets someone
//! share a library they curate elsewhere: tag an item in Jellyfin with the share
//! tag, and sharerr discovers it the same way it discovers a tagged series in
//! Sonarr. Emby speaks the same API surface this client uses — `/System/Info`,
//! `/Items`, `/Shows/{id}/Episodes`, the `X-Emby-Token` header — so one client
//! covers both.
//!
//! # What the walk covers
//!
//! Tags in Jellyfin live on items, and the useful places to put a share tag are
//! the *container* items, so that is what the walk asks for:
//!
//! * **Movies** tagged directly — the item is the file.
//! * **Series** tagged as a whole — every episode with a file is shared.
//! * **Music albums** tagged as a whole — every track is shared.
//! * **Books** tagged directly.
//!
//! A tag on an individual episode or track is not discovered; the sentence "tag
//! the series, not the episode" is easier to act on than a partial rule about
//! which of the two wins.
//!
//! # What is honestly weaker than an *arr source
//!
//! The external ids Jellyfin carries (`ProviderIds`) are passed through when
//! present, but they are only as good as Jellyfin's metadata match — and there
//! is no scene name at all, so release titles are synthesised. A friend's app
//! matches these releases less reliably than ones discovered from Sonarr. That
//! is a property of the source, not a bug in the walk.

use std::collections::HashMap;

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sharerr_client::{clamp_body, error_chain, normalise_base};
use sharerr_core::{Discovered, ExternalIds, MediaSource, MediaSpec};
use url::Url;

/// What can go wrong talking to Jellyfin.
///
/// Same taxonomy as the *arr and torrent clients: the variants exist to keep
/// apart the failures with different fixes.
#[derive(Debug, thiserror::Error)]
pub enum JellyfinError {
    #[error("Jellyfin at {url} could not be reached: {detail}")]
    Unreachable { url: String, detail: String },

    #[error("Jellyfin rejected the API key")]
    AuthRejected,

    #[error("Jellyfin answered {status}: {detail}")]
    Api { status: u16, detail: String },

    #[error("Jellyfin sent a response this build could not read: {detail}")]
    Malformed { detail: String },

    #[error("{0}")]
    Config(String),
}

pub type Result<T> = std::result::Result<T, JellyfinError>;

/// A Jellyfin/Emby API client, scoped to what discovery needs.
pub struct JellyfinClient {
    http: reqwest::Client,
    base: Url,
    api_key: SecretString,
}

impl std::fmt::Debug for JellyfinClient {
    /// Hand-written so the key cannot reach a log through a derived `Debug`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JellyfinClient")
            .field("base", &self.base.as_str())
            .field("api_key", &"<redacted>")
            .finish()
    }
}

// ---------------------------------------------------------------- wire types

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
/// What `/System/Info` reports: proof the server is reachable and the key works.
pub struct SystemInfo {
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub server_name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ItemsPage {
    #[serde(default)]
    items: Vec<Item>,
}

/// One item, whichever type it is — only the fields discovery reads, all
/// defaulted, because a Jellyfin release adding fields must never break the walk.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Item {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    production_year: Option<u16>,
    /// Episode number within the season.
    #[serde(default)]
    index_number: Option<u32>,
    /// Season number, for episodes.
    #[serde(default)]
    parent_index_number: Option<u32>,
    #[serde(default)]
    series_name: Option<String>,
    #[serde(default)]
    album: Option<String>,
    #[serde(default)]
    album_artist: Option<String>,
    #[serde(default)]
    provider_ids: HashMap<String, String>,
    #[serde(default)]
    media_sources: Vec<MediaSourceInfo>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct MediaSourceInfo {
    #[serde(default)]
    size: Option<u64>,
}

impl Item {
    fn size(&self) -> u64 {
        self.media_sources
            .iter()
            .find_map(|source| source.size)
            .unwrap_or(0)
    }

    /// Jellyfin's `ProviderIds` map, folded into sharerr's shape. Keys are as
    /// Jellyfin writes them; matched case-insensitively because Emby is not
    /// consistent about it.
    fn external_ids(&self) -> ExternalIds {
        let get = |name: &str| {
            self.provider_ids
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.clone())
        };
        ExternalIds {
            tvdb: get("Tvdb").and_then(|v| v.parse().ok()),
            tmdb: get("Tmdb").and_then(|v| v.parse().ok()),
            tvmaze: get("TvMaze").and_then(|v| v.parse().ok()),
            imdb: get("Imdb"),
            musicbrainz: get("MusicBrainzReleaseGroup").or_else(|| get("MusicBrainzAlbum")),
            goodreads: get("GoodReads"),
            isbn: get("ISBN"),
        }
    }
}

/// A stable 63-bit id for a Jellyfin item, standing in for the numeric file id
/// an *arr app would have assigned — the same construction the directory source
/// uses for paths. Derived from the item's GUID, so it survives restarts and a
/// file that Jellyfin moves keeps its identity.
fn item_id(guid: &str) -> i64 {
    let digest = Sha256::digest(guid.as_bytes());
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    i64::from_le_bytes(bytes) & i64::MAX
}

// ------------------------------------------------------------------- client

impl JellyfinClient {
    pub fn new(base: &Url, api_key: SecretString) -> Result<Self> {
        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| JellyfinError::Config(format!("building the HTTP client: {e}")))?;
        Ok(Self {
            http,
            base: normalise_base(base),
            api_key,
        })
    }

    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T> {
        let url = self.base.join(path).map_err(|e| {
            JellyfinError::Config(format!("{} is not a usable base URL: {e}", self.base))
        })?;

        let response = self
            .http
            .get(url)
            // The header both servers accept, for API keys and user tokens alike.
            .header("X-Emby-Token", self.api_key.expose_secret())
            .query(query)
            .send()
            .await
            .map_err(|e| JellyfinError::Unreachable {
                url: self.base.to_string(),
                detail: error_chain(&e),
            })?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(JellyfinError::AuthRejected);
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(JellyfinError::Api {
                status: status.as_u16(),
                detail: clamp_body(&body),
            });
        }

        response.json().await.map_err(|e| JellyfinError::Malformed {
            detail: error_chain(&e),
        })
    }

    /// `GET /System/Info` — reachability and the key, proved together.
    pub async fn system_info(&self) -> Result<SystemInfo> {
        self.get_json("System/Info", &[]).await
    }

    async fn tagged(&self, item_types: &str, tag: &str, fields: &str) -> Result<Vec<Item>> {
        let page: ItemsPage = self
            .get_json(
                "Items",
                &[
                    ("recursive", "true"),
                    ("includeItemTypes", item_types),
                    ("tags", tag),
                    ("fields", fields),
                ],
            )
            .await?;
        Ok(page.items)
    }

    /// Everything carrying the share tag, as [`Discovered`] items.
    ///
    /// Four walks — movies, series (expanded to episodes), albums (expanded to
    /// tracks), books — each type carrying its own spec. Every item needs a
    /// `Path`: an item Jellyfin cannot place on disk cannot be seeded, and is
    /// skipped with a log line rather than failing the walk.
    pub async fn discover(&self, tag: &str) -> Result<Vec<Discovered>> {
        let mut discovered = Vec::new();

        for movie in self
            .tagged("Movie", tag, "Path,ProviderIds,ProductionYear,MediaSources")
            .await?
        {
            let Some(path) = movie.path.clone().filter(|p| !p.is_empty()) else {
                tracing::warn!(item = %movie.name, "tagged movie has no path — skipped");
                continue;
            };
            discovered.push(Discovered {
                source: MediaSource::Jellyfin,
                source_id: item_id(&movie.id),
                file_id: item_id(&movie.id),
                spec: MediaSpec::Movie {
                    title: movie.name.clone(),
                    year: movie.production_year,
                },
                arr_path: path.into(),
                size: movie.size(),
                ids: movie.external_ids(),
                scene_name: None,
            });
        }

        for series in self.tagged("Series", tag, "ProviderIds").await? {
            let series_ids = series.external_ids();
            let episodes: ItemsPage = self
                .get_json(
                    &format!("Shows/{}/Episodes", series.id),
                    &[("fields", "Path,MediaSources")],
                )
                .await?;
            for episode in episodes.items {
                let Some(path) = episode.path.clone().filter(|p| !p.is_empty()) else {
                    // An episode with no file is Jellyfin knowing it exists, not
                    // having it — the normal case for future episodes.
                    continue;
                };
                discovered.push(Discovered {
                    source: MediaSource::Jellyfin,
                    source_id: item_id(&series.id),
                    file_id: item_id(&episode.id),
                    spec: MediaSpec::Episode {
                        series_title: episode
                            .series_name
                            .clone()
                            .unwrap_or_else(|| series.name.clone()),
                        season: episode.parent_index_number.unwrap_or(0),
                        episode: episode.index_number.unwrap_or(0),
                    },
                    arr_path: path.into(),
                    size: episode.size(),
                    // The series' ids, not the episode's: they are what a
                    // friend's Sonarr matches the release against.
                    ids: series_ids.clone(),
                    scene_name: None,
                });
            }
        }

        for album in self.tagged("MusicAlbum", tag, "ProviderIds").await? {
            let album_ids = album.external_ids();
            let tracks: ItemsPage = self
                .get_json(
                    "Items",
                    &[
                        ("parentId", album.id.as_str()),
                        ("includeItemTypes", "Audio"),
                        ("fields", "Path,MediaSources"),
                    ],
                )
                .await?;
            for track in tracks.items {
                let Some(path) = track.path.clone().filter(|p| !p.is_empty()) else {
                    continue;
                };
                discovered.push(Discovered {
                    source: MediaSource::Jellyfin,
                    source_id: item_id(&album.id),
                    file_id: item_id(&track.id),
                    spec: MediaSpec::Track {
                        artist: track
                            .album_artist
                            .clone()
                            .or_else(|| album.album_artist.clone())
                            .unwrap_or_else(|| "Unknown Artist".to_owned()),
                        album: track
                            .album
                            .clone()
                            .unwrap_or_else(|| album.name.clone()),
                        track: track.index_number,
                    },
                    arr_path: path.into(),
                    size: track.size(),
                    ids: album_ids.clone(),
                    scene_name: None,
                });
            }
        }

        for book in self
            .tagged("Book", tag, "Path,ProviderIds,MediaSources")
            .await?
        {
            let Some(path) = book.path.clone().filter(|p| !p.is_empty()) else {
                tracing::warn!(item = %book.name, "tagged book has no path — skipped");
                continue;
            };
            discovered.push(Discovered {
                source: MediaSource::Jellyfin,
                source_id: item_id(&book.id),
                file_id: item_id(&book.id),
                spec: MediaSpec::Book {
                    author: book
                        .album_artist
                        .clone()
                        .unwrap_or_else(|| "Unknown Author".to_owned()),
                    title: book.name.clone(),
                },
                arr_path: path.into(),
                size: book.size(),
                ids: book.external_ids(),
                scene_name: None,
            });
        }

        tracing::debug!(items = discovered.len(), tag, "jellyfin discovery");
        Ok(discovered)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> JellyfinClient {
        JellyfinClient::new(
            &Url::parse(&server.uri()).unwrap(),
            SecretString::from("jf-key"),
        )
        .unwrap()
    }

    /// Mount an empty answer for every tagged-items query except the ones a test
    /// mounts specifically — the walk always asks about all four types.
    async fn mount_empty_items(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/Items"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "Items": [], "TotalRecordCount": 0 })),
            )
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn system_info_carries_the_token_header() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/System/Info"))
            .and(header("X-Emby-Token", "jf-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "Version": "10.9.11",
                "ServerName": "shelf",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let info = client(&server).system_info().await.unwrap();
        assert_eq!(info.version, "10.9.11");
        assert_eq!(info.server_name, "shelf");
    }

    #[tokio::test]
    async fn a_rejected_key_is_reported_as_such() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        assert!(matches!(
            client(&server).system_info().await.unwrap_err(),
            JellyfinError::AuthRejected
        ));
    }

    #[tokio::test]
    async fn a_tagged_movie_becomes_a_discovered_item() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/Items"))
            .and(query_param("includeItemTypes", "Movie"))
            .and(query_param("tags", "sharerr"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "Items": [{
                    "Id": "f8d2c1aa000000000000000000000001",
                    "Name": "The Glassblower's Comet",
                    "Path": "/media/movies/The Glassblowers Comet (2019)/movie.mkv",
                    "ProductionYear": 2019,
                    "ProviderIds": { "Tmdb": "551199", "Imdb": "tt8811223" },
                    "MediaSources": [{ "Size": 734003200 }],
                }],
            })))
            .mount(&server)
            .await;
        mount_empty_items(&server).await;

        let items = client(&server).discover("sharerr").await.unwrap();
        assert_eq!(items.len(), 1);
        let movie = &items[0];
        assert_eq!(movie.source, MediaSource::Jellyfin);
        assert_eq!(
            movie.spec,
            MediaSpec::Movie {
                title: "The Glassblower's Comet".to_owned(),
                year: Some(2019),
            }
        );
        assert_eq!(
            movie.arr_path.to_str().unwrap(),
            "/media/movies/The Glassblowers Comet (2019)/movie.mkv"
        );
        assert_eq!(movie.size, 734003200);
        assert_eq!(movie.ids.tmdb, Some(551199));
        assert_eq!(movie.ids.imdb.as_deref(), Some("tt8811223"));
        assert!(movie.file_id > 0);
    }

    /// A tagged series expands to its episodes, each with the series' external
    /// ids — that is what a friend's Sonarr matches against.
    #[tokio::test]
    async fn a_tagged_series_expands_to_its_episodes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/Items"))
            .and(query_param("includeItemTypes", "Series"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "Items": [{
                    "Id": "aa11",
                    "Name": "Lanternwick Hollow",
                    "ProviderIds": { "Tvdb": "918273" },
                }],
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/Shows/aa11/Episodes"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "Items": [
                    {
                        "Id": "ep01",
                        "Name": "The Long Dark",
                        "SeriesName": "Lanternwick Hollow",
                        "ParentIndexNumber": 2,
                        "IndexNumber": 1,
                        "Path": "/media/tv/Lanternwick Hollow/S02E01.mkv",
                        "MediaSources": [{ "Size": 1024 }],
                    },
                    // Known but not on disk: Jellyfin lists it, sharerr must not
                    // try to share it.
                    { "Id": "ep02", "Name": "Future", "IndexNumber": 2 },
                ],
            })))
            .mount(&server)
            .await;
        mount_empty_items(&server).await;

        let items = client(&server).discover("sharerr").await.unwrap();
        assert_eq!(items.len(), 1, "the fileless episode must be skipped");
        assert_eq!(
            items[0].spec,
            MediaSpec::Episode {
                series_title: "Lanternwick Hollow".to_owned(),
                season: 2,
                episode: 1,
            }
        );
        assert_eq!(items[0].ids.tvdb, Some(918273));
        assert_ne!(
            items[0].source_id, items[0].file_id,
            "the series is the source, the episode is the file"
        );
    }

    /// A tagged album expands to its tracks.
    #[tokio::test]
    async fn a_tagged_album_expands_to_its_tracks() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/Items"))
            .and(query_param("includeItemTypes", "MusicAlbum"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "Items": [{
                    "Id": "al01",
                    "Name": "Copper Vale Sessions",
                    "AlbumArtist": "The Fen Lights",
                }],
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/Items"))
            .and(query_param("parentId", "al01"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "Items": [{
                    "Id": "tr01",
                    "Name": "Marsh Light",
                    "Album": "Copper Vale Sessions",
                    "AlbumArtist": "The Fen Lights",
                    "IndexNumber": 3,
                    "Path": "/media/music/The Fen Lights/Copper Vale Sessions/03.flac",
                    "MediaSources": [{ "Size": 2048 }],
                }],
            })))
            .mount(&server)
            .await;
        mount_empty_items(&server).await;

        let items = client(&server).discover("sharerr").await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].spec,
            MediaSpec::Track {
                artist: "The Fen Lights".to_owned(),
                album: "Copper Vale Sessions".to_owned(),
                track: Some(3),
            }
        );
    }

    /// The ids must be stable — they are the store's natural key, and a changed
    /// id would make every item look newly discovered after an upgrade.
    #[test]
    fn item_ids_are_stable_and_non_negative() {
        let a = item_id("f8d2c1aa000000000000000000000001");
        assert_eq!(a, item_id("f8d2c1aa000000000000000000000001"));
        assert!(a >= 0);
        assert_ne!(a, item_id("f8d2c1aa000000000000000000000002"));
    }

    #[tokio::test]
    async fn nothing_tagged_is_an_empty_walk_not_an_error() {
        let server = MockServer::start().await;
        mount_empty_items(&server).await;

        assert!(client(&server).discover("sharerr").await.unwrap().is_empty());
    }
}
