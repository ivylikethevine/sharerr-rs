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

use axum::extract::{Query, State};
use axum::response::Response;
use serde::Deserialize;
use sharerr_core::{MediaSource, ShareState, SharedItem};
use sharerr_store::{Peer, PeerScope};

use super::WebState;
use super::peers::ago;
use super::settings::title_case;
use super::templates::{FilterOption, ItemRow, ItemsPage, SortLink, TokenStatus, render};

#[derive(Debug, Default, Deserialize)]
pub struct ItemsQuery {
    #[serde(default)]
    source: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    q: String,
    #[serde(default)]
    sort: String,
    #[serde(default)]
    dir: String,
}

/// Sortable columns, in the order the header row offers them.
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
        "source" => items.sort_by_cached_key(|item| item.source.as_str().to_owned()),
        "size" => items.sort_by_cached_key(|item| item.size),
        "state" => items.sort_by_cached_key(|item| item.state.as_str().to_owned()),
        _ => items.sort_by_cached_key(|item| item.created_at.unwrap_or(0)),
    }
    if desc {
        items.reverse();
    }

    let peers = store.list_peers().await.unwrap_or_default();
    let active: Vec<Peer> = peers.into_iter().filter(|p| !p.is_revoked()).collect();

    // Computed once for the whole page, not per row: every seeding torrent
    // announces to the same live endpoint, so there is exactly one answer to
    // "where does this instance's tracker currently reach". `None` when
    // nothing is configured to announce to yet — the same condition that
    // blocks the tracker itself (`TorrentError::NoAdvertisedHost`).
    let announce_url = current_announce_url(&state.serve).await;
    // Same reasoning: one current token for the whole instance, hashed once
    // and compared against each row's own stored fingerprint.
    let current_token_fp = state
        .serve
        .tracker_token()
        .await
        .map(|token| crate::sync::fingerprint(&token));

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
                    "?source={}&state={}&q={}&sort={field}&dir={next_dir}",
                    urlencode(&query.source),
                    urlencode(&query.state),
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
        items: items
            .iter()
            .map(|item| {
                row(
                    item,
                    &active,
                    announce_url.as_deref(),
                    current_token_fp.as_deref(),
                )
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
        source_filter: query.source,
        state_filter: query.state,
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

fn row(
    item: &SharedItem,
    peers: &[Peer],
    announce_url: Option<&str>,
    current_token_fp: Option<&str>,
) -> ItemRow {
    ItemRow {
        title: item.spec.title().to_owned(),
        release_title: item.release_title.clone(),
        arr_path: item.arr_path.display().to_string(),
        kind: item.spec.kind_tag(),
        source_label: title_case(item.source.as_str()),
        size: human_size(item.size),
        state_label: title_case(item.state.as_str()),
        state_hint: state_hint(item.state),
        visible_to: visible_to(item, peers),
        since: item.created_at.map(ago).unwrap_or_default(),
        info_hash: item.info_hash.clone(),
        // A torrent with no info hash has not been built yet, so there is
        // nothing meaningful to announce either — `None` regardless of
        // whether the tracker itself is configured.
        announce_url: item.info_hash.as_ref().and(announce_url).map(str::to_owned),
        token_fp: item.announce_token_fp.clone(),
        token_status: token_status(item, current_token_fp),
        ids: ids_summary(&item.ids),
        last_error: item.last_error.clone(),
        created_by_sharerr: item.created_by_sharerr,
        since_absolute: item
            .created_at
            .map(super::peers::absolute)
            .unwrap_or_default(),
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

/// Whether this item's last-confirmed announce token still matches the one
/// currently configured. See [`crate::sync::token_fingerprint`] for how each
/// side is derived.
fn token_status(item: &SharedItem, current_token_fp: Option<&str>) -> TokenStatus {
    // No torrent, nothing to have confirmed yet — not the same condition as a
    // torrent that *was* confirmed and has since drifted.
    if item.info_hash.is_none() {
        return TokenStatus::None;
    }
    match (item.announce_token_fp.as_deref(), current_token_fp) {
        (None, None) => TokenStatus::None,
        (Some(stored), Some(current)) if stored == current => TokenStatus::Valid,
        // Either it changed, or nothing has confirmed this item since a token
        // was first configured (or removed) — both are "not confirmed as
        // current", which is exactly what red is for.
        _ => TokenStatus::Stale,
    }
}

/// The announce URL a freshly built torrent would carry right now, with the
/// token itself replaced by a `<token>` placeholder: the same construction
/// `BuiltinTracker::announce_set` uses, computed live rather than stored so
/// it always reflects whatever the endpoint currently resolves to, but never
/// rendering a live secret to the page — the token's own fingerprint is
/// shown separately per row via [`token_status`]. `None` when nothing is
/// configured to announce to yet.
async fn current_announce_url(state: &crate::state::ServeState) -> Option<String> {
    let base = state.endpoint().current()?;
    let url = sharerr_torrent::announce_url(&base, None).ok()?;
    match state.tracker_token().await {
        Some(_) => Some(format!("{url}/<token>")),
        None => Some(url.to_string()),
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use sharerr_core::MediaSpec;

    fn item(source: MediaSource, spec: MediaSpec, state: ShareState) -> SharedItem {
        SharedItem {
            id: None,
            source,
            source_id: 1,
            file_id: 1,
            spec,
            release_title: "Some.Release.Title".to_owned(),
            arr_path: "/x".into(),
            size: 1_610_612_736, // 1.5 GiB
            ids: sharerr_core::ExternalIds::default(),
            info_hash: None,
            announce_token_fp: None,
            created_by_sharerr: true,
            state,
            last_error: None,
            created_at: None,
        }
    }

    fn peer(label: &str, scope: PeerScope) -> Peer {
        Peer {
            id: 1,
            label: label.to_owned(),
            created_at: 0,
            last_seen_at: None,
            revoked_at: None,
            scope,
            pubkey: None,
            gossip_url: None,
            key_hash: "hash".to_owned(),
        }
    }

    #[test]
    fn sizes_render_in_the_natural_unit() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1_610_612_736), "1.5 GiB");
    }

    #[test]
    fn only_seeding_items_show_anyone_as_able_to_see_them() {
        let it = item(
            MediaSource::Sonarr,
            MediaSpec::Episode {
                series_title: "X".to_owned(),
                season: 1,
                episode: 1,
            },
            ShareState::Pending,
        );
        assert_eq!(visible_to(&it, &[peer("Sam", PeerScope::All)]), "");
    }

    #[test]
    fn a_narrow_scope_admits_a_directory_item_by_declared_kind() {
        let it = item(
            MediaSource::Directory,
            MediaSpec::Movie {
                title: "X".to_owned(),
                year: None,
            },
            ShareState::Seeding,
        );
        assert!(scope_admits(PeerScope::Movies, &it));
        assert!(!scope_admits(PeerScope::Tv, &it));
    }

    #[test]
    fn a_narrow_scope_excludes_a_source_it_does_not_cover() {
        let it = item(
            MediaSource::Radarr,
            MediaSpec::Movie {
                title: "X".to_owned(),
                year: None,
            },
            ShareState::Seeding,
        );
        assert_eq!(
            visible_to(&it, &[peer("Sam", PeerScope::Tv)]),
            "no friend's scope covers it"
        );
        assert_eq!(visible_to(&it, &[peer("Sam", PeerScope::Movies)]), "Sam");
    }

    fn seeding_with_hash(hash: &str) -> SharedItem {
        SharedItem {
            info_hash: Some(hash.to_owned()),
            state: ShareState::Seeding,
            ..item(
                MediaSource::Sonarr,
                MediaSpec::Episode {
                    series_title: "X".to_owned(),
                    season: 1,
                    episode: 1,
                },
                ShareState::Seeding,
            )
        }
    }

    #[test]
    fn no_torrent_yet_is_not_the_same_as_a_stale_token() {
        let pending = item(
            MediaSource::Sonarr,
            MediaSpec::Movie {
                title: "X".to_owned(),
                year: None,
            },
            ShareState::Pending,
        );
        assert_eq!(
            token_status(&pending, Some("current")),
            TokenStatus::None,
            "nothing has been confirmed yet, which is not the same as having drifted"
        );
    }

    #[test]
    fn a_matching_fingerprint_is_valid() {
        let mut it = seeding_with_hash("aa".repeat(20).as_str());
        it.announce_token_fp = Some("abc123".to_owned());
        assert_eq!(token_status(&it, Some("abc123")), TokenStatus::Valid);
    }

    #[test]
    fn a_different_fingerprint_is_stale() {
        let mut it = seeding_with_hash("aa".repeat(20).as_str());
        it.announce_token_fp = Some("old".to_owned());
        assert_eq!(token_status(&it, Some("new")), TokenStatus::Stale);
    }

    /// A token that was configured and then removed (or vice versa) must not
    /// silently read as valid just because both sides happen to differ from
    /// "the same string".
    #[test]
    fn a_token_that_appeared_or_disappeared_is_stale_not_none() {
        let mut it = seeding_with_hash("aa".repeat(20).as_str());
        it.announce_token_fp = Some("abc123".to_owned());
        assert_eq!(token_status(&it, None), TokenStatus::Stale);

        let mut it = seeding_with_hash("bb".repeat(20).as_str());
        it.announce_token_fp = None;
        assert_eq!(token_status(&it, Some("abc123")), TokenStatus::Stale);
    }

    #[test]
    fn only_pending_and_unshared_get_a_hint() {
        assert!(state_hint(ShareState::Pending).is_some());
        assert!(state_hint(ShareState::Unshared).is_some());
        assert!(state_hint(ShareState::Seeding).is_none());
        assert!(state_hint(ShareState::Failed).is_none());
    }

    // -------------------------------------------------------------- page()

    fn web_state(serve: std::sync::Arc<crate::state::ServeState>) -> WebState {
        WebState {
            serve,
            sessions: std::sync::Arc::new(crate::web::auth::Sessions::default()),
        }
    }

    fn named(source: MediaSource, title: &str, file_id: i64) -> SharedItem {
        let spec = MediaSpec::Movie {
            title: title.to_owned(),
            year: None,
        };
        SharedItem {
            file_id,
            ..item(source, spec, ShareState::Seeding)
        }
    }

    async fn body_of(response: Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    /// The search box above the table matches on `release_title`, so a row that
    /// does not show it leaves the operator filtering against a string they
    /// cannot read. `arr_path` is the first thing to check when an item will
    /// not share, and was previously only visible as one sample on `/`.
    #[test]
    fn a_row_carries_the_release_title_and_the_arr_path() {
        let mut source_item = item(
            MediaSource::Sonarr,
            MediaSpec::Movie {
                title: "Harborlight".to_owned(),
                year: Some(2019),
            },
            ShareState::Seeding,
        );
        source_item.release_title = "Harborlight.2019.2160p-SYNTH".to_owned();
        source_item.arr_path = "/data/movies/Harborlight (2019)/Harborlight.mkv".into();

        let row = row(&source_item, &[], None, None);

        assert_eq!(row.release_title, "Harborlight.2019.2160p-SYNTH");
        assert_eq!(
            row.arr_path,
            "/data/movies/Harborlight (2019)/Harborlight.mkv"
        );
        // The two are deliberately different strings — conflating them stalls
        // seeding at 0%, which is why both are worth showing side by side.
        assert_ne!(row.title, row.release_title);
    }

    /// Withdrawing an item sharerr did not create leaves the torrent alone, so
    /// which case a row is in changes what the operator should expect.
    #[test]
    fn a_row_says_whether_sharerr_created_the_torrent() {
        let spec = MediaSpec::Movie {
            title: "Harborlight".to_owned(),
            year: None,
        };

        let mine = item(MediaSource::Radarr, spec.clone(), ShareState::Seeding);
        assert!(row(&mine, &[], None, None).created_by_sharerr);

        let reused = SharedItem {
            created_by_sharerr: false,
            ..item(MediaSource::Radarr, spec, ShareState::Seeding)
        };
        assert!(!row(&reused, &[], None, None).created_by_sharerr);
    }

    #[tokio::test]
    async fn an_empty_store_renders_rather_than_erroring() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        let response = page(State(state), Query(ItemsQuery::default())).await;

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let html = body_of(response).await;
        assert!(html.contains('0'), "{html}");
    }

    #[tokio::test]
    async fn every_stored_item_is_listed_by_default() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let store = serve.store().await.unwrap();
        store
            .upsert(&named(MediaSource::Sonarr, "Lanternwick Hollow", 1))
            .await
            .unwrap();
        store
            .upsert(&named(MediaSource::Radarr, "Harborlight", 2))
            .await
            .unwrap();
        let state = web_state(serve);

        let html = body_of(page(State(state), Query(ItemsQuery::default())).await).await;
        assert!(html.contains("Lanternwick Hollow"), "{html}");
        assert!(html.contains("Harborlight"), "{html}");
    }

    #[tokio::test]
    async fn filtering_by_source_narrows_the_list() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let store = serve.store().await.unwrap();
        store
            .upsert(&named(MediaSource::Sonarr, "Lanternwick Hollow", 1))
            .await
            .unwrap();
        store
            .upsert(&named(MediaSource::Radarr, "Harborlight", 2))
            .await
            .unwrap();
        let state = web_state(serve);

        let html = body_of(
            page(
                State(state),
                Query(ItemsQuery {
                    source: "radarr".to_owned(),
                    ..Default::default()
                }),
            )
            .await,
        )
        .await;

        assert!(html.contains("Harborlight"), "{html}");
        assert!(!html.contains("Lanternwick Hollow"), "{html}");
    }

    /// The tally describes the library, not the current view — a filtered
    /// count could not answer "how much of what I have is actually seeding",
    /// which is the question the page exists for.
    #[tokio::test]
    async fn the_state_tally_survives_a_filter() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let store = serve.store().await.unwrap();
        store
            .upsert(&named(MediaSource::Sonarr, "Lanternwick Hollow", 1))
            .await
            .unwrap();
        store
            .upsert(&SharedItem {
                state: ShareState::Failed,
                ..named(MediaSource::Radarr, "Harborlight", 2)
            })
            .await
            .unwrap();
        let state = web_state(serve);

        let html = body_of(
            page(
                State(state),
                Query(ItemsQuery {
                    source: "sonarr".to_owned(),
                    ..Default::default()
                }),
            )
            .await,
        )
        .await;

        // Only the Sonarr row is listed...
        assert!(!html.contains("Harborlight"), "{html}");
        // ...but the filtered-out Failed item is still counted in the tally.
        assert!(html.contains("1 Seeding"), "{html}");
        assert!(html.contains("1 Failed"), "{html}");
    }

    #[tokio::test]
    async fn the_free_text_search_matches_the_title() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let store = serve.store().await.unwrap();
        store
            .upsert(&named(MediaSource::Sonarr, "Lanternwick Hollow", 1))
            .await
            .unwrap();
        store
            .upsert(&named(MediaSource::Radarr, "Harborlight", 2))
            .await
            .unwrap();
        let state = web_state(serve);

        let html = body_of(
            page(
                State(state),
                Query(ItemsQuery {
                    q: "harbor".to_owned(),
                    ..Default::default()
                }),
            )
            .await,
        )
        .await;

        assert!(html.contains("Harborlight"), "{html}");
        assert!(!html.contains("Lanternwick Hollow"), "{html}");
    }

    #[tokio::test]
    async fn filtering_by_state_narrows_the_list() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let store = serve.store().await.unwrap();
        store
            .upsert(&SharedItem {
                state: ShareState::Failed,
                ..named(MediaSource::Sonarr, "Broken Show", 1)
            })
            .await
            .unwrap();
        store
            .upsert(&named(MediaSource::Radarr, "Harborlight", 2))
            .await
            .unwrap();
        let state = web_state(serve);

        let html = body_of(
            page(
                State(state),
                Query(ItemsQuery {
                    state: "failed".to_owned(),
                    ..Default::default()
                }),
            )
            .await,
        )
        .await;

        assert!(html.contains("Broken Show"), "{html}");
        assert!(!html.contains("Harborlight"), "{html}");
    }

    #[tokio::test]
    async fn sorting_by_title_ascending_orders_the_rows() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let store = serve.store().await.unwrap();
        store
            .upsert(&named(MediaSource::Sonarr, "Zebra", 1))
            .await
            .unwrap();
        store
            .upsert(&named(MediaSource::Radarr, "Apple", 2))
            .await
            .unwrap();
        let state = web_state(serve);

        let html = body_of(
            page(
                State(state),
                Query(ItemsQuery {
                    sort: "title".to_owned(),
                    dir: "asc".to_owned(),
                    ..Default::default()
                }),
            )
            .await,
        )
        .await;

        let apple_at = html.find("Apple").expect("Apple listed");
        let zebra_at = html.find("Zebra").expect("Zebra listed");
        assert!(apple_at < zebra_at, "{html}");
    }

    /// The header row is `sort_links` plus four fixed columns, and the body
    /// row is nine cells — so a `sort_links` of any other length silently
    /// renders every header against the wrong column. `commands::preview`
    /// shipped exactly that bug by hand-writing three of the five, which is
    /// why this asserts on the constant rather than on one page render.
    #[test]
    fn the_sortable_columns_plus_the_fixed_ones_match_the_body_row() {
        const FIXED_HEADERS: usize = 4; // Visible to, Info hash, Announce URL, Token
        const BODY_CELLS: usize = 9;

        assert_eq!(
            SORT_COLUMNS.len() + FIXED_HEADERS,
            BODY_CELLS,
            "items.html's header count must match its body row"
        );
    }
}
