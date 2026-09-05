//! The items page: every file this instance knows about, in one sortable,
//! filterable list.
//!
//! Everything shown here already lives in the store — the sync loop just logs it
//! one line at a time as it happens, and qBittorrent's own torrent list answers a
//! different question ("what is my client doing", not "what has sharerr decided
//! to share and to whom"). Filtering and sorting happen in memory rather than in
//! SQL: the whole library is one page load's worth of rows, and doing it here
//! keeps every operator-facing knob in one place instead of split across a query
//! builder and a template.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use sharerr_core::{MediaSource, MediaSpec, ShareState, SharedItem};
use sharerr_store::{Peer, PeerScope, Store};

use std::collections::HashMap;

use super::WebState;
use super::peers::ago;
use super::settings::title_case;
use super::templates::{
    AddressCell, FilterOption, ItemDetailPage, ItemRow, ItemsPage, SortLink, SwarmRow, TokenStatus,
    render,
};

#[derive(Debug, Default, Deserialize)]
pub struct ItemsQuery {
    #[serde(default)]
    source: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    q: String,
    #[serde(default)]
    sort: String,
    #[serde(default)]
    dir: String,
}

/// The tooltip for each sortable header — kept beside the column list so a
/// column added there gets its sentence here, and the template stays free of
/// per-column branches.
pub(crate) fn column_hint(field: &str) -> &'static str {
    match field {
        "since" => "When sharerr first discovered the file",
        "title" => {
            "The title as the *arr knows it; the release name underneath is what the feed advertises"
        }
        "source" => "Which *arr app or library directory the file came from",
        "size" => "Size of the file on disk",
        "state" => {
            "Pending: waiting for a sync. Seeding: a torrent exists and the client holds it. Failed: the last attempt to share it did not work"
        }
        _ => "",
    }
}

/// Sortable columns, in the order the header row offers them.
pub(crate) const SORT_COLUMNS: &[(&str, &str)] = &[
    ("since", "Since"),
    ("title", "Title"),
    ("source", "Source"),
    ("size", "Size"),
    ("state", "State"),
];

