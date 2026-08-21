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

use std::borrow::Cow;
use std::fmt::Write as _;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use sharerr_core::endpoint::now_epoch;
use sharerr_core::model::{ExternalIds, MediaSource, MediaSpec, SharedItem};

use secrecy::SecretString;

use sharerr_store::PeerScope;

use crate::state::ServeState;

/// Newznab category numbers. Sonarr and Radarr filter on these, and a release in
/// the wrong one is invisible to the app that wants it.
pub(crate) const CAT_TV: u32 = 5000;
pub(crate) const CAT_MOVIES: u32 = 2000;
pub(crate) const CAT_AUDIO: u32 = 3000;
pub(crate) const CAT_XXX: u32 = 6000;
pub(crate) const CAT_BOOKS: u32 = 7000;

/// Every category with its display name, in the order the caps document lists
/// them. The one table behind [`caps_xml`], Jackett's capability list, and
/// Jackett's per-result category names, so the three cannot disagree about
/// what this instance shares.
pub(crate) const CATEGORIES: &[(u32, &str)] = &[
    (CAT_MOVIES, "Movies"),
    (CAT_TV, "TV"),
    (CAT_AUDIO, "Audio"),
    (CAT_XXX, "XXX"),
    (CAT_BOOKS, "Books"),
];

/// The swarm figures every renderer advertises: one seeder — this instance —
/// and free downloads. Constants rather than tracker truth because Prowlarr
/// drops releases with zero seeders, and the tracker legitimately knows of none
/// until a peer announces — a truthful zero would hide every release nobody has
/// taken yet. The XML feed and Jackett's JSON both read these, because the two
/// renderers disagreeing about the same release is a known failure mode here.
pub(crate) const ADVERTISED_SEEDERS: u32 = 1;
pub(crate) const ADVERTISED_PEERS: u32 = 1;
pub(crate) const DOWNLOAD_VOLUME_FACTOR: f32 = 0.0;
pub(crate) const UPLOAD_VOLUME_FACTOR: f32 = 1.0;

/// The display name for a category id.
pub(crate) fn category_name(id: u32) -> &'static str {
    CATEGORIES
        .iter()
        .find(|(cat, _)| *cat == id)
        .map_or("Other", |(_, name)| name)
}

/// Every search function the API answers, with the `t=` aliases it is requested
/// under and the parameters the caps document advertises for it.
///
/// One table generates both the `<searching>` block and the dispatcher's accepted
/// set, so the two cannot drift apart the way independently edited lists would —
/// caps advertising `music-search` while the dispatcher answers `t=music` with
/// "no such function" is exactly the query a friend's Lidarr sends. Music and
/// book searches advertise only `q`: [`SearchQuery`] has no artist/album/author
/// fields, and claiming params that serde drops makes a filtered search match
/// everything.
const SEARCH_FUNCTIONS: &[(&str, &[&str], &str)] = &[
    ("search", &["search"], "q"),
    (
        "tv-search",
        &["tvsearch", "tv-search"],
        "q,season,ep,tvdbid,imdbid",
    ),
    (
        "movie-search",
        &["movie", "moviesearch", "movie-search"],
        "q,imdbid,tmdbid",
    ),
    (
        "music-search",
        &["music", "musicsearch", "music-search"],
        "q",
    ),
    (
        "audio-search",
        &["audio", "audiosearch", "audio-search"],
        "q",
    ),
    ("book-search", &["book", "booksearch", "book-search"], "q"),
];

/// Whether `t` names a search function this API answers.
fn is_search_function(t: &str) -> bool {
    SEARCH_FUNCTIONS
        .iter()
        .any(|(_, aliases, _)| aliases.contains(&t))
}

