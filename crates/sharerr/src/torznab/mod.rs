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
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

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
pub(crate) fn encode_component(value: &str) -> String {
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
            ("tvdbid", item.ids.tvdb),
            ("tmdbid", item.ids.tmdb),
            ("tvmazeid", item.ids.tvmaze),
        ] {
            if let Some(value) = value {
                // An integer needs no escaping and no intermediate `String`.
                let _ = writeln!(
                    out,
                    "      <torznab:attr name=\"{name}\" value=\"{value}\"/>"
                );
            }
        }
        if let Some(imdb) = item.ids.imdb.as_deref() {
            let _ = writeln!(
                out,
                "      <torznab:attr name=\"imdbid\" value=\"{}\"/>",
                escape(imdb)
            );
        }

        // What the file actually is. Optional attributes throughout: an absent one
        // is a fact not known, and publishing it empty would have a friend's
        // quality profile match on `""` rather than skip the comparison.
        //
        // `video`, `audio`, `resolution` and `subs` are the names Jackett
        // established and every Torznab consumer reads; `audiochannels`, `hdr`,
        // `audiosamplerate` and `audiobitdepth` are not in that set, and are
        // emitted anyway because an unknown attribute costs a consumer nothing to
        // ignore and the information is real.
        if let Some(media) = item.media.as_ref() {
            for (name, value) in [
                ("resolution", media.resolution.as_deref()),
                ("video", media.video_codec.as_deref()),
                ("audio", media.audio_codec.as_deref()),
                ("audiochannels", media.audio_channels.as_deref()),
                ("language", media.audio_languages.as_deref()),
                ("subs", media.subtitles.as_deref()),
                ("runtime", media.runtime.as_deref()),
                ("hdr", media.dynamic_range.as_deref()),
                ("audiosamplerate", media.audio_sample_rate.as_deref()),
                ("audiobitdepth", media.audio_bit_depth.as_deref()),
            ] {
                if let Some(value) = value {
                    let _ = writeln!(
                        out,
                        "      <torznab:attr name=\"{name}\" value=\"{}\"/>",
                        escape(value)
                    );
                }
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
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SearchQuery {
    /// The Torznab function. `caps` for the capabilities document, or one of the
    /// search functions: `search`, `tvsearch`, `movie`, `music`, `book`.
    #[param(required = true, example = "tvsearch")]
    #[serde(default)]
    pub t: String,
    /// Free-text needle, matched against the release title.
    pub q: Option<String>,
    pub season: Option<u32>,
    /// Daily shows send `ep=MM/DD` rather than a number. That form has no
    /// season/episode to match against here, so it is read as "no episode
    /// filter" instead of failing the whole request with a bare 400 — which
    /// Prowlarr counts as an indexer failure and backs off from, hiding every
    /// other release too.
    #[serde(default, deserialize_with = "lenient_u32")]
    pub ep: Option<u32>,
    pub tvdbid: Option<i64>,
    pub tmdbid: Option<i64>,
    pub imdbid: Option<String>,
}

// The `ep` field's own doc comment above becomes its description in the
// OpenAPI document, which is the point of writing it there: the lenient
// parse is a thing a client author has to know about.

/// `Some` for a plain non-negative integer, `None` for anything else — see
/// [`SearchQuery::ep`].
fn lenient_u32<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Option<u32>, D::Error> {
    let raw: Option<String> = serde::Deserialize::deserialize(de)?;
    Ok(raw.and_then(|s| s.trim().parse().ok()))
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
                contains_ci(&item.release_title, needle)
                    // Music and books are searched by creator far more than film
                    // and television are, so an artist or author name has to match.
                    || item.spec.creator().is_some_and(|c| contains_ci(c, needle))
                    || contains_ci(item.spec.title(), needle)
            }
        }
    }
}

/// Whether `hay` contains `needle` case-insensitively, where `needle` is
/// already lowercased (by [`SearchQuery::needle`]).
///
/// ASCII text — every release title in practice — is compared in place over
/// byte windows rather than allocating a lowercased copy per field per item
/// per search. Anything else falls back to the full Unicode lowercasing, so
/// the answer is exactly what `hay.to_lowercase().contains(needle)` gives.
fn contains_ci(hay: &str, needle: &str) -> bool {
    if !hay.is_ascii() {
        return hay.to_lowercase().contains(needle);
    }
    let needle = needle.as_bytes();
    needle.is_empty()
        || hay
            .as_bytes()
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle))
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
    let (router, _) = api_router().with_state(serve).split_for_parts();
    router
}

/// The same routes without state, so [`crate::openapi`] can read the document
/// off the very declaration that mounts them — no database, no config, and no
/// second list to keep in step.
pub(crate) fn api_router() -> OpenApiRouter<Arc<ServeState>> {
    OpenApiRouter::new()
        .routes(routes!(api))
        // Gossip rides the same per-peer-key authentication as the feed — the
        // whole point of putting it under /api rather than a second surface.
        .routes(routes!(crate::gossip::pull, crate::gossip::push))
        // Jackett's URL shapes, both the Torznab one and the admin surface.
        .merge(crate::jackett::api_router())
}

