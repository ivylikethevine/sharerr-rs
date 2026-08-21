//! Hermetic tests for Sonarr/Radarr discovery.
//!
//! Every fixture uses invented show and movie titles and invented paths — no real
//! titles, no real files, and no network beyond the local wiremock server.

// The workspace denies casual panics because the production code handles secrets.
// In a test, a panic on an unexpected value *is* the assertion.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use secrecy::SecretString;
use serde_json::json;
use sharerr_arr::{ArrClient, ArrError};
use sharerr_core::{MediaSource, MediaSpec};
use sharerr_testkit::library::TAG_ID;
use sharerr_testkit::mock::mount_json;
use url::Url;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const API_KEY: &str = "0123456789abcdef0123456789abcdef";

fn client(kind: MediaSource, server: &MockServer) -> ArrClient {
    let base = Url::parse(&server.uri()).expect("wiremock uri is a valid url");
    ArrClient::new(kind, &base, SecretString::from(API_KEY)).expect("client builds")
}

/// Mounts `GET /api/v3/tag` with the testkit's `sharerr` tag plus its decoy.
async fn mount_tags(server: &MockServer) {
    mount_json(server, "/api/v3/tag", sharerr_testkit::library::tag_json()).await;
}

// --------------------------------------------------------------- tag resolution

#[tokio::test]
async fn tag_is_resolved_case_insensitively() {
    let server = MockServer::start().await;
    mount_tags(&server).await;

    let client = client(MediaSource::Sonarr, &server);
    // Sonarr lowercases labels on save, so an operator who typed "Sharerr" in the
    // UI and "sharerr" in the config must still get a match.
    assert_eq!(client.tag_id("sharerr").await.unwrap(), TAG_ID);
    assert_eq!(client.tag_id("SHARERR").await.unwrap(), TAG_ID);
    assert_eq!(client.tag_id("ShArErR").await.unwrap(), TAG_ID);
}

#[tokio::test]
async fn a_missing_tag_is_a_named_error_listing_what_does_exist() {
    let server = MockServer::start().await;
    mount_tags(&server).await;

    let err = client(MediaSource::Sonarr, &server)
        .tag_id("shrerr")
        .await
        .unwrap_err();
    match &err {
        ArrError::TagNotFound {
            label, available, ..
        } => {
            assert_eq!(label, "shrerr");
            assert!(available.contains(&"sharerr".to_owned()));
        }
        other => panic!("expected TagNotFound, got {other:?}"),
    }
    // The message has to be actionable — it is the whole point of the variant.
    assert!(err.to_string().contains("sharerr"), "{err}");
}

/// `create_tag` — `sharerr doctor --fix`'s one write to a *arr app — sends the
/// label as a JSON body, not a query string, since that is the shape `/tag`
/// documents for creation.
#[tokio::test]
async fn create_tag_posts_the_label_as_json() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v3/tag"))
        .and(wiremock::matchers::body_json(json!({ "label": "sharerr" })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 9, "label": "sharerr"
        })))
        .expect(1)
        .mount(&server)
        .await;

    client(MediaSource::Sonarr, &server)
        .create_tag("sharerr")
        .await
        .unwrap();
}

/// A rejected key must be reported the same way every other call reports it,
/// not swallowed as a generic failure.
#[tokio::test]
async fn create_tag_reports_a_rejected_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v3/tag"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let err = client(MediaSource::Sonarr, &server)
        .create_tag("sharerr")
        .await
        .unwrap_err();
    assert!(matches!(err, ArrError::Unauthorized { .. }), "{err:?}");
}

// --------------------------------------------------------------- transport

#[tokio::test]
async fn the_api_key_is_sent_as_a_header() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v3/tag"))
        .and(header("x-api-key", API_KEY))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": TAG_ID, "label": "sharerr" }
        ])))
        // `expect` fails the test if the matcher never fires, so a client that
        // dropped the header would fail here rather than silently 404.
        .expect(1)
        .mount(&server)
        .await;

    assert_eq!(
        client(MediaSource::Sonarr, &server)
            .tag_id("sharerr")
            .await
            .unwrap(),
        TAG_ID
    );
}

#[tokio::test]
async fn a_rejected_key_is_distinguishable_from_an_unreachable_host() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v3/tag"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let err = client(MediaSource::Sonarr, &server)
        .tag_id("sharerr")
        .await
        .unwrap_err();
    assert!(err.is_auth_failure(), "got {err:?}");
    assert!(!err.is_unreachable(), "got {err:?}");
}