pub async fn page(State(state): State<WebState>, Query(query): Query<ItemsQuery>) -> Response {
    let store = match state.store_or_503().await {
        Ok(store) => store,
        Err(response) => return *response,
    };

    let (mut items, error) = match store.all_items().await {
        Ok(items) => (items, None),
        Err(err) => (Vec::new(), Some(format!("could not list items: {err}"))),
    };
    let total = items.len();

    // Counted here, before the filters below narrow `items`: the tally answers
    // "what is the state of the library", which a filtered count cannot.
    let state_counts = ShareState::ALL
        .iter()
        .filter_map(|state| {
            let count = items.iter().filter(|item| item.state == *state).count();
            // A state nothing is in is noise in a strip meant to be read at a
            // glance, so it is left out rather than shown as a zero.
            (count > 0).then(|| crate::web::templates::StateCount {
                label: title_case(state.as_str()),
                count,
            })
        })
        .collect();
    // Also over the whole library: "how much am I actually seeding" is the
    // second half of the same question the tally answers.
    let seeding_bytes: u64 = items
        .iter()
        .filter(|item| item.state == ShareState::Seeding)
        .map(|item| item.size)
        .sum();
    // Third read of the same unfiltered slice, and the last one before the
    // filters below narrow it — a composition that moved with the search box
    // would answer a different question on every page load.
    let composition = crate::web::composition::compose(&items);

    let needle = query.q.trim().to_lowercase();
    if !needle.is_empty() {
        items.retain(|item| {
            item.spec.title().to_lowercase().contains(&needle)
                || item.release_title.to_lowercase().contains(&needle)
        });
    }
    if !query.source.is_empty() {
        items.retain(|item| item.source.as_str() == query.source);
    }
    if !query.state.is_empty() {
        items.retain(|item| item.state.as_str() == query.state);
    }
    if !query.kind.is_empty() {
        items.retain(|item| item.spec.kind_tag() == query.kind);
    }
    let shown_bytes: u64 = items.iter().map(|item| item.size).sum();

    // Default view is newest first, matching the order the sync log reports
    // things in. An explicit sort overrides it; the header links below always
    // carry `dir` explicitly, so there is never an ambiguous third click.
    let (sort, desc) = if query.sort.is_empty() {
        ("since", true)
    } else {
        (query.sort.as_str(), query.dir == "desc")
    };
    // `sort_by_cached_key` computes each item's key once, rather than
    // re-lowercasing the title on every comparison the sort makes.
    match sort {
        "title" => items.sort_by_cached_key(|item| item.spec.title().to_lowercase()),
        "source" => items.sort_by_key(|item| item.source.as_str()),
        "size" => items.sort_by_key(|item| item.size),
        "state" => items.sort_by_key(|item| item.state.as_str()),
        _ => items.sort_by_key(|item| item.created_at.unwrap_or(0)),
    }
    if desc {
        items.reverse();
    }

    let peers = store.list_peers().await.unwrap_or_default();
    let active: Vec<Peer> = peers.into_iter().filter(|p| !p.is_revoked()).collect();

    // Same instance-wide-not-per-row reasoning for both: every seeding
    // torrent announces to the same live endpoint and is checked against the
    // same admitted tokens, so there is exactly one answer to compute for the
    // whole page rather than once per row.
    let (current_token_fp, previous_token_fp) = token_fingerprints(&state.serve).await;
    // `None` when nothing is configured to announce to yet — the same
    // condition that blocks the tracker itself (`TorrentError::NoAdvertisedHost`).
    let announce_url = current_announce_url(&state.serve, current_token_fp.is_some());
    let tokens = TokenFps {
        current: current_token_fp.as_deref(),
        previous: previous_token_fp.as_deref(),
    };
    // The tracker's own view of who is in each swarm right now — first-hand,
    // in memory, and only present for torrents with at least one live peer,
    // so a miss below means "nobody", not "unknown".
    let swarms: HashMap<String, SwarmCount> = state
        .serve
        .swarms()
        .snapshots()
        .await
        .into_iter()
        .map(|swarm| {
            (
                hex::encode(swarm.info_hash),
                SwarmCount {
                    complete: swarm.complete,
                    incomplete: swarm.incomplete,
                },
            )
        })
        .collect();

    let sort_links = SORT_COLUMNS
        .iter()
        .map(|(field, label)| {
            let active = *field == sort;
            // Clicking an inactive column starts it ascending, except `since`,
            // where "newest first" is the useful default and descending is what
            // every other timestamp column on this instance's pages already means.
            let next_dir = if active && !desc {
                "desc"
            } else if active && desc {
                "asc"
            } else if *field == "since" {
                "desc"
            } else {
                "asc"
            };
            SortLink {
                label,
                hint: column_hint(field),
                href: format!(
                    "?source={}&state={}&kind={}&q={}&sort={field}&dir={next_dir}",
                    urlencode(&query.source),
                    urlencode(&query.state),
                    urlencode(&query.kind),
                    urlencode(&query.q),
                ),
                active,
                dir: if active {
                    if desc { "desc" } else { "asc" }
                } else {
                    ""
                },
            }
        })
        .collect();

    render(&ItemsPage {
        signed_in: true,
        error,
        total,
        shown: items.len(),
        state_counts,
        seeding_size: human_size(seeding_bytes),
        shown_size: human_size(shown_bytes),
        composition,
        items: items
            .iter()
            .map(|item| {
                let swarm = item
                    .info_hash
                    .as_deref()
                    .and_then(|hash| swarms.get(&hash.to_lowercase()))
                    .copied();
                row(item, &active, announce_url.as_deref(), tokens, swarm)
            })
            .collect(),
        source_options: MediaSource::ALL
            .iter()
            .map(|s| FilterOption {
                value: s.as_str(),
                label: title_case(s.as_str()),
            })
            .collect(),
        state_options: ShareState::ALL
            .iter()
            .map(|s| FilterOption {
                value: s.as_str(),
                label: title_case(s.as_str()),
            })
            .collect(),
        kind_options: MediaSpec::KIND_TAGS
            .iter()
            .map(|k| FilterOption {
                value: k,
                label: title_case(k),
            })
            .collect(),
        source_filter: query.source,
        state_filter: query.state,
        kind_filter: query.kind,
        q: query.q,
        sort_links,
    })
}

use crate::torznab::encode_component as urlencode;

/// Whether `scope` would show `item` in the Torznab feed — the same rule
/// `Store::seeding_items` applies in SQL, restated over an in-memory item so this
/// page can answer "who sees this" without a query per row.
fn scope_admits(scope: PeerScope, item: &SharedItem) -> bool {
    if scope.allows(item.source) {
        return true;
    }
    if !MediaSource::KIND_SCOPED.contains(&item.source) {
        return false;
    }
    let Some(kind) = scope.directory_kind() else {
        return false;
    };
    item.spec.kind_tag() == kind
}

