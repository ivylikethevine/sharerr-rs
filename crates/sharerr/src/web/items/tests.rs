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
        media: None,
        info_hash: None,
        announce_token_fp: None,
        created_by_sharerr: true,
        private: true,
        state,
        last_error: None,
        created_at: None,
        achieved_ratio: None,
        ratio_limit_reported: None,
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
        token_status(
            &pending,
            TokenFps {
                current: Some("current"),
                previous: None
            }
        ),
        TokenStatus::None,
        "nothing has been confirmed yet, which is not the same as having drifted"
    );
}

#[test]
fn a_matching_fingerprint_is_valid() {
    let mut it = seeding_with_hash("aa".repeat(20).as_str());
    it.announce_token_fp = Some("abc123".to_owned());
    assert_eq!(
        token_status(
            &it,
            TokenFps {
                current: Some("abc123"),
                previous: None
            }
        ),
        TokenStatus::Valid
    );
}

/// Matching the current token wins even when it also happens to equal the
/// previous one — `Valid` is checked first, so this must not read as
/// `Rotating` just because both comparisons would technically succeed.
#[test]
fn a_fingerprint_matching_both_current_and_previous_is_valid_not_rotating() {
    let mut it = seeding_with_hash("ee".repeat(20).as_str());
    it.announce_token_fp = Some("abc123".to_owned());
    assert_eq!(
        token_status(
            &it,
            TokenFps {
                current: Some("abc123"),
                previous: Some("abc123")
            }
        ),
        TokenStatus::Valid
    );
}

/// The state dual-token admission exists for: an item still on the token
/// a rotation just replaced is genuinely still being served, not dead.
#[test]
fn a_fingerprint_matching_only_the_previous_token_is_rotating() {
    let mut it = seeding_with_hash("cc".repeat(20).as_str());
    it.announce_token_fp = Some("old".to_owned());
    assert_eq!(
        token_status(
            &it,
            TokenFps {
                current: Some("new"),
                previous: Some("old")
            }
        ),
        TokenStatus::Rotating
    );
}

#[test]
fn a_fingerprint_matching_neither_current_nor_previous_is_stale() {
    let mut it = seeding_with_hash("dd".repeat(20).as_str());
    it.announce_token_fp = Some("ancient".to_owned());
    assert_eq!(
        token_status(
            &it,
            TokenFps {
                current: Some("new"),
                previous: Some("old")
            }
        ),
        TokenStatus::Stale
    );
}

#[test]
fn a_different_fingerprint_is_stale() {
    let mut it = seeding_with_hash("aa".repeat(20).as_str());
    it.announce_token_fp = Some("old".to_owned());
    assert_eq!(
        token_status(
            &it,
            TokenFps {
                current: Some("new"),
                previous: None
            }
        ),
        TokenStatus::Stale
    );
}

/// A token that was configured and then removed (or vice versa) must not
/// silently read as valid just because both sides happen to differ from
/// "the same string".
#[test]
fn a_token_that_appeared_or_disappeared_is_stale_not_none() {
    let mut it = seeding_with_hash("aa".repeat(20).as_str());
    it.announce_token_fp = Some("abc123".to_owned());
    assert_eq!(token_status(&it, TokenFps::default()), TokenStatus::Stale);

    let mut it = seeding_with_hash("bb".repeat(20).as_str());
    it.announce_token_fp = None;
    assert_eq!(
        token_status(
            &it,
            TokenFps {
                current: Some("abc123"),
                previous: None
            }
        ),
        TokenStatus::Stale
    );
}

#[test]
fn ratio_cell_is_empty_before_the_client_reports_anything() {
    let it = seeding_with_hash("aa".repeat(20).as_str());
    assert_eq!(ratio_cell(&it), (String::new(), String::new()));
}

#[test]
fn ratio_cell_names_the_clients_own_limit_when_it_reports_one() {
    let mut it = seeding_with_hash("aa".repeat(20).as_str());
    it.achieved_ratio = Some(1.85);
    it.ratio_limit_reported = Some(2.0);
    let (value, hint) = ratio_cell(&it);
    assert_eq!(value, "1.85");
    assert!(hint.contains("2.00"), "{hint}");
}

