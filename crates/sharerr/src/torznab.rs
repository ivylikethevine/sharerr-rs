//! The Torznab indexer endpoint — how a friend's Prowlarr finds what we share.
//!
//! Torznab is Newznab's torrent dialect: an RSS 2.0 feed with a `torznab:` element
//! namespace, queried over `GET /api?t=...&apikey=...`. Prowlarr's "Generic
//! Torznab" indexer speaks it, and Sonarr/Radarr reach it through Prowlarr.
//!
//! # The one thing that must not be got wrong
//!
//! `<title>` is the **release title**, never the torrent's `info.name`. They are
//! different strings on purpose: `info.name` is the on-disk filename a client
//! looks for inside the save path, while the release title is the scene-style name
//! the friend's Sonarr parses to decide what a release contains. Publishing the
//! filename would leave the far end unable to match the release to anything; the
//! store keeps them apart, and so does this.
//!
//! # Why the XML is written by hand
//!
//! The document is a fixed shape with perhaps a dozen distinct elements. A
//! serializer would add a dependency and a derive layer over markup that is easier
//! to read literally — provided escaping is taken seriously, which is why every
//! interpolation goes through [`escape`] and the tests attack it directly.

use std::fmt::Write as _;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use sharerr_core::config::secret_keys;
use sharerr_core::model::{MediaSpec, SharedItem};

use crate::state::ServeState;

/// Newznab category numbers. Sonarr and Radarr filter on these, and a release in
/// the wrong one is invisible to the app that wants it.
const CAT_TV: u32 = 5000;
const CAT_MOVIES: u32 = 2000;

/// Torznab's own error format. Prowlarr surfaces the description verbatim, which
/// makes it the only place a misconfiguration can be explained to the person
/// holding the other end.
fn error_xml(code: u32, description: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<error code="{code}" description="{}"/>"#,
        escape(description)
    )
}

/// Escape text for an XML text node or a double-quoted attribute value.
///
/// All five predefined entities, not the usual three. Release titles routinely
/// contain `&` and `'`, and attribute values here are always double-quoted — but
/// escaping `'` as well costs nothing and removes a footgun if one ever moves.
fn escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 16);
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Control characters are not legal in XML 1.0 at all, and a stray one
            // from a filename would make the whole document unparseable at the far
            // end rather than merely mangling one title.
            c if (c as u32) < 0x20 && c != '\t' && c != '\n' && c != '\r' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// What the indexer says it can do.
///
/// Prowlarr fetches this once and uses it to decide which searches to send. The
/// supported-params lists are the load-bearing part: claiming `tvdbid` here is
/// what makes Sonarr search by id instead of by free text, which is the difference
/// between a reliable match and a fuzzy one.
pub fn caps_xml() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<caps>
  <server title="sharerr"/>
  <limits max="500" default="100"/>
  <searching>
    <search available="yes" supportedParams="q"/>
    <tv-search available="yes" supportedParams="q,season,ep,tvdbid,imdbid"/>
    <movie-search available="yes" supportedParams="q,imdbid,tmdbid"/>
    <music-search available="no" supportedParams="q"/>
    <audio-search available="no" supportedParams="q"/>
    <book-search available="no" supportedParams="q"/>
  </searching>
  <categories>
    <category id="{CAT_MOVIES}" name="Movies"/>
    <category id="{CAT_TV}" name="TV"/>
  </categories>
</caps>"#
    )
}

/// One release, as the feed publishes it.
#[derive(Debug, Clone)]
pub struct FeedItem<'a> {
    pub item: &'a SharedItem,
    /// Absolute URL the friend's client fetches the `.torrent` from.
    pub download_url: String,
    pub seeders: usize,
    pub leechers: usize,
}