/// A short explanation for a state that would otherwise read as a dead end —
/// see the field comment on [`crate::web::templates::ItemRow::state_hint`].
///
/// `Pending` genuinely should not linger: `Syncer::share` records it, then
/// immediately either fails (which sets `Failed` with a reason) or reaches
/// `Seeding` in the same call — so a row still `Pending` on a later page load
/// means the process died mid-share, not that anything is queued behind it.
fn state_hint(state: ShareState) -> Option<&'static str> {
    match state {
        ShareState::Pending => Some(
            "recorded but not finished — sharerr likely restarted mid-share; \
             the next sync retries it",
        ),
        ShareState::Unshared => {
            Some("not a fault — the tag was removed upstream, so the share was withdrawn")
        }
        ShareState::Seeding | ShareState::Failed => None,
    }
}

/// Who can currently find this item in their feed. Only meaningful once an item
/// is `Seeding` — `Store::seeding_items` excludes everything else, so a pending
/// or failed item is invisible to every friend regardless of scope.
fn visible_to(item: &SharedItem, peers: &[Peer]) -> String {
    if item.state != ShareState::Seeding {
        return String::new();
    }
    let names: Vec<&str> = peers
        .iter()
        .filter(|peer| scope_admits(peer.scope, item))
        .map(|peer| peer.label.as_str())
        .collect();
    if names.is_empty() {
        // Seeding but nobody's scope currently admits it — worth saying plainly,
        // since it looks identical to "shared" everywhere else on this row.
        "no friend's scope covers it".to_owned()
    } else {
        names.join(", ")
    }
}

/// Live seeder/leecher counts for one swarm, as the tracker sees them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SwarmCount {
    pub complete: usize,
    pub incomplete: usize,
}

/// `"2↑ 1↓"` for the column and `"2 seeding · 1 downloading"` for its
/// tooltip. Empty when nobody is in the swarm — the template renders the dash
/// itself, so the cell reads the same as every other "nothing here" on the page.
fn peers_cell(swarm: Option<SwarmCount>) -> (String, String) {
    match swarm {
        Some(SwarmCount {
            complete,
            incomplete,
        }) => (
            format!("{complete}↑ {incomplete}↓"),
            format!("{complete} seeding · {incomplete} downloading"),
        ),
        None => (String::new(), String::new()),
    }
}

/// What the torrent client itself reports for this item's ratio — see
/// `SharedItem::achieved_ratio`. Empty (rendered as a dash) before a torrent
/// has reported anything; once it has, the tooltip either names the specific
/// limit the client is enforcing on this torrent, or says plainly that it
/// isn't holding this torrent to one — a client's own global default,
/// unlimited, or (on some backends) simply not something it can report,
/// which sharerr does not try to distinguish from here.
fn ratio_cell(item: &SharedItem) -> (String, String) {
    let Some(ratio) = item.achieved_ratio else {
        return (String::new(), String::new());
    };
    let value = if ratio.is_infinite() {
        "∞".to_owned()
    } else {
        format!("{ratio:.2}")
    };
    let hint = match item.ratio_limit_reported {
        Some(limit) => format!("Per-torrent limit the client is enforcing: {limit:.2}"),
        None => "The client is not holding this torrent to a fixed per-torrent limit \
                  — its own global default, unlimited, or (on some backends) not \
                  something it can report"
            .to_owned(),
    };
    (value, hint)
}

/// `"Sonarr series 42, file 1337"`: the *arr's own identifiers — the join key
/// an operator greps its logs for when a row here and an entry there
/// disagree. Shared by the table row and the detail page so the two cannot
/// word it differently.
fn source_hint(item: &SharedItem) -> String {
    format!(
        "{} {} {}, file {}",
        title_case(item.source.as_str()),
        match item.spec {
            sharerr_core::MediaSpec::Episode { .. } => "series",
            sharerr_core::MediaSpec::Movie { .. } => "movie",
            sharerr_core::MediaSpec::Track { .. } => "artist",
            sharerr_core::MediaSpec::Book { .. } => "author",
        },
        item.source_id,
        item.file_id
    )
}