#[tokio::test]
async fn an_unreachable_host_is_distinguishable_from_a_rejected_key() {
    // Port 1 is privileged and unbound; connecting is refused immediately.
    let base = Url::parse("http://127.0.0.1:1").unwrap();
    let client = ArrClient::new(MediaSource::Radarr, &base, SecretString::from(API_KEY)).unwrap();

    let err = client.tag_id("sharerr").await.unwrap_err();
    assert!(err.is_unreachable(), "got {err:?}");
    assert!(!err.is_auth_failure(), "got {err:?}");
}

#[tokio::test]
async fn a_base_url_with_a_subpath_is_preserved() {
    let server = MockServer::start().await;
    // A reverse proxy serving Sonarr at /sonarr — joining must append to the
    // subpath, not replace it.
    Mock::given(method("GET"))
        .and(path("/sonarr/api/v3/tag"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": TAG_ID, "label": "sharerr" }
        ])))
        .expect(1)
        .mount(&server)
        .await;

    // Deliberately no trailing slash: the client has to add one.
    let base = Url::parse(&format!("{}/sonarr", server.uri())).unwrap();
    let client = ArrClient::new(MediaSource::Sonarr, &base, SecretString::from(API_KEY)).unwrap();
    assert_eq!(client.tag_id("sharerr").await.unwrap(), TAG_ID);
}

#[tokio::test]
async fn a_malformed_payload_names_the_endpoint_that_produced_it() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v3/tag"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{ this is not json"))
        .mount(&server)
        .await;

    let err = client(MediaSource::Sonarr, &server)
        .tag_id("sharerr")
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ArrError::Decode { path, .. } if path == "tag"),
        "got {err:?}"
    );
}

/// A long non-ASCII error body must not panic the client: `String::truncate` asserts
/// on a char boundary, and a localized error page from a reverse proxy can put a
/// multi-byte character wherever it likes.
#[tokio::test]
async fn a_long_non_ascii_error_body_does_not_panic() {
    let server = MockServer::start().await;
    // Pad so a two-byte character straddles the byte cutoff exactly.
    let body = format!("{}é{}", "a".repeat(399), "b".repeat(500));
    Mock::given(method("GET"))
        .and(path("/api/v3/tag"))
        .respond_with(ResponseTemplate::new(502).set_body_string(body))
        .mount(&server)
        .await;

    let err = client(MediaSource::Sonarr, &server)
        .tag_id("sharerr")
        .await
        .unwrap_err();
    match &err {
        ArrError::Status { status, body, .. } => {
            assert_eq!(*status, 502);
            assert!(body.chars().count() <= 400, "body should be clamped");
            assert!(!body.is_empty());
        }
        other => panic!("expected Status, got {other:?}"),
    }
}

#[tokio::test]
async fn system_status_probes_liveness() {
    let server = MockServer::start().await;
    mount_json(
        &server,
        "/api/v3/system/status",
        json!({ "appName": "Sonarr", "version": "4.0.15.2941", "instanceName": "Sonarr" }),
    )
    .await;

    let status = client(MediaSource::Sonarr, &server)
        .system_status()
        .await
        .unwrap();
    assert_eq!(status.app_name, "Sonarr");
    assert_eq!(status.version, "4.0.15.2941");
}

// --------------------------------------------------------------- sonarr

async fn mount_sonarr_library(server: &MockServer) {
    mount_tags(server).await;
    mount_json(
        server,
        "/api/v3/series",
        json!([
            {
                "id": 11,
                "title": "Lanternwick Hollow",
                "tvdbId": 918273,
                "tvMazeId": 4242,
                "imdbId": "tt7654321",
                "tags": [TAG_ID],
            },
            {
                // Untagged: must not appear in the results at all.
                "id": 12,
                "title": "Copper Vale Station",
                "tvdbId": 112233,
                "tags": [1],
            },
        ]),
    )
    .await;

    Mock::given(method("GET"))
        .and(path("/api/v3/episodefile"))
        .and(query_param("seriesId", "11"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "id": 501,
                "path": "/tv/Lanternwick Hollow/Season 02/lanternwick.s02e01.mkv",
                "size": 2147483648_u64,
                "sceneName": "Lanternwick.Hollow.S02E01.1080p.WEB-DL.DD5.1.H.264-FAKEGRP",
            },
            {
                // A double-length premiere: one file, two episodes.
                "id": 502,
                "path": "/tv/Lanternwick Hollow/Season 02/lanternwick.s02e02e03.mkv",
                "size": 4294967296_u64,
            },
            {
                // Orphaned by a failed import — no episode points at it.
                "id": 599,
                "path": "/tv/Lanternwick Hollow/Season 02/stray.mkv",
                "size": 1024,
            },
        ])))
        .mount(server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/v3/episode"))
        .and(query_param("seriesId", "11"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "seasonNumber": 2, "episodeNumber": 1, "episodeFileId": 501 },
            // Reversed order on purpose: the lowest episode must still win.
            { "seasonNumber": 2, "episodeNumber": 3, "episodeFileId": 502 },
            { "seasonNumber": 2, "episodeNumber": 2, "episodeFileId": 502 },
            { "seasonNumber": 2, "episodeNumber": 4, "episodeFileId": 0 },
        ])))
        .mount(server)
        .await;
}