/// No limit reported still shows the achieved ratio — the honest-blank
/// convention this page follows, not a row that quietly shows nothing.
#[test]
fn ratio_cell_explains_a_missing_limit_without_hiding_the_ratio() {
    let mut it = seeding_with_hash("aa".repeat(20).as_str());
    it.achieved_ratio = Some(0.42);
    it.ratio_limit_reported = None;
    let (value, hint) = ratio_cell(&it);
    assert_eq!(value, "0.42");
    assert!(
        !hint.is_empty(),
        "a missing limit still needs an explanation"
    );
}

#[test]
fn ratio_cell_renders_an_infinite_ratio_as_the_symbol() {
    let mut it = seeding_with_hash("aa".repeat(20).as_str());
    it.achieved_ratio = Some(f64::INFINITY);
    let (value, _) = ratio_cell(&it);
    assert_eq!(value, "∞");
}

#[test]
fn a_swarm_renders_compactly_with_the_long_form_on_hover() {
    let (cell, hint) = peers_cell(Some(SwarmCount {
        complete: 2,
        incomplete: 1,
    }));
    assert_eq!(cell, "2↑ 1↓");
    assert_eq!(hint, "2 seeding · 1 downloading");
    assert_eq!(peers_cell(None), (String::new(), String::new()));
}

#[test]
fn a_short_hash_is_twelve_characters_and_never_panics_on_less() {
    assert_eq!(short_hash(&"ab".repeat(20)), "abababababab");
    assert_eq!(short_hash("abc"), "abc");
}

/// `source_id`/`file_id` are what an operator greps *arr logs for, so
/// they ride along on the source cell's tooltip.
#[test]
fn a_row_names_the_arr_identifiers_in_the_source_hint() {
    let it = SharedItem {
        source_id: 42,
        file_id: 1337,
        ..item(
            MediaSource::Sonarr,
            MediaSpec::Episode {
                series_title: "X".to_owned(),
                season: 1,
                episode: 1,
            },
            ShareState::Seeding,
        )
    };
    let row = row(&it, &[], None, TokenFps::default(), None);
    assert_eq!(row.source_hint, "Sonarr series 42, file 1337");
    assert_eq!(row.info_hash_short, None);
}

#[test]
fn only_pending_and_unshared_get_a_hint() {
    assert!(state_hint(ShareState::Pending).is_some());
    assert!(state_hint(ShareState::Unshared).is_some());
    assert!(state_hint(ShareState::Seeding).is_none());
    assert!(state_hint(ShareState::Failed).is_none());
}

// -------------------------------------------------------------- page()

use super::super::{location, web_state};

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

    let row = row(&source_item, &[], None, TokenFps::default(), None);

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
    assert!(row(&mine, &[], None, TokenFps::default(), None).created_by_sharerr);

    let reused = SharedItem {
        created_by_sharerr: false,
        private: true,
        ..item(MediaSource::Radarr, spec, ShareState::Seeding)
    };
    assert!(!row(&reused, &[], None, TokenFps::default(), None).created_by_sharerr);
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
async fn filtering_by_kind_narrows_the_list() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let store = serve.store().await.unwrap();
    store
        .upsert(&SharedItem {
            spec: MediaSpec::Episode {
                series_title: "Lanternwick Hollow".to_owned(),
                season: 1,
                episode: 1,
            },
            ..named(MediaSource::Sonarr, "unused", 1)
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
                kind: "movie".to_owned(),
                ..Default::default()
            }),
        )
        .await,
    )
    .await;

    assert!(html.contains("Harborlight"), "{html}");
    assert!(!html.contains("Lanternwick Hollow"), "{html}");
    // The sort links carry the kind along so re-sorting keeps the filter.
    assert!(html.contains("kind=movie&#38;q="), "{html}");
}

