//! Process and filesystem saturation, read from `/proc/self` and `statvfs`.
//!
//! The VM runs no node_exporter and the collector reports only on itself, so
//! nothing else can answer "is the bot running out of anything?". This module
//! makes the process answer for itself.
//!
//! Every instrument here is *observable*: the SDK invokes the callback during
//! collection, so each sample is taken at the moment it is exported instead of
//! on a timer of its own that could drift away from the export interval.
//!
//! The names are OpenTelemetry's own conventions rather than `nocturnal.*` —
//! these describe a process, not this bot, and any standard dashboard should
//! read them without translation.

use std::path::PathBuf;

use opentelemetry::metrics::{ObservableCounter, ObservableGauge};
use opentelemetry::{global, KeyValue};

use crate::{attr, metric};

/// Live handles for the observable instruments. The callbacks stop when this
/// is dropped, so it must be kept for as long as the process is to be
/// measured — hold it next to the `TelemetryGuard`.
pub struct ProcessMetrics {
    _cpu: ObservableCounter<f64>,
    _rss: ObservableGauge<u64>,
    _virtual: ObservableGauge<u64>,
    _fds: ObservableGauge<u64>,
    _threads: ObservableGauge<u64>,
    _uptime: ObservableGauge<f64>,
    _filesystem: ObservableGauge<u64>,
}

impl ProcessMetrics {
    /// Install the callbacks. `data_dir` is the event store's directory — the
    /// filesystem reported is whichever one actually holds the WAL, not `/`.
    pub fn install(data_dir: impl Into<PathBuf>) -> ProcessMetrics {
        let meter = global::meter("nocturnal");
        let data_dir = data_dir.into();

        let cpu = meter
            .f64_observable_counter(metric::PROCESS_CPU_TIME)
            .with_unit("s")
            .with_callback(|observer| {
                if let Some(stat) = read_stat() {
                    observer.observe(
                        stat.utime_s,
                        &[KeyValue::new(attr::PROCESS_CPU_STATE, "user")],
                    );
                    observer.observe(
                        stat.stime_s,
                        &[KeyValue::new(attr::PROCESS_CPU_STATE, "system")],
                    );
                }
            })
            .build();

        let rss = meter
            .u64_observable_gauge(metric::PROCESS_MEMORY_USAGE)
            .with_unit("By")
            .with_callback(|observer| {
                if let Some(mem) = read_statm() {
                    observer.observe(mem.resident_bytes, &[]);
                }
            })
            .build();

        let virtual_size = meter
            .u64_observable_gauge(metric::PROCESS_MEMORY_VIRTUAL)
            .with_unit("By")
            .with_callback(|observer| {
                if let Some(mem) = read_statm() {
                    observer.observe(mem.virtual_bytes, &[]);
                }
            })
            .build();

        let fds = meter
            .u64_observable_gauge(metric::PROCESS_OPEN_FILE_DESCRIPTORS)
            .with_unit("{count}")
            .with_callback(|observer| {
                if let Some(n) = open_fds() {
                    observer.observe(n, &[]);
                }
            })
            .build();

        let threads = meter
            .u64_observable_gauge(metric::PROCESS_THREAD_COUNT)
            .with_unit("{thread}")
            .with_callback(|observer| {
                if let Some(stat) = read_stat() {
                    observer.observe(stat.threads, &[]);
                }
            })
            .build();

        let uptime = meter
            .f64_observable_gauge(metric::PROCESS_UPTIME)
            .with_unit("s")
            .with_callback(|observer| {
                if let Some(secs) = uptime_seconds() {
                    observer.observe(secs, &[]);
                }
            })
            .build();

        let filesystem = meter
            .u64_observable_gauge(metric::SYSTEM_FILESYSTEM_USAGE)
            .with_unit("By")
            .with_callback(move |observer| {
                let Some(fs) = filesystem_bytes(&data_dir) else {
                    return;
                };
                let mount = KeyValue::new(
                    attr::SYSTEM_FILESYSTEM_MOUNTPOINT,
                    data_dir.display().to_string(),
                );
                observer.observe(
                    fs.used,
                    &[
                        KeyValue::new(attr::SYSTEM_FILESYSTEM_STATE, "used"),
                        mount.clone(),
                    ],
                );
                observer.observe(
                    fs.free,
                    &[KeyValue::new(attr::SYSTEM_FILESYSTEM_STATE, "free"), mount],
                );
            })
            .build();

        ProcessMetrics {
            _cpu: cpu,
            _rss: rss,
            _virtual: virtual_size,
            _fds: fds,
            _threads: threads,
            _uptime: uptime,
            _filesystem: filesystem,
        }
    }
}