#[tokio::test]
async fn sonarr_discovers_only_tagged_series() {
    let server = MockServer::start().await;
    mount_sonarr_library(&server).await;

    let found = client(MediaSource::Sonarr, &server)
        .discover("sharerr")
        .await
        .unwrap();

    assert_eq!(
        found.len(),
        2,
        "the orphaned file and the untagged series must be excluded"
    );
    assert!(found.iter().all(|d| d.source_id == 11));
    assert!(
        !found
            .iter()
            .any(|d| d.spec.title() == "Copper Vale Station"),
        "an untagged series leaked into discovery"
    );
    assert!(
        !found.iter().any(|d| d.file_id == 599),
        "the orphaned file was not skipped"
    );
}

/// Discovery fetches tagged series concurrently and zips the responses back onto
/// the series list, so a response arriving out of order must not attach one
/// series' files to another's metadata. The first series' lookups are delayed
/// past the second's to force exactly that interleaving.
#[tokio::test]
async fn sonarr_pairs_each_series_with_its_own_files_when_responses_race() {
    let server = MockServer::start().await;
    mount_tags(&server).await;
    mount_json(
        &server,
        "/api/v3/series",
        json!([
            { "id": 11, "title": "Lanternwick Hollow", "tvdbId": 918273, "tags": [TAG_ID] },
            { "id": 21, "title": "Harrowmere", "tvdbId": 445566, "tags": [TAG_ID] },
        ]),
    )
    .await;

    // Series 11 answers slowly, so series 21's response lands first.
    let slow = std::time::Duration::from_millis(150);
    for (series_id, file_id, delay) in [(11, 501, slow), (21, 601, std::time::Duration::ZERO)] {
        Mock::given(method("GET"))
            .and(path("/api/v3/episodefile"))
            .and(query_param("seriesId", series_id.to_string()))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(delay)
                    .set_body_json(json!([{
                        "id": file_id,
                        "path": format!("/tv/{series_id}/ep.mkv"),
                        "size": 1024,
                    }])),
            )
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/api/v3/episode"))
            .and(query_param("seriesId", series_id.to_string()))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(delay)
                    .set_body_json(json!([
                        { "seasonNumber": 1, "episodeNumber": 1, "episodeFileId": file_id },
                    ])),
            )
            .mount(&server)
            .await;
    }

    let found = client(MediaSource::Sonarr, &server)
        .discover("sharerr")
        .await
        .unwrap();

    assert_eq!(found.len(), 2);
    for (file_id, source_id, title) in [(501, 11, "Lanternwick Hollow"), (601, 21, "Harrowmere")] {
        let item = found
            .iter()
            .find(|d| d.file_id == file_id)
            .unwrap_or_else(|| panic!("file {file_id} discovered"));
        assert_eq!(
            item.source_id, source_id,
            "file {file_id} got the wrong series"
        );
        assert_eq!(
            item.spec.title(),
            title,
            "file {file_id} got the wrong title"
        );
    }
}