/// The first 12 hex characters, or the whole thing if somehow shorter: enough
/// to tell two torrents apart at a glance, narrow enough not to squeeze the
/// title column. The full hash stays on hover and behind the copy button.
fn short_hash(hash: &str) -> String {
    hash.get(..12).unwrap_or(hash).to_owned()
}

fn row(
    item: &SharedItem,
    peers: &[Peer],
    announce_url: Option<&str>,
    tokens: TokenFps<'_>,
    swarm: Option<SwarmCount>,
) -> ItemRow {
    let (peers_live, peers_hint) = peers_cell(swarm);
    let (ratio, ratio_hint) = ratio_cell(item);
    ItemRow {
        title: item.spec.title().to_owned(),
        release_title: item.release_title.clone(),
        arr_path: item.arr_path.display().to_string(),
        kind: item.spec.kind_tag(),
        source_label: title_case(item.source.as_str()),
        size: human_size(item.size),
        state_label: title_case(item.state.as_str()),
        state_hint: state_hint(item.state),
        ratio,
        ratio_hint,
        visible_to: visible_to(item, peers),
        since: item.created_at.map(ago).unwrap_or_default(),
        info_hash: item.info_hash.clone(),
        info_hash_short: item.info_hash.as_deref().map(short_hash),
        peers: peers_live,
        peers_hint,
        // The *arr's own identifiers — the join key an operator greps its
        // logs for when a row here and an entry there disagree.
        source_hint: source_hint(item),
        // A torrent with no info hash has not been built yet, so there is
        // nothing meaningful to announce either — `None` regardless of
        // whether the tracker itself is configured.
        announce_url: item.info_hash.as_ref().and(announce_url).map(str::to_owned),
        token_fp: item.announce_token_fp.clone(),
        token_status: token_status(item, tokens),
        ids: ids_summary(&item.ids),
        last_error: item.last_error.clone(),
        created_by_sharerr: item.created_by_sharerr,
        since_absolute: item
            .created_at
            .map(super::peers::absolute)
            .unwrap_or_default(),
        source: item.source.as_str(),
        file_id: item.file_id,
    }
}

/// "tvdb 12345 · imdb tt0111161": every external ID the item carries, in the
/// order a friend's *arr would try them. Empty when there are none.
fn ids_summary(ids: &sharerr_core::ExternalIds) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(v) = ids.tvdb {
        parts.push(format!("tvdb {v}"));
    }
    if let Some(v) = ids.tmdb {
        parts.push(format!("tmdb {v}"));
    }
    if let Some(v) = ids.tvmaze {
        parts.push(format!("tvmaze {v}"));
    }
    if let Some(v) = &ids.imdb {
        parts.push(format!("imdb {v}"));
    }
    if let Some(v) = &ids.musicbrainz {
        parts.push(format!("musicbrainz {v}"));
    }
    if let Some(v) = &ids.goodreads {
        parts.push(format!("goodreads {v}"));
    }
    if let Some(v) = &ids.isbn {
        parts.push(format!("isbn {v}"));
    }
    parts.join(" · ")
}

/// The tracker's admitted-token fingerprints, bundled rather than threaded as
/// two parallel `Option`s: `row` and `token_status` only ever want both
/// together, and a struct with named fields is harder to accidentally
/// transpose than the fourth and fifth arguments in a positional call.
#[derive(Debug, Clone, Copy, Default)]
struct TokenFps<'a> {
    current: Option<&'a str>,
    previous: Option<&'a str>,
}

/// Whether this item's last-confirmed announce token still matches one of
/// the tokens the tracker currently admits. See
/// [`crate::sync::token_fingerprint`] for how each side is derived, and
/// [`crate::state::ServeState::tracker_tokens`] for why there are two:
/// during a rotation the tracker admits the previous token alongside the
/// current one, so an item on the previous token is still being served, not
/// dead — it deserves a state of its own rather than reading identically to
/// one the tracker has actually stopped admitting.
fn token_status(item: &SharedItem, tokens: TokenFps<'_>) -> TokenStatus {
    // No torrent, nothing to have confirmed yet — not the same condition as a
    // torrent that *was* confirmed and has since drifted.
    if item.info_hash.is_none() {
        return TokenStatus::None;
    }
    match (item.announce_token_fp.as_deref(), tokens.current) {
        (None, None) => TokenStatus::None,
        (Some(stored), Some(current)) if stored == current => TokenStatus::Valid,
        (Some(stored), _) if tokens.previous == Some(stored) => TokenStatus::Rotating,
        // Either it changed with no rotation grace covering it, or nothing
        // has confirmed this item since a token was first configured (or
        // removed) — both are "not admitted by anything current", which is
        // exactly what red is for.
        _ => TokenStatus::Stale,
    }
}