/// The Torznab category one item belongs in.
///
/// Derived from the *source* as well as the spec, because Whisparr's files are
/// episodes structurally and adult content categorically — a friend's app filters
/// on the category, so putting them in 5000 would offer them to Sonarr.
pub(crate) fn category_for(item: &SharedItem) -> u32 {
    match (item.source, &item.spec) {
        (MediaSource::Whisparr, _) => CAT_XXX,
        (_, MediaSpec::Episode { .. }) => CAT_TV,
        (_, MediaSpec::Movie { .. }) => CAT_MOVIES,
        (_, MediaSpec::Track { .. }) => CAT_AUDIO,
        (_, MediaSpec::Book { .. }) => CAT_BOOKS,
    }
}

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
///
/// Borrows when nothing needs escaping, which is the overwhelmingly common case:
/// info hashes are hex, and most download URLs and titles contain none of the
/// five. A feed is ten-odd escaped fields per item and can run to thousands of
/// items per request, so always allocating meant tens of thousands of throwaway
/// `String`s on every Prowlarr sync from every friend.
fn escape(raw: &str) -> Cow<'_, str> {
    fn needs_escaping(ch: char) -> bool {
        matches!(ch, '&' | '<' | '>' | '"' | '\'')
            || ((ch as u32) < 0x20 && ch != '\t' && ch != '\n' && ch != '\r')
    }

    if !raw.contains(needs_escaping) {
        return Cow::Borrowed(raw);
    }

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
    Cow::Owned(out)
}

/// What the indexer says it can do.
///
/// Prowlarr fetches this once and uses it to decide which searches to send. The
/// supported-params lists are the load-bearing part: claiming `tvdbid` here is
/// what makes Sonarr search by id instead of by free text, which is the difference
/// between a reliable match and a fuzzy one.
pub fn caps_xml() -> String {
    let mut out = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<caps>
  <server title="sharerr"/>
  <limits max="500" default="100"/>
  <searching>
"#,
    );
    for (element, _, params) in SEARCH_FUNCTIONS {
        let _ = writeln!(
            out,
            r#"    <{element} available="yes" supportedParams="{params}"/>"#
        );
    }
    out.push_str("  </searching>\n  <categories>\n");
    for (id, name) in CATEGORIES {
        let _ = writeln!(out, r#"    <category id="{id}" name="{name}"/>"#);
    }
    out.push_str("  </categories>\n</caps>");
    out
}

/// Render a Unix timestamp as RFC 2822, the only date format RSS accepts.
///
/// **Not cosmetic.** Sonarr and Radarr reject an entire feed whose items have no
/// `pubDate` — "Each item in the RSS feed must have a pubDate element with a valid
/// publish date" — so a feed without this cannot be added as an indexer at all.
/// Not obvious from the caps document or item XML in isolation — only a real
/// Sonarr integration exposes it.
///
/// An item with no stored timestamp falls back to the Unix epoch rather than being
/// omitted. A wrong-but-valid date costs an ordering quirk; a missing one costs the
/// whole feed.
fn rfc2822(created_at: Option<i64>) -> String {
    let stamp = created_at
        .and_then(|secs| time::OffsetDateTime::from_unix_timestamp(secs).ok())
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);

    stamp
        .format(&time::format_description::well_known::Rfc2822)
        .unwrap_or_else(|_| "Thu, 01 Jan 1970 00:00:00 +0000".to_owned())
}

/// One release, as the feed publishes it.
#[derive(Debug, Clone)]
pub struct FeedItem<'a> {
    pub item: &'a SharedItem,
    /// Absolute URL the friend's client fetches the `.torrent` from.
    pub download_url: String,
    /// The same release as a magnet URI, for the clients that prefer one.
    /// Empty when it could not be built (no info hash, which a seeding item
    /// never lacks) — an empty attribute is simply not emitted.
    pub magnet_url: String,
}