#[tokio::test]
async fn sonarr_carries_metadata_ids_and_the_unmapped_path() {
    let server = MockServer::start().await;
    mount_sonarr_library(&server).await;

    let found = client(MediaSource::Sonarr, &server)
        .discover("sharerr")
        .await
        .unwrap();
    let first = found
        .iter()
        .find(|d| d.file_id == 501)
        .expect("file 501 discovered");

    assert_eq!(first.key(), (MediaSource::Sonarr, 501));
    assert_eq!(
        first.spec,
        MediaSpec::Episode {
            series_title: "Lanternwick Hollow".to_owned(),
            season: 2,
            episode: 1,
        }
    );
    assert_eq!(first.ids.tvdb, Some(918273));
    assert_eq!(first.ids.tvmaze, Some(4242));
    assert_eq!(first.ids.imdb.as_deref(), Some("tt7654321"));
    assert_eq!(first.ids.tmdb, None);
    assert_eq!(first.size, 2_147_483_648);
    // Stored exactly as Sonarr reported it — before any path mapping.
    assert_eq!(
        first.arr_path.to_str(),
        Some("/tv/Lanternwick Hollow/Season 02/lanternwick.s02e01.mkv")
    );
    assert_eq!(
        first.scene_name.as_deref(),
        Some("Lanternwick.Hollow.S02E01.1080p.WEB-DL.DD5.1.H.264-FAKEGRP")
    );
}

#[tokio::test]
async fn a_multi_episode_file_is_named_by_its_lowest_episode() {
    let server = MockServer::start().await;
    mount_sonarr_library(&server).await;

    let found = client(MediaSource::Sonarr, &server)
        .discover("sharerr")
        .await
        .unwrap();
    let double = found
        .iter()
        .find(|d| d.file_id == 502)
        .expect("file 502 discovered");

    assert_eq!(
        double.spec,
        MediaSpec::Episode {
            series_title: "Lanternwick Hollow".to_owned(),
            season: 2,
            episode: 2,
        }
    );
    assert_eq!(double.scene_name, None, "no sceneName in the fixture");
}

#[tokio::test]
async fn a_tag_that_nothing_carries_yields_an_empty_result_not_an_error() {
    let server = MockServer::start().await;
    mount_tags(&server).await;
    mount_json(
        &server,
        "/api/v3/series",
        json!([{ "id": 12, "title": "Copper Vale Station", "tags": [1] }]),
    )
    .await;

    // Distinct from TagNotFound: the tag exists, nothing carries it yet.
    let found = client(MediaSource::Sonarr, &server)
        .discover("sharerr")
        .await
        .unwrap();
    assert!(found.is_empty());
}

// --------------------------------------------------------------- radarr

#[tokio::test]
async fn radarr_discovers_tagged_movies_from_the_embedded_file() {
    let server = MockServer::start().await;
    mount_tags(&server).await;
    mount_json(
        &server,
        "/api/v3/movie",
        json!([
            {
                "id": 31,
                "title": "The Gilded Ferry",
                "year": 2019,
                "tmdbId": 555444,
                "imdbId": "tt1234567",
                "tags": [TAG_ID],
                "hasFile": true,
                "movieFile": {
                    "id": 900,
                    "path": "/movies/The Gilded Ferry (2019)/gilded.ferry.2019.mkv",
                    "size": 8589934592_u64,
                    "sceneName": "The.Gilded.Ferry.2019.1080p.BluRay.x264-FAKEGRP",
                },
            },
            {
                "id": 32,
                "title": "Paper Lantern Sky",
                "year": 2021,
                "tags": [1],
                "hasFile": true,
                "movieFile": { "id": 901, "path": "/movies/nope.mkv", "size": 100 },
            },
            {
                // Tagged but not yet downloaded — nothing to share.
                "id": 33,
                "title": "Harrowmere",
                "year": 2024,
                "tags": [TAG_ID],
                "hasFile": false,
            },
        ]),
    )
    .await;

    let found = client(MediaSource::Radarr, &server)
        .discover("sharerr")
        .await
        .unwrap();

    assert_eq!(found.len(), 1);
    let movie = &found[0];
    assert_eq!(movie.key(), (MediaSource::Radarr, 900));
    assert_eq!(
        movie.spec,
        MediaSpec::Movie {
            title: "The Gilded Ferry".to_owned(),
            year: Some(2019)
        }
    );
    assert_eq!(movie.ids.tmdb, Some(555444));
    assert_eq!(movie.ids.imdb.as_deref(), Some("tt1234567"));
    assert_eq!(movie.ids.tvdb, None);
    assert_eq!(movie.size, 8_589_934_592);
}

