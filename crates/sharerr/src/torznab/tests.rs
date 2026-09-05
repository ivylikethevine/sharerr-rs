#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use sharerr_core::model::{MediaSource, ShareState};
use std::path::PathBuf;

/// Every field populated, to prove each one reaches the feed under the name
/// a Torznab consumer expects.
fn full_media() -> sharerr_core::MediaMeta {
    sharerr_core::MediaMeta {
        resolution: Some("1920x1080".to_owned()),
        video_codec: Some("HEVC".to_owned()),
        dynamic_range: Some("HDR10".to_owned()),
        audio_codec: Some("EAC3".to_owned()),
        audio_channels: Some("5.1".to_owned()),
        audio_languages: Some("English/Japanese".to_owned()),
        subtitles: Some("English".to_owned()),
        runtime: Some("0:42:11".to_owned()),
        audio_sample_rate: Some("48000".to_owned()),
        audio_bit_depth: Some("24".to_owned()),
    }
}

#[test]
fn media_attributes_reach_the_feed_under_their_torznab_names() {
    let mut item = episode("Lanternwick.Hollow.S02E01", 2, 1);
    item.media = Some(full_media());
    let xml = render(&item);

    for (name, value) in [
        ("resolution", "1920x1080"),
        ("video", "HEVC"),
        ("audio", "EAC3"),
        ("audiochannels", "5.1"),
        ("language", "English/Japanese"),
        ("subs", "English"),
        ("runtime", "0:42:11"),
        ("hdr", "HDR10"),
        ("audiosamplerate", "48000"),
        ("audiobitdepth", "24"),
    ] {
        let expected = format!(r#"<torznab:attr name="{name}" value="{value}"/>"#);
        assert!(xml.contains(&expected), "missing {name}: {xml}");
    }
}

/// An unknown field is omitted, never published empty: a friend's quality
/// profile comparing against `""` is worse than one that skips the
/// comparison because the attribute is absent.
#[test]
fn unknown_media_fields_are_omitted_rather_than_empty() {
    let mut item = episode("Lanternwick.Hollow.S02E01", 2, 1);
    item.media = Some(sharerr_core::MediaMeta {
        resolution: Some("1280x720".to_owned()),
        ..sharerr_core::MediaMeta::default()
    });
    let xml = render(&item);

    assert!(xml.contains(r#"<torznab:attr name="resolution" value="1280x720"/>"#));
    for absent in [
        "video",
        "audio",
        "audiochannels",
        "subs",
        "runtime",
        "hdr",
        "audiosamplerate",
        "audiobitdepth",
    ] {
        assert!(
            !xml.contains(&format!(r#"name="{absent}""#)),
            "{absent} must not appear at all: {xml}"
        );
    }
}

/// An item with no metadata renders exactly as it did before the feature —
/// the attributes are additive, and a consumer that was working must not see
/// the feed change shape.
#[test]
fn an_item_without_media_gains_no_attributes() {
    let item = episode("Lanternwick.Hollow.S02E01", 2, 1);
    let xml = render(&item);
    for absent in ["resolution", "video", "audio", "subs", "runtime", "hdr"] {
        assert!(!xml.contains(&format!(r#"name="{absent}""#)), "{absent}");
    }
}

/// Metadata is a string from an *arr API or a container header, neither of
/// which sharerr controls, and it lands in XML.
#[test]
fn media_values_are_escaped() {
    let mut item = episode("Lanternwick.Hollow.S02E01", 2, 1);
    item.media = Some(sharerr_core::MediaMeta {
        video_codec: Some("a<b>&\"c\"".to_owned()),
        ..sharerr_core::MediaMeta::default()
    });
    let xml = render(&item);

    assert!(!xml.contains("a<b>"), "raw markup reached the feed: {xml}");
    assert!(xml.contains("a&lt;b&gt;"), "{xml}");
}

fn episode(title: &str, season: u32, ep: u32) -> SharedItem {
    SharedItem {
        id: Some(1),
        source: MediaSource::Sonarr,
        source_id: 7,
        file_id: 1,
        spec: MediaSpec::Episode {
            series_title: "Lanternwick Hollow".to_owned(),
            season,
            episode: ep,
        },
        release_title: title.to_owned(),
        arr_path: PathBuf::from("/tv/x.mkv"),
        size: 2_147_483_648,
        ids: ExternalIds {
            tvdb: Some(918_273),
            tmdb: None,
            tvmaze: Some(4242),
            imdb: Some("tt7654321".to_owned()),
            ..ExternalIds::default()
        },
        media: None,
        info_hash: Some("ab".repeat(20)),
        announce_token_fp: None,
        created_by_sharerr: true,
        state: ShareState::Seeding,
        last_error: None,
        created_at: None,
        achieved_ratio: None,
        ratio_limit_reported: None,
    }
}

fn movie(title: &str) -> SharedItem {
    SharedItem {
        spec: MediaSpec::Movie {
            title: "Harborlight".to_owned(),
            year: Some(2019),
        },
        release_title: title.to_owned(),
        ids: ExternalIds {
            tvdb: None,
            tmdb: Some(555),
            tvmaze: None,
            imdb: Some("tt1112223".to_owned()),
            ..ExternalIds::default()
        },
        media: None,
        ..episode(title, 1, 1)
    }
}

fn render(item: &SharedItem) -> String {
    feed_xml(&[FeedItem {
        item,
        download_url: "http://seed.example:8477/torrents/x.torrent".to_owned(),
        magnet_url: magnet_uri(
            item.info_hash.as_deref().unwrap_or_default(),
            &item.release_title,
            item.size,
            &["http://seed.example:8477/announce".to_owned()],
        ),
    }])
}

/// The trap this whole module is arranged around.
#[test]
fn the_title_is_the_release_title_not_the_filename() {
    let item = episode("Lanternwick.Hollow.S02E04.1080p.WEB-DL.x264-FAKEGRP", 2, 4);
    let xml = render(&item);

    assert!(
        xml.contains("<title>Lanternwick.Hollow.S02E04.1080p.WEB-DL.x264-FAKEGRP</title>"),
        "{xml}"
    );
    assert!(
        !xml.contains("<title>Lanternwick Hollow</title>"),
        "the series title is not a release title"
    );
}

#[test]
fn xml_metacharacters_in_a_title_cannot_break_the_document() {
    // Release titles really do contain ampersands, and a raw `&` makes the whole
    // feed unparseable at the far end rather than mangling one entry.
    let item = episode(r#"Rock & Roll <b>"Hi"</b> 'x' S01E01"#, 1, 1);
    let xml = render(&item);

    assert!(
        xml.contains("Rock &amp; Roll &lt;b&gt;&quot;Hi&quot;&lt;/b&gt; &apos;x&apos;"),
        "{xml}"
    );
    assert!(!xml.contains("<b>"), "raw markup leaked into the feed");
}

#[test]
fn control_characters_are_replaced_rather_than_emitted() {
    let item = episode("Bad\u{0007}Title", 1, 1);
    let xml = render(&item);
    assert!(
        !xml.contains('\u{0007}'),
        "a control char would make this unparseable"
    );
    assert!(xml.contains("Bad Title"), "{xml}");
}

#[test]
fn an_episode_carries_its_ids_season_and_episode() {
    let xml = render(&episode("X.S02E04", 2, 4));

    assert!(
        xml.contains(r#"<torznab:attr name="tvdbid" value="918273"/>"#),
        "{xml}"
    );
    assert!(
        xml.contains(r#"<torznab:attr name="imdbid" value="tt7654321"/>"#),
        "{xml}"
    );
    assert!(
        xml.contains(r#"<torznab:attr name="tvmazeid" value="4242"/>"#),
        "{xml}"
    );
    assert!(
        xml.contains(r#"<torznab:attr name="season" value="2"/>"#),
        "{xml}"
    );
    assert!(
        xml.contains(r#"<torznab:attr name="episode" value="4"/>"#),
        "{xml}"
    );
    assert!(xml.contains("<category>5000</category>"), "tv category");
    // Absent ids must be omitted, not rendered empty — Prowlarr treats an empty
    // value as a real id and matches nothing.
    assert!(!xml.contains(r#"name="tmdbid""#), "{xml}");
}

#[test]
fn a_movie_is_categorised_and_identified_as_one() {
    let xml = render(&movie("Harborlight.2019.1080p.BluRay-FAKEGRP"));
    assert!(xml.contains("<category>2000</category>"), "{xml}");
    assert!(xml.contains(r#"name="tmdbid" value="555""#), "{xml}");
    assert!(!xml.contains(r#"name="season""#), "a film has no season");
}

/// Sonarr and Radarr refuse an entire feed whose items have no `pubDate` —
/// "Each item in the RSS feed must have a pubDate element with a valid
/// publish date" — so a feed without one cannot be added as an indexer at
/// all. Not obvious from reading the document in isolation; only a real
/// client catches it.
#[test]
fn every_item_has_a_pubdate_or_sonarr_rejects_the_whole_feed() {
    let mut item = episode("Lanternwick.Hollow.S02E01", 2, 1);
    item.created_at = Some(1_700_000_000);
    let xml = render(&item);

    assert_eq!(
        xml.matches("<pubDate>").count(),
        1,
        "every item needs one: {xml}"
    );
    // RFC 2822, the only format RSS accepts.
    assert!(
        xml.contains("<pubDate>Tue, 14 Nov 2023 22:13:20 +0000</pubDate>"),
        "{xml}"
    );
}

/// An item with no stored timestamp still gets a valid date. A wrong-but-valid
/// one costs an ordering quirk; a missing one costs the whole feed.
#[test]
fn an_item_without_a_timestamp_still_gets_a_valid_pubdate() {
    let mut item = episode("Lanternwick.Hollow.S02E01", 2, 1);
    item.created_at = None;
    let xml = render(&item);

    assert!(
        xml.contains("<pubDate>Thu, 01 Jan 1970 00:00:00 +0000</pubDate>"),
        "{xml}"
    );
}

#[test]
fn the_enclosure_and_size_are_what_a_client_downloads() {
    let xml = render(&episode("X.S01E01", 1, 1));
    assert!(xml.contains("<size>2147483648</size>"), "{xml}");
    assert!(
            xml.contains(r#"<enclosure url="http://seed.example:8477/torrents/x.torrent" length="2147483648" type="application/x-bittorrent"/>"#),
            "{xml}"
        );
}

/// The `.torrent` download link carries the caller's own token — for
/// `crate::tracker::torrent_file` to rewrite the announce it serves back —
/// under exactly the same condition `magnet_url`'s tiers do: only when
/// there is a tracker token configured at all to attribute against. With
/// none set, the link is unchanged from what it has always been.
#[test]
fn the_download_url_carries_a_peers_token_only_when_one_was_collected() {
    let item = episode("X.S01E01", 1, 1);
    let hash = item.info_hash.clone().unwrap();

    let untokened = Matched {
        items: vec![],
        base: "http://seed.example:8477".to_owned(),
        announces_encoded: vec![],
        download_token: None,
        total: 0,
    };
    assert_eq!(
        untokened.download_url(&item),
        format!("http://seed.example:8477/torrents/{hash}.torrent"),
        "no token configured must leave the link untokened"
    );

    let tokened = Matched {
        download_token: Some("sams-key-hash".to_owned()),
        ..untokened
    };
    assert_eq!(
        tokened.download_url(&item),
        format!("http://seed.example:8477/torrents/{hash}.torrent?token=sams-key-hash")
    );
}

/// The bug this exists to catch: on a gluetun-only deployment (no static
/// `tracker.advertised_host`), the `.torrent` download link must track
/// the live resolved endpoint the magnet tiers already use via
/// `endpoint().recent()` — not fall back to `http://localhost:<port>`,
/// which only works from the box sharerr itself runs on.
#[tokio::test]
async fn the_download_base_tracks_the_live_endpoint_not_localhost() {
    let (_dir, state) = with_peer().await;
    state
        .endpoint()
        .observe(url::Url::parse("http://203.0.113.9:41234/").unwrap());

    let matched = collect(&state, &SearchQuery::default(), PeerScope::All, "sam-key")
        .await
        .unwrap();

    assert_eq!(matched.base, "http://203.0.113.9:41234");
}

/// The magnet is the whole release in one URI: identity, display name,
/// exact length, and the same announce tiers the `.torrent` carries — and
/// it must arrive XML-escaped, because `&` separates its every parameter.
#[test]
fn the_magnet_carries_identity_name_size_and_tracker() {
    let item = episode("Lanternwick.Hollow.S02E04.1080p.WEB-DL.x264-FAKEGRP", 2, 4);
    let magnet = magnet_uri(
        item.info_hash.as_deref().unwrap(),
        &item.release_title,
        item.size,
        // Encoded the way `collect` encodes them, once per response.
        &[encode_component("http://seed.example:8477/announce")],
    );

    assert!(
        magnet.starts_with(&format!(
            "magnet:?xt=urn:btih:{}",
            item.info_hash.as_deref().unwrap()
        )),
        "{magnet}"
    );
    assert!(
        magnet.contains("&dn=Lanternwick.Hollow.S02E04.1080p.WEB-DL.x264-FAKEGRP"),
        "{magnet}"
    );
    assert!(magnet.contains("&xl=2147483648"), "{magnet}");
    assert!(
        magnet.contains("&tr=http%3A%2F%2Fseed.example%3A8477%2Fannounce"),
        "the announce URL must be percent-encoded: {magnet}"
    );

    let xml = render(&item);
    assert!(
        xml.contains(r#"<torznab:attr name="magneturl" value="magnet:?xt=urn:btih:"#),
        "{xml}"
    );
    assert!(
        xml.contains("&amp;dn="),
        "the magnet's ampersands must be XML-escaped: {xml}"
    );
}

/// A rotated endpoint means multiple tiers, all of them in the magnet.
#[test]
fn the_magnet_spans_every_announce_tier() {
    let magnet = magnet_uri(
        "ab".repeat(20).as_str(),
        "X",
        0,
        &[
            encode_component("http://203.0.113.9:41234/announce"),
            encode_component("http://static.example:8477/announce"),
        ],
    );
    assert_eq!(magnet.matches("&tr=").count(), 2, "{magnet}");
    assert!(!magnet.contains("&xl="), "a zero size is not advertised");
}

#[test]
fn an_empty_feed_is_still_a_valid_document() {
    let xml = feed_xml(&[]);
    assert!(xml.starts_with("<?xml version=\"1.0\""));
    assert!(xml.ends_with("</rss>"));
    assert!(!xml.contains("<item>"));
}

#[test]
fn caps_advertises_the_id_searches_that_make_matching_reliable() {
    let caps = caps_xml();
    assert!(
        caps.contains(
            r#"<tv-search available="yes" supportedParams="q,season,ep,tvdbid,imdbid"/>"#
        )
    );
    assert!(caps.contains(r#"name="Movies""#));
    assert!(caps.ends_with("</caps>"));
}

/// Advertising a function the dispatcher refuses is exactly the drift that
/// answers a friend's Lidarr `t=music` with "no such function" while caps
/// claims `music-search`. Clients derive `t=` from the caps element name,
/// both with and without the dash, so every entry must accept both.
#[test]
fn every_advertised_search_function_is_dispatched() {
    for (element, aliases, _) in SEARCH_FUNCTIONS {
        assert!(
            is_search_function(element) || *element == "search",
            "caps advertises <{element}> but the dispatcher refuses t={element}"
        );
        assert!(
            is_search_function(&element.replace('-', "")),
            "the dashless t={} must be accepted",
            element.replace('-', "")
        );
        for alias in *aliases {
            assert!(is_search_function(alias), "alias t={alias} is refused");
        }
    }
}

#[test]
fn an_empty_query_returns_everything() {
    let query = SearchQuery::default();
    assert!(query.matches(&episode("a", 1, 1)));
    assert!(query.matches(&movie("b")));
}

#[test]
fn season_and_episode_filters_narrow_to_one_release() {
    let item = episode("X.S02E04", 2, 4);

    let hit = SearchQuery {
        season: Some(2),
        ep: Some(4),
        ..Default::default()
    };
    assert!(hit.matches(&item));

    let wrong_ep = SearchQuery {
        season: Some(2),
        ep: Some(5),
        ..Default::default()
    };
    assert!(!wrong_ep.matches(&item));

    // A film can never satisfy an episode filter.
    assert!(!hit.matches(&movie("m")));
}

#[test]
fn id_searches_match_and_reject() {
    let item = episode("X.S01E01", 1, 1);

    assert!(
        SearchQuery {
            tvdbid: Some(918_273),
            ..Default::default()
        }
        .matches(&item)
    );
    assert!(
        !SearchQuery {
            tvdbid: Some(1),
            ..Default::default()
        }
        .matches(&item)
    );
    // Radarr's tmdbid against a Sonarr item: no tmdb id stored, so no match.
    assert!(
        !SearchQuery {
            tmdbid: Some(555),
            ..Default::default()
        }
        .matches(&item)
    );
}

#[test]
fn imdb_ids_match_with_or_without_the_tt_prefix() {
    // Sonarr sends the bare number, Radarr sends `tt`-prefixed. Comparing them
    // literally makes an id search silently return nothing.
    let item = episode("X.S01E01", 1, 1);
    assert!(
        SearchQuery {
            imdbid: Some("tt7654321".to_owned()),
            ..Default::default()
        }
        .matches(&item)
    );
    assert!(
        SearchQuery {
            imdbid: Some("7654321".to_owned()),
            ..Default::default()
        }
        .matches(&item)
    );
    assert!(
        !SearchQuery {
            imdbid: Some("0000000".to_owned()),
            ..Default::default()
        }
        .matches(&item)
    );
}

#[test]
fn free_text_search_looks_at_both_the_release_and_the_series() {
    let item = episode("Lanternwick.Hollow.S02E04.1080p-FAKEGRP", 2, 4);

    for needle in ["lanternwick", "FAKEGRP", "hollow s02"] {
        let query = SearchQuery {
            q: Some(needle.to_owned()),
            ..Default::default()
        };
        assert_eq!(
            query.matches(&item),
            needle != "hollow s02",
            "unexpected result for {needle:?}"
        );
    }

    // The series title matches even though it is not in the release string
    // verbatim in that casing.
    assert!(
        SearchQuery {
            q: Some("Lanternwick Hollow".to_owned()),
            ..Default::default()
        }
        .matches(&item)
    );
}

#[test]
fn a_blank_query_string_is_not_a_filter() {
    let query = SearchQuery {
        q: Some("   ".to_owned()),
        ..Default::default()
    };
    assert!(query.matches(&episode("anything", 1, 1)));
}

#[test]
fn errors_are_torznab_shaped_and_escaped() {
    let xml = error_xml(100, r#"bad <key> & "stuff""#);
    assert!(xml.contains(r#"code="100""#));
    assert!(xml.contains("&lt;key&gt; &amp; &quot;stuff&quot;"), "{xml}");
}

// ---------------------------------------------------------------- peer auth

use crate::state::fixtures::unconfigured;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use secrecy::SecretString;
use tower::ServiceExt;

/// Ask the real router, so the answer covers routing and extraction too.
async fn caps_with_key(state: &std::sync::Arc<ServeState>, key: Option<&str>) -> StatusCode {
    let uri = match key {
        Some(key) => format!("/api?t=caps&apikey={key}"),
        None => "/api?t=caps".to_owned(),
    };
    routes(std::sync::Arc::clone(state))
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

/// The point of M4: a friend's own key opens the feed.
#[tokio::test]
async fn a_peers_key_authenticates_the_feed() {
    let (_dir, state) = unconfigured();
    let store = state.store().await.unwrap();
    store
        .create_peer("Sam", &SecretString::from("sam-key"), PeerScope::All)
        .await
        .unwrap();

    assert_eq!(caps_with_key(&state, Some("sam-key")).await, StatusCode::OK);
}

/// And the other half of the point: revoking one friend cuts off exactly that
/// friend.
#[tokio::test]
async fn revoking_one_peer_closes_the_feed_only_for_them() {
    let (_dir, state) = unconfigured();
    let store = state.store().await.unwrap();
    let sam = store
        .create_peer("Sam", &SecretString::from("sam-key"), PeerScope::All)
        .await
        .unwrap();
    store
        .create_peer("Alex", &SecretString::from("alex-key"), PeerScope::All)
        .await
        .unwrap();

    store.revoke_peer(sam.id).await.unwrap();

    assert_eq!(
        caps_with_key(&state, Some("sam-key")).await,
        StatusCode::UNAUTHORIZED,
        "a revoked key must stop working"
    );
    assert_eq!(
        caps_with_key(&state, Some("alex-key")).await,
        StatusCode::OK,
        "revoking Sam must not affect Alex"
    );
}

/// A key nobody was issued, and no key at all, are refused the same way — and
/// neither gets a message confirming what this port is.
#[tokio::test]
async fn an_unknown_or_absent_key_is_refused() {
    let (_dir, state) = unconfigured();
    state.store().await.unwrap();

    assert_eq!(
        caps_with_key(&state, Some("guessed")).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(caps_with_key(&state, None).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        caps_with_key(&state, Some("")).await,
        StatusCode::UNAUTHORIZED,
        "an empty key must not be treated as absent-and-therefore-fine"
    );
}

/// Using the feed is what proves a friend is actually set up, so it has to be
/// recorded — that column is the whole answer to "did Sam get it working?".
#[tokio::test]
async fn a_successful_request_records_that_the_peer_was_seen() {
    let (_dir, state) = unconfigured();
    let store = state.store().await.unwrap();
    store
        .create_peer("Sam", &SecretString::from("sam-key"), PeerScope::All)
        .await
        .unwrap();

    assert_eq!(
        store.list_peers().await.unwrap()[0].last_seen_at,
        None,
        "nobody has used the key yet"
    );

    assert_eq!(caps_with_key(&state, Some("sam-key")).await, StatusCode::OK);

    assert!(
        store.list_peers().await.unwrap()[0].last_seen_at.is_some(),
        "an authenticated request must record the peer as seen"
    );
}

// ------------------------------------------------------------- jackett shape

async fn get(state: &std::sync::Arc<ServeState>, uri: &str) -> StatusCode {
    routes(std::sync::Arc::clone(state))
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

async fn body(state: &std::sync::Arc<ServeState>, uri: &str) -> String {
    let response = routes(std::sync::Arc::clone(state))
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

async fn with_peer() -> (tempfile::TempDir, std::sync::Arc<ServeState>) {
    let (dir, state) = unconfigured();
    state
        .store()
        .await
        .unwrap()
        .create_peer("Sam", &SecretString::from("sam-key"), PeerScope::All)
        .await
        .unwrap();
    (dir, state)
}

/// The three shapes a Jackett-configured client actually requests. All of them
/// have to work, because which one you get depends on whether the client
/// appends `/api` to a base URL that may or may not already end in a slash.
#[tokio::test]
async fn the_jackett_paths_serve_the_same_feed() {
    let (_dir, state) = with_peer().await;

    for uri in [
        "/api/v2.0/indexers/sharerr/results/torznab/api?t=caps&apikey=sam-key",
        "/api/v2.0/indexers/sharerr/results/torznab/?t=caps&apikey=sam-key",
        "/api/v2.0/indexers/sharerr/results/torznab?t=caps&apikey=sam-key",
    ] {
        assert_eq!(get(&state, uri).await, StatusCode::OK, "{uri}");
    }
}

/// Jackett proxies many trackers and names each one in the path. sharerr is the
/// only thing it serves, so any id — including Jackett's `all` aggregate, and
/// whatever id someone had in their old config — means this feed.
#[tokio::test]
async fn any_indexer_id_reaches_the_same_feed() {
    let (_dir, state) = with_peer().await;

    for id in ["sharerr", "all", "some-old-jackett-id"] {
        let uri = format!("/api/v2.0/indexers/{id}/results/torznab/api?t=caps&apikey=sam-key");
        assert_eq!(get(&state, &uri).await, StatusCode::OK, "{uri}");
    }
}

/// Byte-identical to `/api`, or the two paths would drift into describing
/// different capabilities to different clients.
#[tokio::test]
async fn the_jackett_path_returns_the_same_document_as_the_plain_one() {
    let (_dir, state) = with_peer().await;

    let plain = body(&state, "/api?t=caps&apikey=sam-key").await;
    let jackett = body(
        &state,
        "/api/v2.0/indexers/sharerr/results/torznab/api?t=caps&apikey=sam-key",
    )
    .await;

    assert_eq!(plain, jackett);
    assert!(plain.contains("<caps>"), "{plain}");
}

/// The Jackett path must not be a way around authentication.
#[tokio::test]
async fn the_jackett_path_is_authenticated_too() {
    let (_dir, state) = with_peer().await;

    assert_eq!(
        get(
            &state,
            "/api/v2.0/indexers/sharerr/results/torznab/api?t=caps"
        )
        .await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get(
            &state,
            "/api/v2.0/indexers/sharerr/results/torznab/api?t=caps&apikey=wrong"
        )
        .await,
        StatusCode::UNAUTHORIZED
    );
}

// ------------------------------------------------------------ scoped feeds

/// Seed one TV item and one film, so scoping has something to distinguish.
async fn with_both_kinds() -> (tempfile::TempDir, std::sync::Arc<ServeState>) {
    use sharerr_core::model::ShareState;

    let (dir, state) = unconfigured();
    let store = state.store().await.unwrap();

    for (source, file_id, hash) in [
        (MediaSource::Sonarr, 1_i64, "aa"),
        (MediaSource::Radarr, 2_i64, "bb"),
    ] {
        let mut item = episode("Something.S01E01", 1, 1);
        item.source = source;
        item.file_id = file_id;
        item.info_hash = None;
        item.state = ShareState::Pending;
        store.upsert(&item).await.unwrap();
        store
            .set_info_hash(source, file_id, &hash.repeat(20))
            .await
            .unwrap();
        store
            .set_state(source, file_id, ShareState::Seeding, None)
            .await
            .unwrap();
    }
    (dir, state)
}

async fn feed_for(state: &std::sync::Arc<ServeState>, key: &str) -> String {
    let response = routes(std::sync::Arc::clone(state))
        .oneshot(
            Request::builder()
                .uri(format!("/api?t=search&apikey={key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The feature: two friends, two different libraries, one instance.
/// The assembled feed carries a magnet per item — the attribute a client
/// that prefers magnets looks for, next to the `.torrent` enclosure.
#[tokio::test]
async fn the_feed_offers_a_magnet_alongside_the_torrent() {
    let (_dir, state) = with_both_kinds().await;
    let store = state.store().await.unwrap();
    store
        .create_peer("Sam", &SecretString::from("sam-key"), PeerScope::All)
        .await
        .unwrap();

    let feed = feed_for(&state, "sam-key").await;
    assert_eq!(
        feed.matches("magneturl").count(),
        2,
        "every item gets one: {feed}"
    );
    assert!(
        feed.contains("magnet:?xt=urn:btih:aaaaaaaaaa"),
        "the magnet must carry the item's own hash: {feed}"
    );
}

#[tokio::test]
async fn each_friend_sees_only_what_their_scope_allows() {
    let (_dir, state) = with_both_kinds().await;
    let store = state.store().await.unwrap();
    store
        .create_peer("Tv", &SecretString::from("tv-key"), PeerScope::Tv)
        .await
        .unwrap();
    store
        .create_peer("Films", &SecretString::from("film-key"), PeerScope::Movies)
        .await
        .unwrap();
    store
        .create_peer("Both", &SecretString::from("both-key"), PeerScope::All)
        .await
        .unwrap();

    assert_eq!(
        feed_for(&state, "tv-key").await.matches("<item>").count(),
        1
    );
    assert_eq!(
        feed_for(&state, "film-key").await.matches("<item>").count(),
        1
    );
    assert_eq!(
        feed_for(&state, "both-key").await.matches("<item>").count(),
        2,
        "an unscoped friend still sees everything"
    );
}

/// Directory items reach the feed like any other item — categorised by
/// their spec, since the source carries no media kind — and a narrow scope
/// admits them by their declared kind rather than by source.
#[tokio::test]
async fn directory_items_reach_the_feed_and_honour_scope() {
    let (_dir, state) = unconfigured();
    let store = state.store().await.unwrap();

    let seed = [
        (
            41_i64,
            "cc",
            episode("Lanternwick.Hollow.S02E01.WEB-DL.x264-SHARERR", 2, 1),
        ),
        (42_i64, "dd", movie("Harborlight.2019.WEB-DL.x264-SHARERR")),
    ];
    for (file_id, hash, mut item) in seed {
        item.source = MediaSource::Directory;
        item.file_id = file_id;
        // What the scanner actually produces: no ids, nothing seeding yet.
        item.ids = ExternalIds::default();
        item.info_hash = None;
        item.state = ShareState::Pending;
        store.upsert(&item).await.unwrap();
        store
            .set_info_hash(MediaSource::Directory, file_id, &hash.repeat(20))
            .await
            .unwrap();
        store
            .set_state(MediaSource::Directory, file_id, ShareState::Seeding, None)
            .await
            .unwrap();
    }

    store
        .create_peer("Tv", &SecretString::from("tv-key"), PeerScope::Tv)
        .await
        .unwrap();
    store
        .create_peer("All", &SecretString::from("all-key"), PeerScope::All)
        .await
        .unwrap();

    let everything = feed_for(&state, "all-key").await;
    assert_eq!(everything.matches("<item>").count(), 2, "{everything}");
    assert!(
        everything.contains(&CAT_TV.to_string()) && everything.contains(&CAT_MOVIES.to_string()),
        "the categories must come from each item's spec: {everything}"
    );

    let tv = feed_for(&state, "tv-key").await;
    assert_eq!(tv.matches("<item>").count(), 1, "{tv}");
    assert!(tv.contains("Lanternwick"), "{tv}");
    assert!(
        !tv.contains("Harborlight"),
        "a tv-scoped friend must not see a directory movie: {tv}"
    );
}

/// Scope is decided by *who is asking*, not by the query — so a friend cannot
/// widen it by searching the other category.
#[tokio::test]
async fn a_scoped_friend_cannot_search_their_way_out_of_it() {
    let (_dir, state) = with_both_kinds().await;
    state
        .store()
        .await
        .unwrap()
        .create_peer("Tv", &SecretString::from("tv-key"), PeerScope::Tv)
        .await
        .unwrap();

    // Asking explicitly for movies must still return only what TV scope allows.
    let response = routes(std::sync::Arc::clone(&state))
        .oneshot(
            Request::builder()
                .uri("/api?t=movie-search&apikey=tv-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let xml = String::from_utf8_lossy(&bytes);

    assert!(
        !xml.contains(&"bb".repeat(20)),
        "a TV-scoped friend was served a film: {xml}"
    );
}

/// Changing the scope takes effect on the next request — an operator who
/// narrows a friend expects that to be true immediately, not after a restart.
#[tokio::test]
async fn narrowing_a_scope_takes_effect_at_once() {
    let (_dir, state) = with_both_kinds().await;
    let store = state.store().await.unwrap();
    let peer = store
        .create_peer("Sam", &SecretString::from("sam-key"), PeerScope::All)
        .await
        .unwrap();

    assert_eq!(
        feed_for(&state, "sam-key").await.matches("<item>").count(),
        2
    );

    store
        .set_peer_scope(peer.id, PeerScope::Movies)
        .await
        .unwrap();

    assert_eq!(
        feed_for(&state, "sam-key").await.matches("<item>").count(),
        1,
        "the narrowed scope must apply to the very next request"
    );
}

// ------------------------------------------- search filters over /api itself
//
// `matches_with` is exhaustively unit-tested as a pure function above, but
// that never proves axum's `Query<SearchQuery>` extractor actually parses
// `season`, `ep`, and `imdbid` off a real URL and threads them through to a
// real search — only the Jackett-shaped paths were ever asked this. These
// hit the plain `/api` route Prowlarr and a direct Sonarr/Radarr use.

async fn xml_body(state: &std::sync::Arc<ServeState>, uri: &str) -> String {
    let response = routes(std::sync::Arc::clone(state))
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[tokio::test]
async fn tvsearch_filters_by_season_and_episode_through_the_router() {
    let (_dir, state) = with_peer().await;
    let store = state.store().await.unwrap();

    for (file_id, hash, season, ep) in [(1_i64, "aa", 1, 1), (2_i64, "bb", 1, 2)] {
        let mut item = episode("Lanternwick.Hollow.SXXEXX.WEB-DL.x264-SHARERR", season, ep);
        item.file_id = file_id;
        item.info_hash = None;
        item.state = ShareState::Pending;
        store.upsert(&item).await.unwrap();
        store
            .set_info_hash(MediaSource::Sonarr, file_id, &hash.repeat(20))
            .await
            .unwrap();
        store
            .set_state(MediaSource::Sonarr, file_id, ShareState::Seeding, None)
            .await
            .unwrap();
    }

    let xml = xml_body(&state, "/api?t=tvsearch&season=1&ep=2&apikey=sam-key").await;
    assert!(xml.contains(&"bb".repeat(20)), "{xml}");
    assert!(!xml.contains(&"aa".repeat(20)), "{xml}");
}

#[tokio::test]
async fn moviesearch_filters_by_imdbid_through_the_router() {
    let (_dir, state) = with_peer().await;
    let store = state.store().await.unwrap();

    for (file_id, hash, title, imdb) in [
        (
            1_i64,
            "aa",
            "Harborlight.2019.WEB-DL.x264-SHARERR",
            "tt1112223",
        ),
        (
            2_i64,
            "bb",
            "Otherfilm.2020.WEB-DL.x264-SHARERR",
            "tt9998887",
        ),
    ] {
        let mut item = movie(title);
        item.source = MediaSource::Radarr;
        item.file_id = file_id;
        item.ids.imdb = Some(imdb.to_owned());
        item.info_hash = None;
        item.state = ShareState::Pending;
        store.upsert(&item).await.unwrap();
        store
            .set_info_hash(MediaSource::Radarr, file_id, &hash.repeat(20))
            .await
            .unwrap();
        store
            .set_state(MediaSource::Radarr, file_id, ShareState::Seeding, None)
            .await
            .unwrap();
    }

    let xml = xml_body(
        &state,
        "/api?t=movie-search&imdbid=tt9998887&apikey=sam-key",
    )
    .await;
    assert!(xml.contains(&"bb".repeat(20)), "{xml}");
    assert!(!xml.contains(&"aa".repeat(20)), "{xml}");
}

/// The plain text query, not just the structured filters — the shape a
/// client falls back to when it has no id for the release at all.
#[tokio::test]
async fn a_text_query_filters_through_the_router_too() {
    let (_dir, state) = with_peer().await;
    let store = state.store().await.unwrap();

    for (file_id, hash, title, series_title) in [
        (
            1_i64,
            "aa",
            "Lanternwick.Hollow.S01E01.WEB-DL.x264-SHARERR",
            "Lanternwick Hollow",
        ),
        (
            2_i64,
            "bb",
            "Otherfilm.S01E01.WEB-DL.x264-SHARERR",
            "Otherfilm",
        ),
    ] {
        let mut item = episode(title, 1, 1);
        item.file_id = file_id;
        item.spec = MediaSpec::Episode {
            series_title: series_title.to_owned(),
            season: 1,
            episode: 1,
        };
        item.info_hash = None;
        item.state = ShareState::Pending;
        store.upsert(&item).await.unwrap();
        store
            .set_info_hash(MediaSource::Sonarr, file_id, &hash.repeat(20))
            .await
            .unwrap();
        store
            .set_state(MediaSource::Sonarr, file_id, ShareState::Seeding, None)
            .await
            .unwrap();
    }

    let xml = xml_body(&state, "/api?t=search&q=Lanternwick&apikey=sam-key").await;
    assert!(xml.contains(&"aa".repeat(20)), "{xml}");
    assert!(!xml.contains(&"bb".repeat(20)), "{xml}");
}

// ------------------------------------------------------ the long tail

#[test]
fn category_follows_the_source_first_and_the_spec_second() {
    let mut adult = episode("Lanternwick", 1, 1);
    adult.source = MediaSource::Whisparr;
    assert_eq!(category_for(&adult), CAT_XXX);

    let mut track = movie("Lanterns");
    track.source = MediaSource::Lidarr;
    track.spec = MediaSpec::Track {
        artist: "Quiet Harbour".to_owned(),
        album: "Lanterns".to_owned(),
        track: Some(1),
    };
    assert_eq!(category_for(&track), CAT_AUDIO);

    let mut book = movie("The Copper Vale");
    book.source = MediaSource::Readarr;
    book.spec = MediaSpec::Book {
        author: "Mara Vell".to_owned(),
        title: "The Copper Vale".to_owned(),
    };
    assert_eq!(category_for(&book), CAT_BOOKS);
}

#[test]
fn an_episode_filter_that_matches_the_season_but_not_the_episode_excludes() {
    let query = SearchQuery {
        t: "tvsearch".to_owned(),
        season: Some(1),
        ep: Some(2),
        ..Default::default()
    };
    assert!(!query.matches(&episode("Lanternwick", 1, 1)));
    assert!(query.matches(&episode("Lanternwick", 1, 2)));
}

#[test]
fn contains_ci_falls_back_to_unicode_lowercasing_for_non_ascii_text() {
    assert!(contains_ci("Lanternwick Höllow", "höllow"));
    assert!(!contains_ci("Lanternwick Höllow", "hollow"));
}

#[tokio::test]
async fn an_unknown_function_is_a_torznab_error_not_a_404() {
    let (_dir, state) = with_peer().await;
    let text = body(&state, "/api?t=bogus&apikey=sam-key").await;
    assert!(text.contains("no such function: bogus"), "{text}");
    assert_eq!(
        get(&state, "/api?t=bogus&apikey=sam-key").await,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn collect_reports_an_unready_store_and_a_release_without_a_torrent_has_no_magnet() {
    let (_dir, state) = crate::state::fixtures::store_unopenable();
    let Err(err) = collect(&state, &SearchQuery::default(), PeerScope::All, "fp").await else {
        panic!("a store that will not open cannot answer a search");
    };
    assert_eq!(err.0, StatusCode::SERVICE_UNAVAILABLE);
    assert!(err.1.contains("not ready"), "{}", err.1);

    let (_dir, state) = unconfigured();
    let store = state.store().await.unwrap();
    let mut item = movie("Harborlight");
    item.info_hash = Some("ab".repeat(20));
    item.state = ShareState::Seeding;
    store.upsert(&item).await.unwrap();
    let matched = collect(&state, &SearchQuery::default(), PeerScope::All, "fp")
        .await
        .unwrap();
    assert_eq!(matched.items.len(), 1);
    assert!(matched.magnet_url(&matched.items[0]).starts_with("magnet:"));
    // A release the store lists but whose torrent is not built yet has
    // nothing a magnet could name.
    item.info_hash = None;
    assert_eq!(matched.magnet_url(&item), "");
}