/// Query-string escaping via form_urlencoded: magnet consumers parse the tail as
/// a query string, and `+` for space is the convention there.
fn encode_component(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

/// Render a magnet URI for one release.
///
/// `xt` is the identity, `dn` the display name, `xl` the exact length, and one
/// `tr` per announce tier — the same tiers the `.torrent` itself carries, so a
/// client arriving by magnet announces to the same tracker. Worth stating
/// honestly: sharerr's torrents are private, and some clients refuse metadata
/// exchange on private torrents, so the `.torrent` enclosure stays the primary
/// path and the magnet is the convenience.
///
/// The announce tiers arrive already percent-encoded, via [`encode_component`].
/// They are the same handful of strings for every item in a response, so taking
/// them raw meant a feed of a few thousand releases re-encoded them a few
/// thousand times to produce identical output. The display name genuinely varies
/// per item and is still encoded here.
pub(crate) fn magnet_uri(
    info_hash: &str,
    title: &str,
    size: u64,
    encoded_announces: &[String],
) -> String {
    let mut out = format!(
        "magnet:?xt=urn:btih:{info_hash}&dn={}",
        encode_component(title)
    );
    if size > 0 {
        let _ = write!(out, "&xl={size}");
    }
    for announce in encoded_announces {
        let _ = write!(out, "&tr={announce}");
    }
    out
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
        let category = category_for(item);
        // Unwrapped safely: `seeding_items` guarantees an info hash, and an item
        // without one is filtered out before it reaches here.
        let info_hash = item.info_hash.as_deref().unwrap_or_default();

        // See ADVERTISED_SEEDERS for why these are constants, not tracker truth.

        let _ = write!(
            out,
            r#"    <item>
      <title>{title}</title>
      <guid isPermaLink="false">{hash}</guid>
      <link>{link}</link>
      <pubDate>{pub_date}</pubDate>
      <size>{size}</size>
      <category>{category}</category>
      <enclosure url="{link}" length="{size}" type="application/x-bittorrent"/>
      <torznab:attr name="category" value="{category}"/>
      <torznab:attr name="seeders" value="{seeders}"/>
      <torznab:attr name="peers" value="{peers}"/>
      <torznab:attr name="infohash" value="{hash}"/>
      <torznab:attr name="downloadvolumefactor" value="{down_factor}"/>
      <torznab:attr name="uploadvolumefactor" value="{up_factor}"/>
"#,
            // The release title, never `info.name`. See the module header.
            title = escape(&item.release_title),
            hash = escape(info_hash),
            link = escape(&entry.download_url),
            pub_date = rfc2822(item.created_at),
            size = item.size,
            seeders = ADVERTISED_SEEDERS,
            peers = ADVERTISED_PEERS,
            down_factor = DOWNLOAD_VOLUME_FACTOR,
            up_factor = UPLOAD_VOLUME_FACTOR,
        );

        if !entry.magnet_url.is_empty() {
            let _ = writeln!(
                out,
                "      <torznab:attr name=\"magneturl\" value=\"{}\"/>",
                escape(&entry.magnet_url)
            );
        }

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
    pub q: Option<String>,
    pub season: Option<u32>,
    pub ep: Option<u32>,
    pub tvdbid: Option<i64>,
    pub tmdbid: Option<i64>,
    pub imdbid: Option<String>,
}

impl SearchQuery {
    /// The free-text needle, normalised, or `None` when the query has no usable
    /// one. Computed once per request by [`collect`] — it is constant across
    /// every candidate item.
    pub fn needle(&self) -> Option<String> {
        self.q
            .as_deref()
            .map(|q| q.trim().to_lowercase())
            .filter(|q| !q.is_empty())
    }

    /// [`Self::matches_with`] normalising the needle itself; a convenience for
    /// tests, where one call has one item.
    #[cfg(test)]
    fn matches(&self, item: &SharedItem) -> bool {
        self.matches_with(self.needle().as_deref(), item)
    }

    /// Whether an item satisfies every constraint the query set, given the
    /// needle [`Self::needle`] normalised once for the whole request.
    ///
    /// Filters are ANDed and an absent filter matches everything, which is what
    /// makes a bare `t=tvsearch` return the whole library — the behaviour Prowlarr
    /// relies on for its "test" button and for RSS sync.
    pub fn matches_with(&self, needle: Option<&str>, item: &SharedItem) -> bool {
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
            // A season or episode filter cannot be satisfied by anything that is
            // not an episode, so asking for one excludes films, music and books
            // rather than silently ignoring the filter.
            MediaSpec::Movie { .. } | MediaSpec::Track { .. } | MediaSpec::Book { .. } => {
                if self.season.is_some() || self.ep.is_some() {
                    return false;
                }
            }
        }

        match needle {
            None => true,
            Some(needle) => {
                item.release_title.to_lowercase().contains(needle)
                    // Music and books are searched by creator far more than film
                    // and television are, so an artist or author name has to match.
                    || item
                        .spec
                        .creator()
                        .is_some_and(|c| c.to_lowercase().contains(needle))
                    || item.spec.title().to_lowercase().contains(needle)
            }
        }
    }
}