#[tokio::test]
async fn radarr_falls_back_to_the_moviefile_endpoint() {
    let server = MockServer::start().await;
    mount_tags(&server).await;
    mount_json(
        &server,
        "/api/v3/movie",
        json!([{
            // hasFile, but the resource did not inline it.
            "id": 31,
            "title": "The Gilded Ferry",
            "year": 2019,
            "tmdbId": 555444,
            "tags": [TAG_ID],
            "hasFile": true,
        }]),
    )
    .await;

    Mock::given(method("GET"))
        .and(path("/api/v3/moviefile"))
        .and(query_param("movieId", "31"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "id": 900,
            "path": "/movies/The Gilded Ferry (2019)/gilded.ferry.2019.mkv",
            "size": 8589934592_u64,
        }])))
        .expect(1)
        .mount(&server)
        .await;

    let found = client(MediaSource::Radarr, &server)
        .discover("sharerr")
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].file_id, 900);
}

#[tokio::test]
async fn radarr_placeholder_values_become_none() {
    let server = MockServer::start().await;
    mount_tags(&server).await;
    mount_json(
        &server,
        "/api/v3/movie",
        json!([{
            "id": 31,
            "title": "Harrowmere",
            // Both apps use these instead of null for "unset".
            "year": 0,
            "tmdbId": 0,
            "imdbId": "",
            "tags": [TAG_ID],
            "hasFile": true,
            "movieFile": { "id": 900, "path": "/movies/harrowmere.mkv", "size": 5, "sceneName": "" },
        }]),
    )
    .await;

    let found = client(MediaSource::Radarr, &server)
        .discover("sharerr")
        .await
        .unwrap();
    let movie = &found[0];
    assert_eq!(
        movie.spec,
        MediaSpec::Movie {
            title: "Harrowmere".to_owned(),
            year: None
        }
    );
    assert_eq!(movie.ids.tmdb, None);
    assert_eq!(movie.ids.imdb, None);
    assert_eq!(movie.scene_name, None);
}

#[tokio::test]
async fn discovered_items_promote_to_storable_items() {
    let server = MockServer::start().await;
    mount_sonarr_library(&server).await;

    let found = client(MediaSource::Sonarr, &server)
        .discover("sharerr")
        .await
        .unwrap();
    let discovered = found.into_iter().find(|d| d.file_id == 501).unwrap();
    let key = discovered.key();

    let item = discovered.into_shared_item("Some.Release.Title-SHARERR".to_owned());
    assert_eq!(
        item.key(),
        key,
        "the natural key must survive the conversion"
    );
    assert_eq!(item.release_title, "Some.Release.Title-SHARERR");
    assert_eq!(item.state, sharerr_core::ShareState::Pending);
    assert!(item.id.is_none());
    assert!(item.info_hash.is_none());
}

// --------------------------------------------------------------- music & books

/// Lidarr and Readarr are on API **v1**, not v3. A client that assumed one prefix
/// for every *arr app 404s here in a way that looks like a wrong URL.
#[tokio::test]
async fn lidarr_is_reached_on_api_v1() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tag"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": TAG_ID, "label": "sharerr" }
        ])))
        .mount(&server)
        .await;

    let client = client(MediaSource::Lidarr, &server);
    assert_eq!(client.tag_id("sharerr").await.unwrap(), TAG_ID);
}

async fn mount_lidarr(server: &MockServer) {
    mount_json(
        server,
        "/api/v1/tag",
        json!([{ "id": TAG_ID, "label": "sharerr" }]),
    )
    .await;
    mount_json(
        server,
        "/api/v1/artist",
        json!([{
            "id": 11,
            "artistName": "Lanternwick Ensemble",
            "tags": [TAG_ID],
            "foreignArtistId": "artist-mbid"
        }]),
    )
    .await;
    mount_json(
        server,
        "/api/v1/album",
        json!([{ "id": 21, "title": "Hollow Songs", "foreignAlbumId": "album-mbid" }]),
    )
    .await;
}

#[tokio::test]
async fn lidarr_discovers_a_tagged_artists_files() {
    let server = MockServer::start().await;
    mount_lidarr(&server).await;
    mount_json(
        &server,
        "/api/v1/trackfile",
        json!([{
            "id": 31, "albumId": 21,
            "path": "/music/Lanternwick Ensemble/Hollow Songs/01.flac",
            "size": 41_943_040_u64
        }]),
    )
    .await;
    // One track claims the file, so it is that track.
    mount_json(
        &server,
        "/api/v1/track",
        json!([{ "trackFileId": 31, "absoluteTrackNumber": 1 }]),
    )
    .await;

    let found = client(MediaSource::Lidarr, &server)
        .discover("sharerr")
        .await
        .unwrap();

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].source, MediaSource::Lidarr);
    assert_eq!(
        found[0].spec,
        MediaSpec::Track {
            artist: "Lanternwick Ensemble".to_owned(),
            album: "Hollow Songs".to_owned(),
            track: Some(1),
        }
    );
    // The album's MusicBrainz id is what a friend's Lidarr matches on.
    assert_eq!(found[0].ids.musicbrainz.as_deref(), Some("album-mbid"));
}

