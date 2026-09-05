//! Reconciliation run history — one row per sync pass, when it started and
//! finished, and what it did.

use sqlx::Row;

use sharerr_core::endpoint::now_epoch;

use crate::db::{Store, StoreError};

type Result<T> = std::result::Result<T, StoreError>;

impl Store {
    /// Open a reconciliation run, returning its id for [`Self::finish_run`].
    pub async fn begin_run(&self) -> Result<i64> {
        let row = sqlx::query("INSERT INTO sync_runs (started_at) VALUES (?1) RETURNING id")
            .bind(now_epoch())
            .fetch_one(self.pool())
            .await?;
        Ok(row.try_get::<i64, _>("id")?)
    }

    /// Close a run and record what it did.
    pub async fn finish_run(&self, run_id: i64, summary: &RunSummary) -> Result<()> {
        sqlx::query(
            "UPDATE sync_runs SET finished_at = ?1, discovered = ?2, added = ?3, \
             unshared = ?4, failed = ?5, error = ?6 WHERE id = ?7",
        )
        .bind(now_epoch())
        .bind(summary.discovered)
        .bind(summary.added)
        .bind(summary.unshared)
        .bind(summary.failed)
        .bind(summary.error.as_deref())
        .bind(run_id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// The most recent runs, newest first. Also the readiness probe's cheap proof
    /// that the database is answering.
    pub async fn recent_runs(&self, limit: i64) -> Result<Vec<RunRecord>> {
        let rows = sqlx::query(
            "SELECT id, started_at, finished_at, discovered, added, unshared, failed, error \
             FROM sync_runs ORDER BY id DESC LIMIT ?1",
        )
        .bind(limit)
        .fetch_all(self.pool())
        .await?;

        rows.iter()
            .map(|r| {
                Ok(RunRecord {
                    id: r.try_get("id")?,
                    started_at: r.try_get("started_at")?,
                    finished_at: r.try_get("finished_at")?,
                    summary: RunSummary {
                        discovered: r.try_get("discovered")?,
                        added: r.try_get("added")?,
                        unshared: r.try_get("unshared")?,
                        failed: r.try_get("failed")?,
                        error: r.try_get("error")?,
                    },
                })
            })
            .collect()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
/// What one reconciliation pass changed.
pub struct RunSummary {
    pub discovered: i64,
    pub added: i64,
    pub unshared: i64,
    pub failed: i64,
    pub error: Option<String>,
}

impl RunSummary {
    /// How this pass reads to an operator, and whether it should be marked as a
    /// failure.
    ///
    /// An outright error replaces the counts — there is nothing useful to say
    /// about a pass that did not finish. Otherwise only the counts worth acting
    /// on appear, so a quiet pass renders as an empty string rather than three
    /// zeroes.
    ///
    /// Lives beside the data because two pages render it: the status page's
    /// one-glance line and the run history on Diagnostics. They differ only in
    /// whether the discovered count leads, which is what `with_discovered` picks
    /// — when they were two copies they could disagree about the same run.
    pub fn describe(&self, with_discovered: bool) -> (String, bool) {
        if let Some(error) = &self.error {
            return (error.clone(), true);
        }

        let mut parts = Vec::new();
        if with_discovered {
            parts.push(format!("{} discovered", self.discovered));
        }
        if self.added > 0 {
            parts.push(format!("{} added", self.added));
        }
        if self.unshared > 0 {
            parts.push(format!("{} unshared", self.unshared));
        }
        if self.failed > 0 {
            parts.push(format!("{} failed", self.failed));
        }
        (parts.join(", "), self.failed > 0)
    }
}

#[derive(Debug, Clone)]
/// A completed run as stored, with its timing.
pub struct RunRecord {
    pub id: i64,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub summary: RunSummary,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[tokio::test]
    async fn sync_runs_record_a_summary_newest_first() {
        let store = Store::open_in_memory().await.unwrap();

        let first = store.begin_run().await.unwrap();
        let summary = RunSummary {
            discovered: 12,
            added: 3,
            unshared: 1,
            failed: 0,
            error: None,
        };
        store.finish_run(first, &summary).await.unwrap();

        let second = store.begin_run().await.unwrap();

        let runs = store.recent_runs(10).await.unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].id, second);
        assert!(
            runs[0].finished_at.is_none(),
            "an in-flight run has no finish time"
        );
        assert_eq!(runs[1].id, first);
        assert!(runs[1].finished_at.is_some());
        assert_eq!(runs[1].summary, summary);
    }

    #[tokio::test]
    async fn recent_runs_honours_its_limit() {
        let store = Store::open_in_memory().await.unwrap();
        for _ in 0..5 {
            store.begin_run().await.unwrap();
        }
        assert_eq!(store.recent_runs(2).await.unwrap().len(), 2);
    }
}