/// Compare IMDb ids tolerantly, via [`ExternalIds::imdb_bare`] on both sides.
///
/// Comparing them literally means an id search silently matches nothing, which
/// looks exactly like "the friend has none of this".
fn imdb_matches(stored: Option<&str>, wanted: &str) -> bool {
    stored.is_some_and(|stored| ExternalIds::imdb_bare(stored) == ExternalIds::imdb_bare(wanted))
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

pub fn routes(serve: Arc<ServeState>) -> axum::Router {
    axum::Router::new()
        .route("/api", axum::routing::get(api))
        // Gossip rides the same per-peer-key authentication as the feed — the
        // whole point of putting it under /api rather than a second surface.
        .route(
            "/api/gossip/endpoints",
            axum::routing::get(crate::gossip::pull).post(crate::gossip::push),
        )
        .with_state(Arc::clone(&serve))
        // Jackett's URL shapes, both the Torznab one and the admin surface.
        .merge(crate::jackett::routes(serve))
}

/// `GET /api?t=...`
pub async fn api(
    State(state): State<Arc<ServeState>>,
    caller: Caller,
    Query(query): Query<SearchQuery>,
) -> Response {
    // `caps` is fetched before the key is ever configured in Prowlarr, but
    // requiring the key here too is deliberate: it is one fewer endpoint that
    // says anything at all to an unauthenticated caller.
    if query.t == "caps" {
        xml(caps_xml())
    } else if is_search_function(&query.t) {
        search(&state, &query, caller.scope(), caller.key_hash()).await
    } else {
        xml_status(
            StatusCode::BAD_REQUEST,
            error_xml(202, &format!("no such function: {}", query.t)),
        )
    }
}

/// What a search matched, owned so more than one renderer can use it.
///
/// Torznab answers in XML and Jackett's own API answers the *same search* in JSON.
/// Running the query twice, once per renderer, is how the two would drift into
/// disagreeing about what this instance shares — the same class of mistake
/// `crate::checks` exists to prevent for `doctor` and the web UI's probes.
pub(crate) struct Matched {
    pub items: Vec<SharedItem>,
    /// Absolute base URL a client fetches `.torrent` files from.
    pub base: String,
    /// The announce URLs a magnet carries, current endpoint first — the same
    /// tiers freshly built torrents get, percent-encoded once per response
    /// rather than once per item, since they are identical for every release in
    /// it.
    announces_encoded: Vec<String>,
    /// How many items were considered, before filtering.
    pub total: usize,
}

impl Matched {
    /// The URL for one item's `.torrent`.
    pub fn download_url(&self, item: &SharedItem) -> String {
        format!(
            "{}{}",
            self.base,
            crate::tracker::torrent_download_path(item.info_hash.as_deref().unwrap_or_default())
        )
    }

    /// The same release as a magnet URI, or empty when there is no info hash.
    pub fn magnet_url(&self, item: &SharedItem) -> String {
        match item.info_hash.as_deref() {
            Some(hash) => magnet_uri(
                hash,
                &item.release_title,
                item.size,
                &self.announces_encoded,
            ),
            None => String::new(),
        }
    }
}

/// Run a search, or return the error response to send instead.
///
/// The error side is already a `Response` because both renderers want the failure
/// reported in their own content type, and there is exactly one caller of each.
pub(crate) async fn collect(
    state: &ServeState,
    query: &SearchQuery,
    scope: PeerScope,
    peer_token: &str,
) -> Result<Matched, (StatusCode, String)> {
    let store = state.store().await.map_err(|reason| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("sharerr is not ready: {reason}"),
        )
    })?;

    // The scope is applied by the store itself, in SQL, before any row is
    // decoded. What a friend may see is not a search filter they could widen —
    // it is decided by who they are, and it never reaches this function's query
    // logic at all.
    let items = store.seeding_items(scope).await.map_err(|err| {
        tracing::error!(error = %err, "torznab search failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "could not read the library".to_owned(),
        )
    })?;

    let config = state.config().await;
    let total = items.len();
    // The needle is constant across the whole request, so it is normalised once
    // here rather than once per candidate item.
    let needle = query.needle();
    let matched = items
        .into_iter()
        .filter(|item| query.matches_with(needle.as_deref(), item))
        .collect();

    // The magnet's `tr` tiers: every recently held endpoint, the same list a
    // freshly built torrent carries, with an announce token when one is set —
    // the magnet is an alternative to the `.torrent`, so it must grant the same
    // right to announce.
    //
    // The token embedded is the *caller's own* `key_hash`, not the shared
    // instance token — see `crate::tracker`'s `authenticate_token`. This is
    // what lets a real announce be attributed to this specific friend, and
    // what makes revoking them reach the tracker, not just the feed. Only
    // when the operator has a tracker token configured at all: with none
    // set, announces are unauthenticated for everyone and there is nothing
    // to attribute, so the URL carries no token segment, same as today.
    let token = state.tracker_token().await.is_some().then_some(peer_token);
    let announces: Vec<String> = state
        .endpoint()
        .recent()
        .iter()
        .filter_map(|base| sharerr_torrent::announce_url(base, token).ok())
        .map(|url| url.to_string())
        .collect();

    Ok(Matched {
        items: matched,
        base: config.public_base_url(),
        announces_encoded: announces.iter().map(|a| encode_component(a)).collect(),
        total,
    })
}