/// Render the RSS feed.
pub fn feed_xml(items: &[FeedItem<'_>]) -> String {
    let mut out = String::with_capacity(512 + items.len() * 512);
    out.push_str(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:atom="http://www.w3.org/2005/Atom" xmlns:torznab="http://torznab.com/schemas/2015/feed">
  <channel>
    <title>sharerr</title>
    <description>Content shared by a friend running sharerr</description>
"#,
    );

    for entry in items {
        let item = entry.item;
        let category = match item.spec {
            MediaSpec::Episode { .. } => CAT_TV,
            MediaSpec::Movie { .. } => CAT_MOVIES,
        };
        // Unwrapped safely: `seeding_items` guarantees an info hash, and an item
        // without one is filtered out before it reaches here.
        let info_hash = item.info_hash.as_deref().unwrap_or_default();

        let _ = write!(
            out,
            r#"    <item>
      <title>{title}</title>
      <guid isPermaLink="false">{hash}</guid>
      <link>{link}</link>
      <size>{size}</size>
      <category>{category}</category>
      <enclosure url="{link}" length="{size}" type="application/x-bittorrent"/>
      <torznab:attr name="category" value="{category}"/>
      <torznab:attr name="seeders" value="{seeders}"/>
      <torznab:attr name="peers" value="{peers}"/>
      <torznab:attr name="infohash" value="{hash}"/>
      <torznab:attr name="downloadvolumefactor" value="0"/>
      <torznab:attr name="uploadvolumefactor" value="1"/>
"#,
            // The release title, never `info.name`. See the module header.
            title = escape(&item.release_title),
            hash = escape(info_hash),
            link = escape(&entry.download_url),
            size = item.size,
            seeders = entry.seeders,
            // Torznab's "peers" is the total, not the leecher count.
            peers = entry.seeders + entry.leechers,
        );

        // The ids are what let the far end match a release to a known series or
        // film rather than parsing the title and hoping.
        for (name, value) in [
            ("tvdbid", item.ids.tvdb.map(|v| v.to_string())),
            ("tmdbid", item.ids.tmdb.map(|v| v.to_string())),
            ("tvmazeid", item.ids.tvmaze.map(|v| v.to_string())),
            ("imdbid", item.ids.imdb.clone()),
        ] {
            if let Some(value) = value {
                let _ = writeln!(
                    out,
                    "      <torznab:attr name=\"{name}\" value=\"{}\"/>",
                    escape(&value)
                );
            }
        }

        if let MediaSpec::Episode {
            season, episode, ..
        } = item.spec
        {
            let _ = write!(
                out,
                "      <torznab:attr name=\"season\" value=\"{season}\"/>\n      <torznab:attr name=\"episode\" value=\"{episode}\"/>\n"
            );
        }

        out.push_str("    </item>\n");
    }

    out.push_str("  </channel>\n</rss>");
    out
}

// ---------------------------------------------------------------------------
// Query matching
// ---------------------------------------------------------------------------

/// The subset of Torznab's query parameters sharerr honours.
#[derive(Debug, Default, Deserialize)]
pub struct SearchQuery {
    #[serde(default)]
    pub t: String,
    pub apikey: Option<String>,
    pub q: Option<String>,
    pub season: Option<u32>,
    pub ep: Option<u32>,
    pub tvdbid: Option<i64>,
    pub tmdbid: Option<i64>,
    pub imdbid: Option<String>,
}

impl SearchQuery {
    /// Whether an item satisfies every constraint the query set.
    ///
    /// Filters are ANDed and an absent filter matches everything, which is what
    /// makes a bare `t=tvsearch` return the whole library — the behaviour Prowlarr
    /// relies on for its "test" button and for RSS sync.
    pub fn matches(&self, item: &SharedItem) -> bool {
        if let Some(tvdbid) = self.tvdbid
            && item.ids.tvdb != Some(tvdbid)
        {
            return false;
        }
        if let Some(tmdbid) = self.tmdbid
            && item.ids.tmdb != Some(tmdbid)
        {
            return false;
        }
        if let Some(imdbid) = &self.imdbid
            && !imdb_matches(item.ids.imdb.as_deref(), imdbid)
        {
            return false;
        }

        match item.spec {
            MediaSpec::Episode {
                season, episode, ..
            } => {
                if self.season.is_some_and(|want| want != season) {
                    return false;
                }
                if self.ep.is_some_and(|want| want != episode) {
                    return false;
                }
            }
            MediaSpec::Movie { .. } => {
                // A season or episode filter cannot be satisfied by a film.
                if self.season.is_some() || self.ep.is_some() {
                    return false;
                }
            }
        }

        match &self.q {
            None => true,
            Some(q) => {
                let needle = q.trim().to_lowercase();
                needle.is_empty()
                    || item.release_title.to_lowercase().contains(&needle)
                    || item.spec.title().to_lowercase().contains(&needle)
            }
        }
    }
}

/// Compare IMDb ids tolerantly.
///
/// Sonarr sends `1234567`, Radarr sends `tt1234567`, and the *arr APIs return
/// either depending on the endpoint. Comparing them literally means an id search
/// silently matches nothing, which looks exactly like "the friend has none of this".
fn imdb_matches(stored: Option<&str>, wanted: &str) -> bool {
    let normalise = |raw: &str| raw.trim().trim_start_matches("tt").to_owned();
    stored.is_some_and(|stored| normalise(stored) == normalise(wanted))
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

pub fn routes(serve: Arc<ServeState>) -> axum::Router {
    axum::Router::new()
        .route("/api", axum::routing::get(api))
        .with_state(serve)
}

/// `GET /api?t=...`
pub async fn api(
    State(state): State<Arc<ServeState>>,
    Query(query): Query<SearchQuery>,
) -> Response {
    if let Err(response) = check_api_key(&state, query.apikey.as_deref()).await {
        return response;
    }

    match query.t.as_str() {
        // `caps` is fetched before the key is ever configured in Prowlarr, but
        // requiring the key here too is deliberate: it is one fewer endpoint that
        // says anything at all to an unauthenticated caller.
        "caps" => xml(caps_xml()),
        "search" | "tvsearch" | "tv-search" | "movie" | "moviesearch" | "movie-search" => {
            search(&state, &query).await
        }
        other => xml_status(
            StatusCode::BAD_REQUEST,
            error_xml(202, &format!("no such function: {other}")),
        ),
    }
}

async fn search(state: &ServeState, query: &SearchQuery) -> Response {
    let store = match state.store().await {
        Ok(store) => store,
        Err(reason) => {
            return xml_status(
                StatusCode::SERVICE_UNAVAILABLE,
                error_xml(900, &format!("sharerr is not ready: {reason}")),
            );
        }
    };

    let items = match store.seeding_items().await {
        Ok(items) => items,
        Err(err) => {
            tracing::error!(error = %err, "torznab search failed");
            return xml_status(
                StatusCode::INTERNAL_SERVER_ERROR,
                error_xml(900, "could not read the library"),
            );
        }
    };

    let config = state.config().await;
    let base = public_base_url(&config);

    let matched: Vec<_> = items.iter().filter(|item| query.matches(item)).collect();
    let entries: Vec<FeedItem<'_>> = matched
        .iter()
        .map(|item| FeedItem {
            item,
            download_url: format!(
                "{base}/torrents/{}.torrent",
                item.info_hash.as_deref().unwrap_or_default()
            ),
            // Reported as one seeder — this instance — rather than from the swarm.
            // Prowlarr drops releases with zero seeders, and the tracker legitimately
            // knows of none until a peer announces, so a truthful zero would hide
            // every release that nobody has downloaded yet.
            seeders: 1,
            leechers: 0,
        })
        .collect();

    tracing::debug!(
        function = %query.t,
        returned = entries.len(),
        of = items.len(),
        "torznab search"
    );
    xml(feed_xml(&entries))
}

/// The URL a friend reaches this instance on.
///
/// Built from `tracker.advertised_host`, which is the only address sharerr is told
/// about that is known to work from outside. The bind address is deliberately not
/// consulted: it is usually `0.0.0.0`, which is not a URL anyone can fetch.
pub fn public_base_url(config: &sharerr_core::Config) -> String {
    let host = config
        .tracker
        .advertised_host
        .as_deref()
        .unwrap_or("localhost");
    let port = config
        .tracker
        .port
        .unwrap_or_else(|| config.server.bind.port());
    format!("http://{host}:{port}")
}

/// Torznab authenticates with an `apikey` query parameter.
///
/// Absent from the vault means the endpoint is closed rather than open: an
/// indexer feed lists everything this instance shares, and defaulting to
/// unauthenticated would publish the library to anyone who found the port.
async fn check_api_key(state: &ServeState, supplied: Option<&str>) -> Result<(), Response> {
    let stored = state
        .open_vault()
        .await
        .ok()
        .and_then(|vault| vault.get(secret_keys::TORZNAB_API_KEY).ok().flatten());

    let Some(stored) = stored else {
        return Err(xml_status(
            StatusCode::SERVICE_UNAVAILABLE,
            error_xml(
                100,
                "this sharerr instance has no Torznab API key yet — generate one in Settings",
            ),
        ));
    };

    let stored = secrecy::ExposeSecret::expose_secret(&stored);
    if crate::secrets::constant_time_eq(stored, supplied.unwrap_or_default()) {
        Ok(())
    } else {
        tracing::warn!("rejected a torznab request with a bad api key");
        Err(xml_status(
            StatusCode::UNAUTHORIZED,
            error_xml(100, "incorrect user credentials"),
        ))
    }
}

fn xml(body: String) -> Response {
    (
        [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
        body,
    )
        .into_response()
}

fn xml_status(status: StatusCode, body: String) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use sharerr_core::model::{ExternalIds, MediaSource, ShareState};
    use std::path::PathBuf;

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
            },
            info_hash: Some("ab".repeat(20)),
            state: ShareState::Seeding,
            last_error: None,
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
            },
            ..episode(title, 1, 1)
        }
    }

    fn render(item: &SharedItem) -> String {
        feed_xml(&[FeedItem {
            item,
            download_url: "http://seed.example:8477/torrents/x.torrent".to_owned(),
            seeders: 1,
            leechers: 0,
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

    #[test]
    fn the_enclosure_and_size_are_what_a_client_downloads() {
        let xml = render(&episode("X.S01E01", 1, 1));
        assert!(xml.contains("<size>2147483648</size>"), "{xml}");
        assert!(
            xml.contains(r#"<enclosure url="http://seed.example:8477/torrents/x.torrent" length="2147483648" type="application/x-bittorrent"/>"#),
            "{xml}"
        );
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
        assert!(caps.contains(
            r#"<tv-search available="yes" supportedParams="q,season,ep,tvdbid,imdbid"/>"#
        ));
        assert!(caps.contains(r#"name="Movies""#));
        assert!(caps.ends_with("</caps>"));
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
}