/// A file several tracks point at holds a whole album, and naming it after any one
/// of them would be wrong.
#[tokio::test]
async fn a_lidarr_file_holding_a_whole_album_carries_no_track_number() {
    let server = MockServer::start().await;
    mount_lidarr(&server).await;
    mount_json(
        &server,
        "/api/v1/trackfile",
        json!([{
            "id": 31, "albumId": 21,
            "path": "/music/Lanternwick Ensemble/Hollow Songs/album.flac",
            "size": 419_430_400_u64
        }]),
    )
    .await;
    mount_json(
        &server,
        "/api/v1/track",
        json!([
            { "trackFileId": 31, "absoluteTrackNumber": 1 },
            { "trackFileId": 31, "absoluteTrackNumber": 2 }
        ]),
    )
    .await;

    let found = client(MediaSource::Lidarr, &server)
        .discover("sharerr")
        .await
        .unwrap();

    assert_eq!(found.len(), 1);
    assert!(
        matches!(found[0].spec, MediaSpec::Track { track: None, .. }),
        "a multi-track file must not claim to be one track: {:?}",
        found[0].spec
    );
}

#[tokio::test]
async fn readarr_discovers_a_tagged_authors_books() {
    let server = MockServer::start().await;
    mount_json(
        &server,
        "/api/v1/tag",
        json!([{ "id": TAG_ID, "label": "sharerr" }]),
    )
    .await;
    mount_json(
        &server,
        "/api/v1/author",
        json!([{ "id": 41, "authorName": "Marisol Vane", "tags": [TAG_ID] }]),
    )
    .await;
    mount_json(
        &server,
        "/api/v1/book",
        json!([{
            "id": 51, "title": "The Gilded Ferry", "foreignBookId": "gr-9001",
            "editions": [
                { "isbn13": "9780000000001", "monitored": false },
                { "isbn13": "9780000000002", "monitored": true }
            ]
        }]),
    )
    .await;
    mount_json(
        &server,
        "/api/v1/bookfile",
        json!([{
            "id": 61, "bookId": 51,
            "path": "/books/Marisol Vane/The Gilded Ferry.epub",
            "size": 1_048_576_u64
        }]),
    )
    .await;

    let found = client(MediaSource::Readarr, &server)
        .discover("sharerr")
        .await
        .unwrap();

    assert_eq!(found.len(), 1);
    assert_eq!(
        found[0].spec,
        MediaSpec::Book {
            author: "Marisol Vane".to_owned(),
            title: "The Gilded Ferry".to_owned(),
        }
    );
    assert_eq!(found[0].ids.goodreads.as_deref(), Some("gr-9001"));
    // The monitored edition's ISBN, not just the first one listed.
    assert_eq!(found[0].ids.isbn.as_deref(), Some("9780000000002"));
}

/// Whisparr is Sonarr's codebase, so it walks series and episode files with the
/// same code and the same v3 prefix — the difference is only what it catalogues.
#[tokio::test]
async fn whisparr_walks_like_sonarr() {
    let server = MockServer::start().await;
    mount_tags(&server).await;
    mount_json(
        &server,
        "/api/v3/series",
        json!([{ "id": 71, "title": "Lanternwick After Dark", "tags": [TAG_ID] }]),
    )
    .await;
    mount_json(
        &server,
        "/api/v3/episodefile",
        json!([{ "id": 81, "seriesId": 71, "path": "/xxx/a.mkv", "size": 1024_u64 }]),
    )
    .await;
    mount_json(
        &server,
        "/api/v3/episode",
        json!([{ "seriesId": 71, "episodeFileId": 81, "seasonNumber": 1, "episodeNumber": 1 }]),
    )
    .await;

    let found = client(MediaSource::Whisparr, &server)
        .discover("sharerr")
        .await
        .unwrap();

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].source, MediaSource::Whisparr);
}
