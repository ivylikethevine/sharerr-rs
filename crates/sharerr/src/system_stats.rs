//! CPU, memory, and disk usage — sampled by a background loop and read
//! cheaply by the status page's stat tiles.
//!
//! `_stat_tiles.html`'s own doc comment states the design constraint this
//! follows: the tile grid is worth polling because rendering it costs no
//! I/O of its own. Reading `/proc` and the filesystem on every poll would
//! break that, so a sample is instead taken on a timer and handlers only
//! ever read the last one — the same shape [`crate::gluetun::GluetunStatus`]
//! already uses for the same reason.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use sysinfo::{Disk, Disks, System};

use crate::state::ServeState;

/// How often the background loop resamples.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// What the sampler last measured, for a handler to read with no I/O of its
/// own. `None` before the first tick has completed.
#[derive(Debug, Default)]
pub struct SystemStatus {
    inner: tokio::sync::RwLock<Option<SystemSnapshot>>,
}

/// One sample. `disk_used`/`disk_total` are `None` when no mounted
/// filesystem was found covering the data directory — not expected in the
/// documented Docker deployment, but rendered as an honest blank rather than
/// a wrong number if it happens.
#[derive(Debug, Clone, Copy)]
pub struct SystemSnapshot {
    pub cpu_percent: f32,
    pub memory_used: u64,
    pub memory_total: u64,
    pub disk_used: Option<u64>,
    pub disk_total: Option<u64>,
}

impl SystemStatus {
    async fn record(&self, snapshot: SystemSnapshot) {
        *self.inner.write().await = Some(snapshot);
    }

    /// The most recent sample.
    pub async fn snapshot(&self) -> Option<SystemSnapshot> {
        *self.inner.read().await
    }
}

/// Resample CPU, memory, and disk usage forever, recording each sample into
/// `state`'s [`SystemStatus`]. Never returns.
pub async fn poll_loop(state: Arc<ServeState>) {
    let status = state.system_status();
    let mut sys = System::new();
    let data_dir = state.data_dir().await;
    // The one disk the page reports. Resolved once, off the runtime, and only that
    // disk refreshed per sample: re-listing means re-reading the mount table and a
    // statvfs on every mount, which a hung network mount would stall.
    let mut disk = None;

    loop {
        let dir = data_dir.clone();
        let held = disk.take();
        let refreshed = tokio::task::spawn_blocking(move || refresh_disk(held, &dir)).await;
        disk = refreshed.unwrap_or_else(|err| {
            tracing::warn!(error = %err, "could not read the data directory's disk");
            None
        });
        let snapshot = sample(&mut sys, disk.as_ref()).await;
        status.record(snapshot).await;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One sample. Two `refresh_cpu_usage` calls a short gap apart are what
/// sysinfo needs to report a real delta-based percentage rather than `0.0`
/// on a single-shot read — see `sysinfo::MINIMUM_CPU_UPDATE_INTERVAL`.
async fn sample(sys: &mut System, disk: Option<&Disk>) -> SystemSnapshot {
    sys.refresh_cpu_usage();
    tokio::time::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL).await;
    sys.refresh_cpu_usage();
    let cpu_percent = sys.global_cpu_usage();

    sys.refresh_memory();
    let memory_used = sys.used_memory();
    let memory_total = sys.total_memory();

    let (disk_used, disk_total) = disk.map_or((None, None), |disk| {
        let total = disk.total_space();
        let used = total.saturating_sub(disk.available_space());
        (Some(used), Some(total))
    });

    SystemSnapshot {
        cpu_percent,
        memory_used,
        memory_total,
        disk_used,
        disk_total,
    }
}

/// Pre-render a sample for the status page's stat tiles. The template does no
/// arithmetic of its own — same convention as every other computed figure on
/// that page.
pub fn format(snapshot: SystemSnapshot) -> (String, String, Option<String>) {
    let cpu_percent = format!("{:.1}%", snapshot.cpu_percent);
    let memory_usage = format!(
        "{} of {}",
        crate::web::items::human_size(snapshot.memory_used),
        crate::web::items::human_size(snapshot.memory_total)
    );
    let disk_usage = match (snapshot.disk_used, snapshot.disk_total) {
        (Some(used), Some(total)) => Some(format!(
            "{} of {}",
            crate::web::items::human_size(used),
            crate::web::items::human_size(total)
        )),
        _ => None,
    };
    (cpu_percent, memory_usage, disk_usage)
}

/// `held` brought up to date, or the disk covering `path` when there is none yet.
/// Blocking; [`poll_loop`] runs it off the runtime.
fn refresh_disk(held: Option<Disk>, path: &Path) -> Option<Disk> {
    if let Some(mut disk) = held {
        disk.refresh();
        return Some(disk);
    }
    disk_covering(Vec::from(Disks::new_with_refreshed_list()), path)
}

/// The disk covering `path` — the mount point that is the longest prefix of
/// `path` among everything sysinfo enumerated, the same "most specific mount
/// wins" rule `/proc/mounts` itself follows. `None` if nothing matched.
fn disk_covering(disks: Vec<Disk>, path: &Path) -> Option<Disk> {
    disks
        .into_iter()
        .filter(|d| path.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(disk_used: Option<u64>, disk_total: Option<u64>) -> SystemSnapshot {
        SystemSnapshot {
            cpu_percent: 12.34,
            memory_used: 4 * 1024 * 1024 * 1024,
            memory_total: 16 * 1024 * 1024 * 1024,
            disk_used,
            disk_total,
        }
    }

    #[test]
    fn format_renders_cpu_to_one_decimal() {
        let (cpu, _, _) = format(snapshot(None, None));
        assert_eq!(cpu, "12.3%");
    }

    #[test]
    fn format_renders_memory_as_used_of_total() {
        let (_, memory, _) = format(snapshot(None, None));
        assert_eq!(memory, "4.0 GiB of 16.0 GiB");
    }

    #[test]
    fn format_renders_disk_when_a_mount_was_found() {
        let (_, _, disk) = format(snapshot(
            Some(120 * 1024 * 1024 * 1024),
            Some(500 * 1024 * 1024 * 1024),
        ));
        assert_eq!(disk.as_deref(), Some("120.0 GiB of 500.0 GiB"));
    }

    /// The honest-blank case: no mount covered the data directory, so the
    /// tile must not fabricate a "0 B of 0 B" that reads as a real answer.
    #[test]
    fn format_leaves_disk_blank_when_nothing_matched() {
        let (_, _, disk) = format(snapshot(None, None));
        assert_eq!(disk, None);
    }

    /// Local host introspection, not network/container/database — the one
    /// live-environment check in this module, since `sample`'s actual
    /// sysinfo/statvfs calls have no fake to stand in for them. `/tmp`
    /// always resolves to *some* mounted filesystem on every platform this
    /// runs on.
    #[tokio::test]
    async fn sample_reads_real_memory_and_a_disk_covering_tmp() {
        let mut sys = System::new();
        let disk = refresh_disk(None, std::env::temp_dir().as_path());
        let snapshot = sample(&mut sys, disk.as_ref()).await;

        assert!(snapshot.memory_total > 0, "a host always has some memory");
        assert!(
            snapshot.memory_used <= snapshot.memory_total,
            "used must not exceed total"
        );
        assert!(
            snapshot.disk_total.is_some(),
            "/tmp must resolve to a mounted filesystem"
        );
    }
}