/// The seeding total describes the library, the shown total the view.
#[tokio::test]
async fn the_summary_totals_the_seeding_size_and_the_shown_size() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let store = serve.store().await.unwrap();
    // Two seeding items at 1.5 GiB each, one failed one that must not count.
    store
        .upsert(&named(MediaSource::Sonarr, "Lanternwick Hollow", 1))
        .await
        .unwrap();
    store
        .upsert(&named(MediaSource::Radarr, "Harborlight", 2))
        .await
        .unwrap();
    store
        .upsert(&SharedItem {
            state: ShareState::Failed,
            ..named(MediaSource::Radarr, "Broken", 3)
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

    assert!(html.contains("3.0 GiB seeding"), "{html}");
    assert!(html.contains("1 of 3"), "{html}");
    assert!(html.contains("1.5 GiB shown"), "{html}");
}

/// A live swarm shows up against its row, matched on the stored hex hash.
#[tokio::test]
async fn a_live_swarm_is_counted_on_its_row() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let store = serve.store().await.unwrap();
    let hash = "ab".repeat(20);
    store
        .upsert(&SharedItem {
            info_hash: Some(hash.clone()),
            ..named(MediaSource::Radarr, "Harborlight", 1)
        })
        .await
        .unwrap();
    store
        .upsert(&SharedItem {
            info_hash: Some("cd".repeat(20)),
            ..named(MediaSource::Radarr, "Lonely", 2)
        })
        .await
        .unwrap();
    let raw = sharerr_torrent::announce::info_hash_from_hex(&hash).unwrap();
    // `left=0` marks a seeder, which is what a friend who finished looks like.
    let request = sharerr_torrent::AnnounceRequest {
        info_hash: raw,
        peer_id: [1; 20],
        port: 6881,
        left: 0,
        event: sharerr_torrent::Event::None,
        compact: true,
        numwant: 50,
        declared_ip: None,
    };
    serve
        .swarms()
        .announce(&request, "203.0.113.1:6881".parse().unwrap())
        .await;
    let state = web_state(serve);

    let html = body_of(page(State(state), Query(ItemsQuery::default())).await).await;
    assert!(html.contains("1↑ 0↓"), "{html}");
    assert!(html.contains("1 seeding · 0 downloading"), "{html}");
    // The first twelve characters are visible; the whole hash is on hover.
    assert!(html.contains(">abababababab<"), "{html}");
    assert!(html.contains(&format!("title=\"{hash}\"")), "{html}");
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

/// The header row is `sort_links` plus six fixed columns, and the body
/// row is eleven cells — so a `sort_links` of any other length silently
/// renders every header against the wrong column. `commands::preview`
/// shipped exactly that bug by hand-writing three of the five, which is
/// why this asserts on the constant rather than on one page render.
#[test]
fn the_sortable_columns_plus_the_fixed_ones_match_the_body_row() {
    const FIXED_HEADERS: usize = 6; // Ratio, Peers, Visible to, Info hash, Announce URL, Token
    const BODY_CELLS: usize = 11;

    assert_eq!(
        SORT_COLUMNS.len() + FIXED_HEADERS,
        BODY_CELLS,
        "items.html's header count must match its body row"
    );
}

// ----------------------------------------------------------- detail page

#[tokio::test]
async fn detail_shows_the_release_title_and_the_file_name_side_by_side() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let store = serve.store().await.unwrap();
    let mut source_item = named(MediaSource::Sonarr, "Lanternwick Hollow", 1);
    source_item.release_title = "Lanternwick.Hollow.S01E01.SYNTH".to_owned();
    source_item.arr_path = "/tv/Lanternwick Hollow/S01E01.mkv".into();
    store.upsert(&source_item).await.unwrap();
    let state = web_state(serve);

    let html = body_of(
        detail(
            State(state),
            Path((MediaSource::Sonarr, 1)),
            Query(DetailQuery::default()),
        )
        .await,
    )
    .await;

    assert!(html.contains("Lanternwick.Hollow.S01E01.SYNTH"), "{html}");
    assert!(html.contains("S01E01.mkv"), "{html}");
}

#[tokio::test]
async fn detail_answers_404_for_an_unknown_item() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let state = web_state(serve);

    let response = detail(
        State(state),
        Path((MediaSource::Sonarr, 999)),
        Query(DetailQuery::default()),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn detail_shows_the_flash_message_from_the_query_string() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let store = serve.store().await.unwrap();
    store
        .upsert(&named(MediaSource::Sonarr, "Lanternwick Hollow", 1))
        .await
        .unwrap();
    let state = web_state(serve);

    let html = body_of(
        detail(
            State(state),
            Path((MediaSource::Sonarr, 1)),
            Query(DetailQuery {
                error: Some("could not reach qBittorrent".to_owned()),
                ..Default::default()
            }),
        )
        .await,
    )
    .await;

    assert!(html.contains("could not reach qBittorrent"), "{html}");
}