/// Render `matched` as the literal RSS/Torznab XML a client fetches — the one
/// renderer both the real feed and the settings-page preview go through, so
/// the two cannot drift into disagreeing about what a friend actually sees.
/// See [`FeedItem`] and [`feed_xml`].
pub(crate) fn render_feed(matched: &Matched) -> String {
    let entries: Vec<FeedItem<'_>> = matched
        .items
        .iter()
        .map(|item| FeedItem {
            item,
            download_url: matched.download_url(item),
            magnet_url: matched.magnet_url(item),
        })
        .collect();
    feed_xml(&entries)
}

async fn search(state: &ServeState, query: &SearchQuery, scope: PeerScope, peer_token: &str) -> Response {
    let matched = match collect(state, query, scope, peer_token).await {
        Ok(matched) => matched,
        Err((status, reason)) => return xml_status(status, error_xml(900, &reason)),
    };

    tracing::debug!(
        function = %query.t,
        returned = matched.items.len(),
        of = matched.total,
        "torznab search"
    );
    xml(render_feed(&matched))
}

/// An authenticated caller, carrying what they are allowed to see.
///
/// The scope has to come back from authentication, because it is a property of
/// the *caller* rather than of the query — and a caller cannot be trusted to say
/// who they are. Returning `()` and looking the peer up again inside the search
/// would mean two lookups that could disagree.
pub(crate) struct Caller {
    scope: PeerScope,
    /// Which peer row authenticated. There is no unauthenticated path, so
    /// every caller is a real peer, and the gossip endpoints — which require
    /// one, to know who said what — can always resolve it.
    peer_id: i64,
    /// This peer's own `key_hash` — the value now embedded as the tracker
    /// token in their magnet links, so an announce it grants can be
    /// attributed back to them. See `crate::tracker`'s `authenticate_token`.
    key_hash: String,
}

impl Caller {
    pub fn scope(&self) -> PeerScope {
        self.scope
    }

    pub fn peer_id(&self) -> i64 {
        self.peer_id
    }

    pub fn key_hash(&self) -> &str {
        &self.key_hash
    }
}

/// Authentication as an extractor: a feed handler that wants to answer at all
/// declares `caller: Caller` and cannot compile without it, where a hand-invoked
/// check is one forgotten call away from an open endpoint. The rejection is the
/// same XML error every feed surface has always sent.
impl axum::extract::FromRequestParts<Arc<ServeState>> for Caller {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &Arc<ServeState>,
    ) -> Result<Self, Self::Rejection> {
        #[derive(Deserialize)]
        struct Key {
            apikey: Option<String>,
        }

        // A query string too malformed to parse cannot be carrying a valid key,
        // so it is treated as an absent one rather than a different error.
        let apikey = axum::extract::Query::<Key>::try_from_uri(&parts.uri)
            .map(|query| query.0.apikey)
            .unwrap_or_default();

        // The source address, when the server was built with connect-info —
        // observed for peer endpoint memory, never required: authentication
        // works the same without it.
        let remote = parts
            .extensions
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|ci| ci.0.ip());

        check_api_key(state, apikey.as_deref(), remote).await
    }
}

