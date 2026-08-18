//! Readarr discovery: which book files carry the sharerr tag.
//!
//! The simplest of the walks. Tags live on the *author*, so tagging one shares
//! their whole catalogue — the same series-level surprise Sonarr and Lidarr have —
//! and Readarr's unit of import is a `bookFile`, one per book, with none of the
//! multi-track ambiguity music has.
//!
//! Readarr is on API `v1`. Note also that upstream Readarr is no longer actively
//! maintained; this walk is deliberately conservative about which fields it needs,
//! so a fork that drifts slightly still works.

use futures::stream::{self, StreamExt, TryStreamExt};
use serde::Deserialize;
use sharerr_core::{ExternalIds, MediaSource, MediaSpec};

use crate::client::ArrClient;
use crate::error::Result;
use crate::models::non_empty;
use crate::{DISCOVERY_CONCURRENCY, Discovered};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Author {
    id: i64,
    #[serde(default)]
    author_name: String,
    #[serde(default)]
    tags: Vec<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Book {
    id: i64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    foreign_book_id: Option<String>,
    #[serde(default)]
    editions: Vec<Edition>,
}

/// One published edition. The ISBN lives here rather than on the book, because a
/// work can have many.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Edition {
    #[serde(default)]
    isbn13: Option<String>,
    #[serde(default)]
    monitored: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BookFile {
    id: i64,
    #[serde(default)]
    book_id: i64,
    #[serde(default)]
    path: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    scene_name: Option<String>,
}

/// What one tagged author needs fetching for it. Fetched by id and zipped back
/// onto the author list by the caller — see `sonarr::SeriesPayload` for why.
type AuthorPayload = (Vec<Book>, Vec<BookFile>);

async fn fetch_author(client: &ArrClient, author_id: i64) -> Result<AuthorPayload> {
    let author_id = author_id.to_string();

    // Independent lookups, so they run concurrently — per tagged author this
    // halves the round trips paid in sequence.
    let by_author = [("authorId", author_id)];
    let (books, files) = tokio::try_join!(
        client.get::<Vec<Book>>("book", &by_author),
        client.get::<Vec<BookFile>>("bookfile", &by_author),
    )?;
    Ok((books, files))
}

pub(crate) async fn discover(client: &ArrClient, tag_id: i64) -> Result<Vec<Discovered>> {
    let authors: Vec<Author> = client.get("author", &[]).await?;
    let tagged: Vec<&Author> = authors
        .iter()
        .filter(|a| a.tags.contains(&tag_id))
        .collect();

    tracing::debug!(
        total = authors.len(),
        tagged = tagged.len(),
        "readarr authors scanned for the sharerr tag"
    );

    // Concurrent across authors as well as within one — see
    // `DISCOVERY_CONCURRENCY`.
    let ids: Vec<i64> = tagged.iter().map(|a| a.id).collect();
    let fetched: Vec<AuthorPayload> = stream::iter(ids)
        .map(|id| fetch_author(client, id))
        .buffered(DISCOVERY_CONCURRENCY)
        .try_collect()
        .await?;

    let mut discovered = Vec::new();
    for (author, (books, files)) in tagged.into_iter().zip(fetched) {
        if files.is_empty() {
            tracing::debug!(author = %author.author_name, "tagged but has no files on disk");
            continue;
        }

        // Indexed once, then each file's lookup is O(1).
        let book_by_id: std::collections::HashMap<i64, &Book> =
            books.iter().map(|b| (b.id, b)).collect();

        for file in files {
            let Some(book) = book_by_id.get(&file.book_id).copied() else {
                tracing::warn!(
                    author = %author.author_name,
                    file_id = file.id,
                    "book file belongs to no listed book; skipping"
                );
                continue;
            };

            let isbn = pick_isbn(&book.editions);

            discovered.push(Discovered {
                source: MediaSource::Readarr,
                source_id: author.id,
                file_id: file.id,
                spec: MediaSpec::Book {
                    author: author.author_name.clone(),
                    title: book.title.clone(),
                },
                arr_path: file.path.clone().into(),
                size: file.size,
                ids: ExternalIds {
                    goodreads: non_empty(book.foreign_book_id.clone()),
                    isbn,
                    ..ExternalIds::default()
                },
                scene_name: non_empty(file.scene_name.clone()),
            });
        }
    }

    Ok(discovered)
}

/// The monitored edition first — that is the one the operator actually has —
/// falling back to any edition that carries an ISBN at all.
fn pick_isbn(editions: &[Edition]) -> Option<String> {
    editions
        .iter()
        .find(|e| e.monitored && e.isbn13.is_some())
        .or_else(|| editions.iter().find(|e| e.isbn13.is_some()))
        .and_then(|e| non_empty(e.isbn13.clone()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn edition(isbn: Option<&str>, monitored: bool) -> Edition {
        Edition {
            isbn13: isbn.map(str::to_owned),
            monitored,
        }
    }

    /// The monitored edition is the one the operator actually holds, so its ISBN is
    /// the one a friend should match on.
    #[test]
    fn the_monitored_edition_supplies_the_isbn() {
        let editions = vec![
            edition(Some("9780000000001"), false),
            edition(Some("9780000000002"), true),
        ];
        assert_eq!(pick_isbn(&editions).as_deref(), Some("9780000000002"));
    }

    /// A book with no monitored edition still has a usable id — better an ISBN from
    /// another edition than none at all.
    #[test]
    fn an_unmonitored_edition_is_better_than_no_isbn() {
        let editions = vec![edition(None, true), edition(Some("9780000000003"), false)];
        assert_eq!(pick_isbn(&editions).as_deref(), Some("9780000000003"));
    }

    #[test]
    fn a_book_with_no_isbn_anywhere_reports_none() {
        assert_eq!(pick_isbn(&[edition(None, true)]), None);
    }
}