// -------------------------------------------------------- manual actions

#[tokio::test]
async fn retry_answers_404_for_an_unknown_item() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let state = web_state(serve);

    let response = retry(State(state), Path((MediaSource::Sonarr, 999))).await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// `unconfigured()`'s syncer never finishes building — see
/// `ServeState::new`'s initial `Err("still starting up")` — so a retry
/// must say so rather than claiming success or panicking.
#[tokio::test]
async fn retry_redirects_with_an_error_when_the_syncer_is_not_ready() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let store = serve.store().await.unwrap();
    let mut source_item = named(MediaSource::Sonarr, "Lanternwick Hollow", 1);
    source_item.state = ShareState::Failed;
    store.upsert(&source_item).await.unwrap();
    let state = web_state(serve);

    let response = retry(State(state), Path((MediaSource::Sonarr, 1))).await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let location = location(&response);
    assert!(location.starts_with("/items/sonarr/1?error="), "{location}");
}

/// A syncer that builds but has nothing to scan bails outright — still
/// the honest outcome to show, not a silent no-op.
#[tokio::test]
async fn retry_redirects_with_an_error_when_nothing_can_be_scanned() {
    let (_dir, serve) = crate::state::fixtures::ready().await;
    let store = serve.store().await.unwrap();
    let mut source_item = named(MediaSource::Sonarr, "Lanternwick Hollow", 1);
    source_item.state = ShareState::Failed;
    store.upsert(&source_item).await.unwrap();
    let state = web_state(serve);

    let response = retry(State(state), Path((MediaSource::Sonarr, 1))).await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let location = location(&response);
    assert!(location.starts_with("/items/sonarr/1?error="), "{location}");
}

#[tokio::test]
async fn rebuild_answers_404_for_an_unknown_item() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let state = web_state(serve);

    let response = rebuild(State(state), Path((MediaSource::Sonarr, 999))).await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// The store-side half of "force rebuild" must take effect even though
/// the immediate sync pass that follows has nothing to scan and errors
/// out — an operator who fixed the file and clicked rebuild should not
/// have the clear silently undone by the pass failing.
#[tokio::test]
async fn rebuild_clears_the_torrent_identity_even_though_the_pass_then_fails() {
    let (_dir, serve) = crate::state::fixtures::ready().await;
    let store = serve.store().await.unwrap();
    let mut source_item = named(MediaSource::Sonarr, "Lanternwick Hollow", 1);
    source_item.info_hash = Some("ab".repeat(20));
    source_item.created_by_sharerr = false; // no client call needed to clear it
    source_item.state = ShareState::Seeding;
    store.upsert(&source_item).await.unwrap();
    let state = web_state(serve);

    let response = rebuild(State(state), Path((MediaSource::Sonarr, 1))).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);

    let cleared = store.get(MediaSource::Sonarr, 1).await.unwrap().unwrap();
    assert_eq!(cleared.state, ShareState::Pending);
    assert_eq!(cleared.info_hash, None);
}

#[tokio::test]
async fn unshare_answers_404_for_an_unknown_item() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let state = web_state(serve);

    let response = unshare(State(state), Path((MediaSource::Sonarr, 999))).await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// Already unshared redirects rather than erroring — same convention
/// `peers::revoke` follows for an already-revoked key.
#[tokio::test]
async fn unsharing_an_already_unshared_item_still_redirects() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let store = serve.store().await.unwrap();
    let mut source_item = named(MediaSource::Sonarr, "Lanternwick Hollow", 1);
    source_item.state = ShareState::Unshared;
    store.upsert(&source_item).await.unwrap();
    let state = web_state(serve);

    let response = unshare(State(state), Path((MediaSource::Sonarr, 1))).await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), "/items/sonarr/1");
}

