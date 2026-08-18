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
use sharerr_core::{MediaSource, MediaSpec, ShareState, SharedItem};
use sharerr_store::{Peer, PeerScope};

use super::WebState;
use super::peers::ago;
use super::settings::title_case;
use super::templates::{FilterOption, ItemRow, ItemsPage, SortLink, render};

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
const SORT_COLUMNS: &[(&str, &str)] = &[
    ("since", "Since"),
    ("title", "Title"),
    ("source", "Source"),
    ("size", "Size"),
    ("state", "State"),
];

pub async fn page(State(state): State<WebState>, Query(query): Query<ItemsQuery>) -> Response {
    let store = match state.store_or_503().await {
        Ok(store) => store,
        Err(response) => return response,
    };

    let (mut items, error) = match store.all_items().await {
        Ok(items) => (items, None),
        Err(err) => (Vec::new(), Some(format!("could not list items: {err}"))),
    };
    let total = items.len();

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
    let sort = if query.sort.is_empty() {
        "since"
    } else {
        query.sort.as_str()
    };
    let desc = if query.sort.is_empty() {
        true
    } else {
        query.dir == "desc"
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
        items: items.iter().map(|item| row(item, &active)).collect(),
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

fn urlencode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

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
    spec_kind(&item.spec) == kind
}

fn spec_kind(spec: &MediaSpec) -> &'static str {
    match spec {
        MediaSpec::Episode { .. } => "episode",
        MediaSpec::Movie { .. } => "movie",
        MediaSpec::Track { .. } => "track",
        MediaSpec::Book { .. } => "book",
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

fn row(item: &SharedItem, peers: &[Peer]) -> ItemRow {
    ItemRow {
        title: item.spec.title().to_owned(),
        kind: spec_kind(&item.spec),
        source_label: title_case(item.source.as_str()),
        size: human_size(item.size),
        state_label: title_case(item.state.as_str()),
        visible_to: visible_to(item, peers),
        since: item.created_at.map(ago).unwrap_or_default(),
        info_hash_short: item
            .info_hash
            .as_deref()
            .map(|h| format!("{}…", &h[..h.len().min(10)])),
        last_error: item.last_error.clone(),
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
}