/// `(current, previous)` tracker-token fingerprints for the whole page —
/// derived once and shared by every row, the same reasoning as
/// [`current_announce_url`]. A vault the tracker itself could not open
/// renders as "no token info" rather than failing the page — admission fails
/// closed elsewhere; this is display only.
async fn token_fingerprints(state: &crate::state::ServeState) -> (Option<String>, Option<String>) {
    let (current, previous) = state.tracker_tokens().await.unwrap_or_default();
    (
        current.map(|token| crate::sync::fingerprint(&token)),
        previous.map(|token| crate::sync::fingerprint(&token)),
    )
}

/// The announce URL a freshly built torrent would carry right now, with the
/// token itself replaced by a `<token>` placeholder: the same construction
/// `BuiltinTracker::announce_set` uses, computed live rather than stored so
/// it always reflects whatever the endpoint currently resolves to, but never
/// rendering a live secret to the page — the token's own fingerprint is
/// shown separately per row via [`token_status`]. `None` when nothing is
/// configured to announce to yet.
///
/// `has_token` comes from the caller's own [`token_fingerprints`] call rather
/// than a second lookup here: `state.tracker_token()` alone would only warm
/// half of [`crate::state::ServeState::tracker_tokens`]'s cache, forcing the
/// very next call to derive the vault key a second time. No longer `async`
/// itself now that it no longer touches the vault — `state.endpoint()` is a
/// plain in-memory read.
fn current_announce_url(state: &crate::state::ServeState, has_token: bool) -> Option<String> {
    let base = state.endpoint().current()?;
    let url = sharerr_torrent::announce_url(&base, None).ok()?;
    if has_token {
        Some(format!("{url}/<token>"))
    } else {
        Some(url.to_string())
    }
}

/// A byte count as a person reads it — binary units, one decimal past the first,
/// because "734003200" next to "1073741824" tells an operator nothing a glance
/// should have to parse.
pub(crate) fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