/// Decide who an `apikey` belongs to.
///
/// Only one thing can satisfy it: a peer's own key. Each friend holds a
/// different key, so one can be revoked without disturbing the others, and the
/// request records who made it.
///
/// No match means the endpoint is closed rather than open: an indexer feed
/// lists everything this instance shares, and defaulting to unauthenticated
/// would publish the library to anyone who found the port.
async fn check_api_key(
    state: &ServeState,
    supplied: Option<&str>,
    remote: Option<std::net::IpAddr>,
) -> Result<Caller, Response> {
    let refused = || {
        xml_status(
            StatusCode::UNAUTHORIZED,
            error_xml(100, "incorrect user credentials"),
        )
    };

    // An absent key is refused the same way a wrong one is. Saying "this instance
    // has no key configured" to an unauthenticated caller would confirm the port
    // belongs to sharerr.
    let Some(supplied) = supplied.filter(|key| !key.is_empty()) else {
        return Err(refused());
    };

    // One indexed lookup on a SHA-256 of the supplied key.
    if let Ok(store) = state.store().await {
        match store.peer_by_key(&SecretString::from(supplied)).await {
            Ok(Some(peer)) => {
                // Recorded after authenticating, and failure to record is not
                // failure to authenticate: a read-only or busy database should not
                // take the feed down.
                record_sighting(
                    &store,
                    peer.id,
                    sharerr_store::EndpointKind::Api,
                    remote.map(|ip| ip.to_string()).as_deref(),
                )
                .await;
                tracing::debug!(
                    peer = %peer.label,
                    scope = peer.scope.as_str(),
                    "torznab request authenticated"
                );
                return Ok(Caller {
                    scope: peer.scope,
                    peer_id: peer.id,
                    key_hash: peer.key_hash,
                });
            }
            Ok(None) => {}
            Err(err) => {
                // A database that will not answer must not silently fall through to
                // a comparison that might pass — but it also must not be reported as
                // bad credentials, which would send the operator to the wrong place.
                tracing::error!(error = %err, "could not check peer keys");
                return Err(xml_status(
                    StatusCode::SERVICE_UNAVAILABLE,
                    error_xml(900, "could not check credentials"),
                ));
            }
        }
    }

    tracing::warn!("rejected a torznab request with a bad api key");
    Err(refused())
}

/// Best-effort: a peer was just seen (an authenticated feed request, a
/// tracker announce naming their own token — any first-hand sighting), so
/// record it, but a failure to record must never fail whatever already
/// succeeded to trigger the call. Shared by `torznab`'s own API-key
/// authentication and `crate::tracker::handle_announce`'s per-peer announce
/// attribution, since both boil down to the identical throttled
/// touch-then-record sequence, just for different
/// [`sharerr_store::EndpointKind`]s and address shapes (a bare source IP
/// here; `ip:port` for a real BitTorrent announce).
pub(crate) async fn record_sighting(
    store: &sharerr_store::Store,
    peer_id: i64,
    kind: sharerr_store::EndpointKind,
    addr: Option<&str>,
) {
    match store.touch_peer(peer_id).await {
        // The touch fired, so its five-minute throttle also gates the
        // endpoint observation — a Prowlarr RSS burst records one sighting,
        // not one per request.
        Ok(true) => {
            if let Some(addr) = addr
                && let Err(err) = store
                    .record_peer_endpoint(peer_id, kind, addr, now_epoch(), sharerr_store::ObservedVia::Direct)
                    .await
            {
                tracing::warn!(peer_id, error = %err, "could not record a peer's address");
            }
        }
        Ok(false) => {}
        Err(err) => {
            tracing::warn!(peer_id, error = %err, "could not touch a peer's last-seen time");
        }
    }
}

pub(crate) fn xml(body: String) -> Response {
    xml_status(StatusCode::OK, body)
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
    use sharerr_core::model::{MediaSource, ShareState};
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
                ..ExternalIds::default()
            },
            info_hash: Some("ab".repeat(20)),
            announce_token_fp: None,
            state: ShareState::Seeding,
            last_error: None,
            created_at: None,
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
        assert!(caps.contains(
            r#"<tv-search available="yes" supportedParams="q,season,ep,tvdbid,imdbid"/>"#
        ));
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
            everything.contains(&CAT_TV.to_string())
                && everything.contains(&CAT_MOVIES.to_string()),
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
}