/// No info hash means no client to call at all — the `(None, _)` arm of
/// `Syncer::unshare_one` — so this exercises the whole handler without
/// needing a real torrent client behind it.
#[tokio::test]
async fn unsharing_a_pending_item_marks_it_unshared() {
    let (_dir, serve) = crate::state::fixtures::ready().await;
    let store = serve.store().await.unwrap();
    let source_item = named(MediaSource::Sonarr, "Lanternwick Hollow", 1);
    store.upsert(&source_item).await.unwrap();
    let state = web_state(serve);

    let response = unshare(State(state), Path((MediaSource::Sonarr, 1))).await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let location = location(&response);
    assert!(location.starts_with("/items/sonarr/1?ok="), "{location}");
    let got = store.get(MediaSource::Sonarr, 1).await.unwrap().unwrap();
    assert_eq!(got.state, ShareState::Unshared);
}

/// A torrent this instance did not add is left running — the
/// `(Some(_), false)` arm — which also needs no working client call.
#[tokio::test]
async fn unsharing_an_adopted_torrent_leaves_it_in_the_client() {
    let (_dir, serve) = crate::state::fixtures::ready().await;
    let store = serve.store().await.unwrap();
    let mut source_item = named(MediaSource::Sonarr, "Lanternwick Hollow", 1);
    source_item.info_hash = Some("ab".repeat(20));
    source_item.created_by_sharerr = false;
    source_item.state = ShareState::Seeding;
    store.upsert(&source_item).await.unwrap();
    let state = web_state(serve);

    let response = unshare(State(state), Path((MediaSource::Sonarr, 1))).await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let got = store.get(MediaSource::Sonarr, 1).await.unwrap().unwrap();
    assert_eq!(got.state, ShareState::Unshared);
}

// ------------------------------------------------------- the long tail

#[test]
fn column_hint_is_empty_for_a_column_that_has_none() {
    assert_eq!(column_hint("nope"), "");
    assert!(!column_hint("state").is_empty());
}

/// A source outside the kind-scoped set is admitted by its own source
/// alone — a narrow scope that does not name it never falls through to
/// the declared-kind check.
#[test]
fn scope_admits_never_reads_a_declared_kind_for_an_arr_source() {
    let it = item(
        MediaSource::Radarr,
        MediaSpec::Movie {
            title: "X".to_owned(),
            year: None,
        },
        ShareState::Seeding,
    );
    assert!(!scope_admits(PeerScope::Tv, &it));
    assert!(scope_admits(PeerScope::Movies, &it));
}

#[tokio::test]
async fn a_row_lists_every_external_id_in_lookup_order() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let store = serve.store().await.unwrap();
    store
        .upsert(&SharedItem {
            ids: sharerr_core::ExternalIds {
                tvdb: Some(1),
                tmdb: Some(2),
                tvmaze: Some(3),
                imdb: Some("tt4".to_owned()),
                musicbrainz: Some("mb5".to_owned()),
                goodreads: Some("gr6".to_owned()),
                isbn: Some("9787".to_owned()),
            },
            ..named(MediaSource::Radarr, "Harborlight", 1)
        })
        .await
        .unwrap();
    let state = web_state(serve);

    let html = body_of(page(State(state), Query(ItemsQuery::default())).await).await;

    for id in [
        "tvdb 1",
        "tmdb 2",
        "tvmaze 3",
        "imdb tt4",
        "musicbrainz mb5",
        "goodreads gr6",
        "isbn 9787",
    ] {
        assert!(html.contains(id), "missing {id} in:\n{html}");
    }
}

#[tokio::test]
async fn the_source_hint_names_an_artist_for_music_and_an_author_for_books() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let store = serve.store().await.unwrap();
    store
        .upsert(&item(
            MediaSource::Lidarr,
            MediaSpec::Track {
                artist: "Quiet Harbour".to_owned(),
                album: "Lanterns".to_owned(),
                track: Some(1),
            },
            ShareState::Seeding,
        ))
        .await
        .unwrap();
    store
        .upsert(&SharedItem {
            file_id: 2,
            ..item(
                MediaSource::Readarr,
                MediaSpec::Book {
                    author: "Mara Vell".to_owned(),
                    title: "The Copper Vale".to_owned(),
                },
                ShareState::Seeding,
            )
        })
        .await
        .unwrap();
    let state = web_state(serve);

    let html = body_of(page(State(state), Query(ItemsQuery::default())).await).await;

    assert!(html.contains("Lidarr artist 1, file 1"), "{html}");
    assert!(html.contains("Readarr author 1, file 2"), "{html}");
}