// ---------------------------------------------------------------------------
// The per-item detail page and manual actions
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct DetailQuery {
    #[serde(default)]
    ok: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// Everything about one item. Reached from a row on [`page`] above.
pub async fn detail(
    State(state): State<WebState>,
    Path((source, file_id)): Path<(MediaSource, i64)>,
    Query(query): Query<DetailQuery>,
) -> Response {
    let store = match state.store_or_503().await {
        Ok(store) => store,
        Err(response) => return *response,
    };
    let item = match fetch(&store, source, file_id).await {
        Ok(item) => item,
        Err(response) => return *response,
    };

    render(&build_detail(&state, &store, item, query).await)
}

/// Force a fresh torrent right now: retry a `Failed` item, or ask a `Seeding`
/// one to be rebuilt from the file as it exists on disk today.
///
/// Both are a full sync pass, not a bespoke single-item share — see
/// [`trigger_sync_now`]'s doc for why that is the correct choice here, not a
/// shortcut around a harder one.
pub async fn retry(
    State(state): State<WebState>,
    Path((source, file_id)): Path<(MediaSource, i64)>,
) -> Response {
    let store = match state.store_or_503().await {
        Ok(store) => store,
        Err(response) => return *response,
    };
    if let Err(response) = fetch(&store, source, file_id).await {
        return *response;
    }

    tracing::info!(%source, file_id, "retry requested — running a sync pass now");
    redirect_with_outcome(source, file_id, trigger_sync_now(&state).await)
}

/// Remove the current torrent (if this instance owns it), clear the item's
/// torrent identity, and run a sync pass now so the rebuild happens
/// immediately rather than at the next scheduled interval.
pub async fn rebuild(
    State(state): State<WebState>,
    Path((source, file_id)): Path<(MediaSource, i64)>,
) -> Response {
    let store = match state.store_or_503().await {
        Ok(store) => store,
        Err(response) => return *response,
    };
    let item = match fetch(&store, source, file_id).await {
        Ok(item) => item,
        Err(response) => return *response,
    };

    let syncer = match state.serve.syncer().await {
        Ok(syncer) => syncer,
        Err(err) => return redirect_with_error(source, file_id, &err),
    };
    if let Err(err) = syncer.prepare_rebuild(&item).await {
        return redirect_with_error(source, file_id, &err.to_string());
    }

    tracing::info!(%source, file_id, "rebuild requested — running a sync pass now");
    redirect_with_outcome(source, file_id, trigger_sync_now(&state).await)
}

/// Stop sharing one file on demand — the same effect a tag removal upstream
/// has, without waiting for one. Never touches the file itself; see
/// `Syncer::unshare_one`.
pub async fn unshare(
    State(state): State<WebState>,
    Path((source, file_id)): Path<(MediaSource, i64)>,
) -> Response {
    let store = match state.store_or_503().await {
        Ok(store) => store,
        Err(response) => return *response,
    };
    let item = match fetch(&store, source, file_id).await {
        Ok(item) => item,
        Err(response) => return *response,
    };
    if item.state == ShareState::Unshared {
        // Already unshared is not an error worth showing — same convention
        // `peers::revoke` follows for an already-revoked key.
        return Redirect::to(&format!("/items/{source}/{file_id}")).into_response();
    }

    let syncer = match state.serve.syncer().await {
        Ok(syncer) => syncer,
        Err(err) => return redirect_with_error(source, file_id, &err),
    };
    match syncer.unshare_one(&item).await {
        Ok(()) => {
            tracing::info!(%source, file_id, "unshared (the file was not touched)");
            redirect_with_ok(
                source,
                file_id,
                "unshared — the file itself was not touched",
            )
        }
        Err(err) => redirect_with_error(source, file_id, &err.to_string()),
    }
}

/// One item, or the 404/500 response every action and the detail page answer
/// with identically when it is missing or unreadable.
///
/// Boxed for the same reason `WebState::store_or_503` is: `Response` alone is
/// well over clippy's `result_large_err` threshold.
async fn fetch(
    store: &Store,
    source: MediaSource,
    file_id: i64,
) -> Result<SharedItem, Box<Response>> {
    match store.get(source, file_id).await {
        Ok(Some(item)) => Ok(item),
        Ok(None) => Err(Box::new(
            (StatusCode::NOT_FOUND, "no such item").into_response(),
        )),
        Err(err) => Err(Box::new(
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("could not load that item: {err}"),
            )
                .into_response(),
        )),
    }
}

/// Run a full sync pass right now, for the "Retry" and "Force rebuild"
/// actions.
///
/// A full pass, not a bespoke single-item share, for two reasons.
/// `Syncer::share` needs a freshly [`sharerr_core::Discovered`] item straight
/// from the source — a stored `SharedItem` cannot honestly stand in for one,
/// since it carries neither `scene_name` nor `original_path`, both of which
/// feed release-title synthesis. And a bare [`crate::state::ServeState::request_sync`]
/// nudge would silently do nothing on an instance that has periodic sync
/// turned off: that only wakes the background loop, which itself takes no
/// action at all while `[sync] enabled = false` — see `commands::serve::background`.
/// Running the pass directly, the same way `sharerr sync` does, is correct
/// regardless of that setting. `ShareState::Failed`'s own doc already frames
/// a retry as sync-pass-shaped ("retried on the next sync"), so this is
/// "the next sync", requested now instead of waited for.
async fn trigger_sync_now(state: &WebState) -> Result<(String, bool), String> {
    let syncer = state.serve.syncer().await?;
    match syncer.run(false).await {
        Ok(report) => Ok(report.describe(false)),
        Err(err) => Err(format!("{err:#}")),
    }
}

fn redirect_with_outcome(
    source: MediaSource,
    file_id: i64,
    outcome: Result<(String, bool), String>,
) -> Response {
    match outcome {
        Ok((message, failed)) if failed => redirect_with_error(source, file_id, &message),
        Ok((message, _)) => redirect_with_ok(source, file_id, &message),
        Err(err) => redirect_with_error(source, file_id, &err),
    }
}

fn redirect_with_ok(source: MediaSource, file_id: i64, message: &str) -> Response {
    Redirect::to(&format!(
        "/items/{source}/{file_id}?ok={}",
        urlencode(message)
    ))
    .into_response()
}

fn redirect_with_error(source: MediaSource, file_id: i64, message: &str) -> Response {
    Redirect::to(&format!(
        "/items/{source}/{file_id}?error={}",
        urlencode(message)
    ))
    .into_response()
}