struct Stat {
    utime_s: f64,
    stime_s: f64,
    threads: u64,
    /// Clock ticks since boot at which the process started.
    starttime_ticks: u64,
}

/// Parse `/proc/self/stat`. Field 2 (`comm`) is an arbitrary string wrapped in
/// parentheses and may itself contain spaces and `)`, so everything is counted
/// from after the *last* `)` rather than by splitting the whole line.
fn read_stat() -> Option<Stat> {
    let raw = std::fs::read_to_string("/proc/self/stat").ok()?;
    let rest = raw.rsplit_once(')')?.1;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // `rest` begins at field 3 (`state`), so proc(5)'s field N is at N - 3.
    let field = |n: usize| fields.get(n - 3)?.parse::<u64>().ok();
    let hz = rustix::param::clock_ticks_per_second() as f64;
    Some(Stat {
        utime_s: field(14)? as f64 / hz,
        stime_s: field(15)? as f64 / hz,
        threads: field(20)?,
        starttime_ticks: field(22)?,
    })
}

struct Mem {
    virtual_bytes: u64,
    resident_bytes: u64,
}

/// `/proc/self/statm` is "size resident shared text lib data dt", in pages.
fn read_statm() -> Option<Mem> {
    let raw = std::fs::read_to_string("/proc/self/statm").ok()?;
    let mut fields = raw.split_whitespace();
    let page = rustix::param::page_size() as u64;
    let size: u64 = fields.next()?.parse().ok()?;
    let resident: u64 = fields.next()?.parse().ok()?;
    Some(Mem {
        virtual_bytes: size * page,
        resident_bytes: resident * page,
    })
}

fn open_fds() -> Option<u64> {
    // Counting the directory costs one open plus the entries — the fd for the
    // read itself is included, which is a constant 1 and not worth correcting.
    Some(std::fs::read_dir("/proc/self/fd").ok()?.count() as u64)
}

fn uptime_seconds() -> Option<f64> {
    let stat = read_stat()?;
    let raw = std::fs::read_to_string("/proc/uptime").ok()?;
    let since_boot: f64 = raw.split_whitespace().next()?.parse().ok()?;
    let hz = rustix::param::clock_ticks_per_second() as f64;
    Some((since_boot - stat.starttime_ticks as f64 / hz).max(0.0))
}

struct FilesystemBytes {
    used: u64,
    free: u64,
}

fn filesystem_bytes(path: &std::path::Path) -> Option<FilesystemBytes> {
    let vfs = rustix::fs::statvfs(path).ok()?;
    let block = vfs.f_frsize;
    Some(FilesystemBytes {
        used: (vfs.f_blocks - vfs.f_bfree) * block,
        // What is available to *us*, not counting the root reserve — the
        // honest number for "how much runway does the WAL have".
        free: vfs.f_bavail * block,
    })
}

#[cfg(test)]
mod tests {
    use super::{filesystem_bytes, open_fds, read_stat, read_statm, uptime_seconds};

    /// `comm` may contain spaces and parentheses (the thread name is
    /// attacker-adjacent: it is whatever the binary was renamed to). Naive
    /// whitespace splitting misreads every field after it.
    #[test]
    fn stat_fields_survive_a_comm_with_spaces() {
        let stat = read_stat().expect("/proc/self/stat parses");
        assert!(
            stat.threads >= 1,
            "a running process has at least one thread"
        );
        assert!(stat.utime_s >= 0.0);
    }

    #[test]
    fn memory_is_nonzero_and_rss_fits_in_the_address_space() {
        let mem = read_statm().expect("/proc/self/statm parses");
        assert!(
            mem.resident_bytes > 0,
            "a running process has resident pages"
        );
        assert!(
            mem.resident_bytes <= mem.virtual_bytes,
            "rss {} exceeded vsize {}",
            mem.resident_bytes,
            mem.virtual_bytes
        );
    }

    #[test]
    fn fds_and_uptime_read() {
        assert!(open_fds().expect("fd count") > 0);
        assert!(uptime_seconds().expect("uptime") >= 0.0);
    }

    #[test]
    fn filesystem_reports_a_plausible_split() {
        let dir = std::env::temp_dir();
        let fs = filesystem_bytes(&dir).expect("statvfs on the temp dir");
        assert!(fs.used > 0, "a mounted filesystem has used bytes");
    }
}