#[tokio::test]
async fn detail_lists_the_live_swarm_with_masked_peer_addresses() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let store = serve.store().await.unwrap();
    let hash = "ab".repeat(20);
    store
        .upsert(&SharedItem {
            info_hash: Some(hash.clone()),
            ..named(MediaSource::Radarr, "Harborlight", 1)
        })
        .await
        .unwrap();
    let raw = sharerr_torrent::announce::info_hash_from_hex(&hash).unwrap();
    let request = sharerr_torrent::AnnounceRequest {
        info_hash: raw,
        peer_id: [1; 20],
        port: 6881,
        left: 0,
        event: sharerr_torrent::Event::None,
        compact: true,
        numwant: 50,
        declared_ip: None,
    };
    serve
        .swarms()
        .announce(&request, "203.0.113.1:6881".parse().unwrap())
        .await;
    let state = web_state(serve);

    let html = body_of(
        detail(
            State(state),
            Path((MediaSource::Radarr, 1)),
            Query(DetailQuery::default()),
        )
        .await,
    )
    .await;

    assert!(
        html.contains("203.0.113.1:6881"),
        "the full address is on hover:\n{html}"
    );
    assert!(
        html.contains(&super::super::topology::mask_address("203.0.113.1:6881")),
        "{html}"
    );
}

#[tokio::test]
async fn detail_shows_the_announce_url_once_an_endpoint_is_advertised() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let store = serve.store().await.unwrap();
    // Only an item with a torrent has an announce URL worth showing.
    store
        .upsert(&SharedItem {
            info_hash: Some("ab".repeat(20)),
            ..named(MediaSource::Radarr, "Harborlight", 1)
        })
        .await
        .unwrap();
    serve
        .endpoint()
        .observe("http://203.0.113.9:51413".parse().unwrap());
    let state = web_state(serve);

    let html = body_of(
        detail(
            State(state),
            Path((MediaSource::Radarr, 1)),
            Query(DetailQuery::default()),
        )
        .await,
    )
    .await;

    assert!(html.contains("203.0.113.9:51413/announce"), "{html}");
}

/// A relative `arr_path` cannot be resolved at all; the page shows it
/// unmapped rather than pretending to know whether the file exists.
#[tokio::test]
async fn detail_shows_an_unresolvable_path_as_it_was_recorded() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let store = serve.store().await.unwrap();
    store
        .upsert(&SharedItem {
            arr_path: "relative/Harborlight.mkv".into(),
            ..named(MediaSource::Radarr, "Harborlight", 1)
        })
        .await
        .unwrap();
    let state = web_state(serve);

    let response = detail(
        State(state),
        Path((MediaSource::Radarr, 1)),
        Query(DetailQuery::default()),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let html = body_of(response).await;
    assert!(html.contains("relative/Harborlight.mkv"), "{html}");
}

#[tokio::test]
async fn every_item_action_answers_503_when_the_store_will_not_open() {
    let (_dir, serve) = crate::state::fixtures::store_unopenable();
    let state = web_state(serve);
    let at = || Path((MediaSource::Radarr, 1));

    let response = detail(State(state.clone()), at(), Query(DetailQuery::default())).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let response = retry(State(state.clone()), at()).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let response = rebuild(State(state.clone()), at()).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let response = unshare(State(state), at()).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

/// A syncer with a scannable (if empty) source completes the pass, so
/// the redirect carries the pass's own outcome rather than a failure to
/// run it at all.
#[tokio::test]
async fn retry_redirects_with_the_passes_outcome_when_it_completes() {
    let (_dir, serve) = crate::state::fixtures::ready_with_source().await;
    let store = serve.store().await.unwrap();
    let mut source_item = named(MediaSource::Sonarr, "Lanternwick Hollow", 1);
    source_item.state = ShareState::Failed;
    store.upsert(&source_item).await.unwrap();
    let state = web_state(serve);

    let response = retry(State(state), Path((MediaSource::Sonarr, 1))).await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let location = location(&response);
    assert!(location.starts_with("/items/sonarr/1?ok="), "{location}");
}