/// The tracker's own live view of this item's swarm — `None` before a
/// torrent exists or when nobody is announcing right now.
async fn swarm_for(state: &WebState, item: &SharedItem) -> Option<SwarmRow> {
    let hash = item.info_hash.as_deref()?;
    let target = hash.to_lowercase();
    let swarm = state
        .serve
        .swarms()
        .snapshots()
        .await
        .into_iter()
        .find(|s| hex::encode(s.info_hash) == target)?;

    let more = swarm
        .peers
        .len()
        .saturating_sub(super::topology::MAX_SWARM_PEERS);
    let peers = swarm
        .peers
        .iter()
        .take(super::topology::MAX_SWARM_PEERS)
        .map(|addr| {
            let full = addr.to_string();
            AddressCell {
                masked: super::topology::mask_address(&full),
                full,
            }
        })
        .collect();

    Some(SwarmRow {
        title: item.spec.title().to_owned(),
        complete: swarm.complete,
        incomplete: swarm.incomplete,
        peers,
        more,
    })
}

async fn build_detail(
    state: &WebState,
    store: &Store,
    item: SharedItem,
    query: DetailQuery,
) -> ItemDetailPage {
    let config = state.serve.config().await;
    let peers = store.list_peers().await.unwrap_or_default();
    let active: Vec<Peer> = peers.into_iter().filter(|p| !p.is_revoked()).collect();

    let (current_token_fp, previous_token_fp) = token_fingerprints(&state.serve).await;
    let announce_url = current_announce_url(&state.serve, current_token_fp.is_some());
    let tokens = TokenFps {
        current: current_token_fp.as_deref(),
        previous: previous_token_fp.as_deref(),
    };
    let (ratio, ratio_hint) = ratio_cell(&item);

    // A path that fails to resolve at all (a non-absolute `arr_path`) is a
    // configuration problem `doctor` already reports; the detail page shows
    // the unmapped path and says plainly that it could not check existence,
    // rather than pretending it knows.
    let resolved = config.resolver().resolve_for(item.source, &item.arr_path);
    let (arr_path, sharerr_path, qbit_path, mapping_applied, path_exists) = match &resolved {
        Ok(paths) => (
            paths.arr.display().to_string(),
            paths.sharerr.display().to_string(),
            paths.qbit.display().to_string(),
            paths.mapping_applied,
            Some(paths.sharerr.exists()),
        ),
        Err(_) => (
            item.arr_path.display().to_string(),
            String::new(),
            String::new(),
            false,
            None,
        ),
    };

    let swarm = swarm_for(state, &item).await;

    ItemDetailPage {
        signed_in: true,
        source: item.source.as_str(),
        file_id: item.file_id,
        title: item.spec.title().to_owned(),
        release_title: item.release_title.clone(),
        // The torrent's own name always describes the file where it sits —
        // see `CLAUDE.md`'s first trap — so this and `release_title` are the
        // two strings worth showing side by side.
        file_name: item
            .arr_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned()),
        kind: item.spec.kind_tag(),
        source_label: title_case(item.source.as_str()),
        source_hint: source_hint(&item),
        size: human_size(item.size),
        state_label: title_case(item.state.as_str()),
        state_hint: state_hint(item.state),
        last_error: item.last_error.clone(),
        since: item.created_at.map(ago).unwrap_or_default(),
        since_absolute: item
            .created_at
            .map(super::peers::absolute)
            .unwrap_or_default(),
        info_hash: item.info_hash.clone(),
        created_by_sharerr: item.created_by_sharerr,
        ratio,
        ratio_hint,
        token_fp: item.announce_token_fp.clone(),
        token_status: token_status(&item, tokens),
        announce_url: item
            .info_hash
            .as_ref()
            .and(announce_url.as_deref())
            .map(str::to_owned),
        ids: ids_summary(&item.ids),
        visible_to: visible_to(&item, &active),
        media: item.media.clone(),
        arr_path,
        sharerr_path,
        qbit_path,
        mapping_applied,
        path_exists,
        swarm,
        can_retry: item.state == ShareState::Failed,
        can_rebuild: item.info_hash.is_some(),
        can_unshare: item.state != ShareState::Unshared,
        message_failed: query.error.is_some(),
        message: query.ok.or(query.error),
    }
}

#[cfg(test)]
mod tests;