/// `GET /api?t=...`
#[utoipa::path(
    get,
    path = "/api",
    tag = "torznab",
    operation_id = "torznab",
    security(("peerApiKey" = [])),
    params(SearchQuery),
    responses(
        (status = 200, content_type = "application/xml", description =
         "A Torznab document: the capabilities XML for `t=caps`, otherwise an RSS \
          feed of matching releases. Each item carries a `.torrent` link and a magnet \
          whose announce tiers are attributed to the calling peer.", body = String),
        (status = 400, content_type = "application/xml", description =
         "No such Torznab function — a Torznab `<error code=\"202\">`.", body = String),
        (status = 401, content_type = "application/xml", description =
         "No `apikey`, or one that matches no active peer. `t=caps` requires it too, \
          deliberately: one fewer endpoint that says anything to an unauthenticated \
          caller.", body = String),
        (status = 503, content_type = "application/xml",
         description = "The database is not open yet.", body = String),
    ),
)]
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
    /// The caller's own `key_hash`, to embed on `.torrent` download links the
    /// same way [`Self::magnet_url`] embeds it in announce tiers — so
    /// `crate::tracker::torrent_file` can serve back an announce rewritten for
    /// this specific friend instead of the shared instance token. `None` under
    /// the same condition the magnet omits a token entirely: no tracker token
    /// configured, so there is nothing to attribute.
    download_token: Option<String>,
    /// How many items were considered, before filtering.
    pub total: usize,
}

impl Matched {
    /// The URL for one item's `.torrent`.
    pub fn download_url(&self, item: &SharedItem) -> String {
        let path =
            crate::tracker::torrent_download_path(item.info_hash.as_deref().unwrap_or_default());
        match &self.download_token {
            Some(token) => format!("{}{path}?token={}", self.base, encode_component(token)),
            None => format!("{}{path}", self.base),
        }
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
        // The live endpoint, not `config.public_base_url()` — see
        // `ServeState::public_base_url`'s docs. The magnet tiers above
        // already use the live `endpoint().recent()`; the `.torrent`
        // download link must track the same address, or a gluetun-only
        // deployment hands out a `.torrent` pointing at
        // `http://localhost:<port>` on the friend's own box.
        base: state.public_base_url().await,
        announces_encoded: announces.iter().map(|a| encode_component(a)).collect(),
        download_token: token.map(str::to_owned),
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

async fn search(
    state: &ServeState,
    query: &SearchQuery,
    scope: PeerScope,
    peer_token: &str,
) -> Response {
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

        check_api_key(state, apikey.as_deref(), remote)
            .await
            .map_err(|rejection| *rejection)
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
///
/// The error is boxed only to keep this `Result` under clippy's
/// `result_large_err` threshold — `Response` alone is not. The one caller,
/// [`Caller`]'s `FromRequestParts` impl, unboxes before returning, since its
/// `Rejection` type is fixed by the trait.
async fn check_api_key(
    state: &ServeState,
    supplied: Option<&str>,
    remote: Option<std::net::IpAddr>,
) -> Result<Caller, Box<Response>> {
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
        return Err(Box::new(refused()));
    };

    // One indexed lookup on a SHA-256 of the supplied key.
    if let Ok(store) = state.store().await {
        match store.peer_by_key(&SecretString::from(supplied)).await {
            Ok(Some(peer)) => {
                // Recorded after authenticating, and failure to record is not
                // failure to authenticate: a read-only or busy database should not
                // take the feed down.
                record_sighting(
                    state,
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
                return Err(Box::new(xml_status(
                    StatusCode::SERVICE_UNAVAILABLE,
                    error_xml(900, "could not check credentials"),
                )));
            }
        }
    }

    tracing::warn!("rejected a torznab request with a bad api key");
    Err(Box::new(refused()))
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
///
/// `state` is only for the first-contact notification: the store decides
/// whether this sighting was the peer's very first
/// ([`sharerr_store::Touch::First`]), and that is the one event here worth
/// telling the operator about — the moment a key they handed out was
/// actually used.
pub(crate) async fn record_sighting(
    state: &ServeState,
    store: &sharerr_store::Store,
    peer_id: i64,
    kind: sharerr_store::EndpointKind,
    addr: Option<&str>,
) {
    let touch = match store.touch_peer(peer_id).await {
        Ok(touch) => touch,
        Err(err) => {
            tracing::warn!(peer_id, error = %err, "could not touch a peer's last-seen time");
            return;
        }
    };
    // The touch fired, so its five-minute throttle also gates the endpoint
    // observation — a Prowlarr RSS burst records one sighting, not one per
    // request.
    if touch.updated()
        && let Some(addr) = addr
        && let Err(err) = store
            .record_peer_endpoint(
                peer_id,
                kind,
                addr,
                Some(now_epoch()),
                sharerr_store::ObservedVia::Direct,
            )
            .await
    {
        tracing::warn!(peer_id, error = %err, "could not record a peer's address");
    }
    if touch == sharerr_store::Touch::First {
        crate::notify::peer_first_contact(state, store, peer_id, kind).await;
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
mod tests;
