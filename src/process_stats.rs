//! Resource usage of the current process and the cgroup it runs in.
//!
//! Linux-only by design: everything is read from `/proc` and
//! `/sys/fs/cgroup`. Elsewhere (e.g. macOS during development) every field is
//! `None`. The file parsing is factored into pure functions so it stays
//! testable on any platform.
//!
//! Memory is read per process rather than from cgroup `memory.current`,
//! because the latter includes reclaimable page cache. Anonymous memory is what
//! drives an OOM kill, and full RSS does not track it for processes that mmap
//! large files: every page of the mapping they touch counts as resident, but
//! the kernel drops those under pressure instead of killing anything. Hence
//! [`ProcessStats::anon_rss_bytes`] next to the full RSS. The cgroup is resolved
//! from `/proc/self/cgroup`, so this works both inside a container (where the
//! cgroup root is the container's) and for a systemd service on a VM.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Kernel clock ticks per second for `/proc/<pid>/stat` times. This is the
/// userspace ABI constant `USER_HZ`, which is 100 on every architecture
/// Toolforge and its VMs run on; reading it via `sysconf` would need `libc`.
const USER_HZ: f64 = 100.0;

/// cgroup v1 reports "no limit" as a huge page-aligned number; anything at or
/// above this is treated as unlimited.
const NO_LIMIT_SENTINEL: u64 = 1 << 53;

/// One reading. Fields are `None` when the platform or cgroup setup does not
/// provide them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProcessStats {
    /// Resident set size of this process, in bytes, including pages of
    /// mmap'd files.
    pub rss_bytes: Option<u64>,
    /// Resident memory the kernel cannot reclaim without swap (`RssAnon` +
    /// `RssShmem`: heap, stacks, shared memory), in bytes. `None` on kernels
    /// before 4.5, which lack the breakdown.
    pub anon_rss_bytes: Option<u64>,
    /// Memory limit of the process's cgroup, in bytes.
    pub memory_limit_bytes: Option<u64>,
    /// CPU time (user + system) used by this process so far, in seconds.
    pub cpu_seconds: Option<f64>,
    /// CPU limit of the cgroup, in cores.
    pub cpu_limit_cores: Option<f64>,
    /// Number of threads of this process.
    pub threads: Option<u64>,
    /// Number of open file descriptors of this process.
    pub open_fds: Option<u64>,
    /// OOM kills in the cgroup so far (`memory.events`). In a container that
    /// was restarted in place this starts again from zero.
    pub oom_kills: Option<u64>,
}

impl ProcessStats {
    /// Reads the current values.
    pub fn read() -> Self {
        let status = std::fs::read_to_string("/proc/self/status").ok();
        let cgroup = cgroup_dir();
        let read_cgroup = |file: &str| {
            cgroup
                .as_ref()
                .and_then(|dir| std::fs::read_to_string(dir.join(file)).ok())
        };
        Self {
            rss_bytes: status
                .as_deref()
                .and_then(|s| status_field(s, "VmRSS:"))
                .map(kib_to_bytes),
            anon_rss_bytes: status.as_deref().and_then(parse_anon_rss).map(kib_to_bytes),
            memory_limit_bytes: memory_limit(cgroup.as_deref()),
            cpu_seconds: std::fs::read_to_string("/proc/self/stat")
                .ok()
                .and_then(|raw| parse_cpu_seconds(&raw)),
            cpu_limit_cores: read_cgroup("cpu.max").and_then(|raw| parse_cpu_max(&raw)),
            threads: status.as_deref().and_then(|s| status_field(s, "Threads:")),
            open_fds: std::fs::read_dir("/proc/self/fd")
                .ok()
                .map(|dir| dir.count() as u64),
            oom_kills: read_cgroup("memory.events").and_then(|raw| parse_oom_kills(&raw)),
        }
    }
}

/// Memory readings only: cheaper than [`ProcessStats::read`], for checking
/// memory pressure often (e.g. before taking on more work). Reads
/// `/proc/self/status`; the cgroup memory limit is resolved once and cached.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MemoryStats {
    /// As [`ProcessStats::rss_bytes`].
    pub rss_bytes: Option<u64>,
    /// As [`ProcessStats::anon_rss_bytes`].
    pub anon_rss_bytes: Option<u64>,
    /// As [`ProcessStats::memory_limit_bytes`].
    pub limit_bytes: Option<u64>,
}

impl MemoryStats {
    /// Reads the current values.
    pub fn read() -> Self {
        static LIMIT: OnceLock<Option<u64>> = OnceLock::new();
        let status = std::fs::read_to_string("/proc/self/status").ok();
        Self {
            rss_bytes: status
                .as_deref()
                .and_then(|s| status_field(s, "VmRSS:"))
                .map(kib_to_bytes),
            anon_rss_bytes: status.as_deref().and_then(parse_anon_rss).map(kib_to_bytes),
            limit_bytes: *LIMIT.get_or_init(|| memory_limit(cgroup_dir().as_deref())),
        }
    }

    /// Full RSS as a fraction of the limit; `None` without both. Above `1.0`
    /// is possible: page cache of mmap'd files counts towards RSS.
    pub fn used_fraction(&self) -> Option<f64> {
        let limit = self.limit_bytes.filter(|limit| *limit > 0)?;
        Some(self.rss_bytes? as f64 / limit as f64)
    }
}

/// The memory limit of the cgroup at `cgroup` (v2), else the v1 limit.
fn memory_limit(cgroup: Option<&Path>) -> Option<u64> {
    cgroup
        .and_then(|dir| std::fs::read_to_string(dir.join("memory.max")).ok())
        .or_else(|| std::fs::read_to_string("/sys/fs/cgroup/memory/memory.limit_in_bytes").ok())
        .and_then(|raw| parse_memory_limit(&raw))
}

/// The cgroup v2 directory of this process, if it exists.
fn cgroup_dir() -> Option<PathBuf> {
    let raw = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let dir = Path::new("/sys/fs/cgroup").join(parse_cgroup_path(&raw)?);
    dir.is_dir().then_some(dir)
}

/// The cgroup v2 path (the `0::<path>` line) of a `/proc/self/cgroup` body,
/// relative to the cgroup root.
fn parse_cgroup_path(raw: &str) -> Option<&str> {
    raw.lines()
        .find_map(|line| line.strip_prefix("0::"))
        .map(|path| path.trim().trim_start_matches('/'))
}

/// A numeric field (first token after the label) of a `/proc/self/status` body.
fn status_field(raw: &str, label: &str) -> Option<u64> {
    raw.lines()
        .find_map(|line| line.strip_prefix(label))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|value| value.parse().ok())
}

/// `RssAnon` + `RssShmem` of a `/proc/self/status` body, in KiB.
fn parse_anon_rss(raw: &str) -> Option<u64> {
    let anon = status_field(raw, "RssAnon:")?;
    let shmem = status_field(raw, "RssShmem:").unwrap_or(0);
    Some(anon.saturating_add(shmem))
}

/// `/proc` reports memory in KiB (labelled "kB").
fn kib_to_bytes(kib: u64) -> u64 {
    kib.saturating_mul(1024)
}

/// A cgroup memory limit; `None` for "max", the v1 "unlimited" sentinel, and
/// anything unparseable.
fn parse_memory_limit(raw: &str) -> Option<u64> {
    raw.trim()
        .parse::<u64>()
        .ok()
        .filter(|v| *v > 0 && *v < NO_LIMIT_SENTINEL)
}

/// utime + stime of a `/proc/self/stat` body, in seconds. The command name
/// (field 2) is parenthesised and may contain spaces, so fields are counted
/// from the last `)`: utime and stime are fields 14 and 15 overall, i.e. the
/// 12th and 13th after it.
fn parse_cpu_seconds(raw: &str) -> Option<f64> {
    let after_comm = raw.get(raw.rfind(')')? + 1..)?;
    let mut fields = after_comm.split_whitespace().skip(11);
    let utime: u64 = fields.next()?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    Some(utime.saturating_add(stime) as f64 / USER_HZ)
}

/// Cores allowed by a cgroup v2 `cpu.max` body (`"<quota> <period>"`, or
/// `"max <period>"` for no limit).
fn parse_cpu_max(raw: &str) -> Option<f64> {
    let mut parts = raw.split_whitespace();
    let quota: f64 = parts.next()?.parse().ok()?;
    let period: f64 = parts.next()?.parse().ok()?;
    (period > 0.0).then(|| quota / period)
}

/// The `oom_kill` counter of a cgroup v2 `memory.events` body.
fn parse_oom_kills(raw: &str) -> Option<u64> {
    raw.lines()
        .find_map(|line| line.strip_prefix("oom_kill "))
        .and_then(|value| value.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_cgroup_path() {
        assert_eq!(parse_cgroup_path("0::/\n"), Some(""));
        assert_eq!(
            parse_cgroup_path("0::/system.slice/petscan.service\n"),
            Some("system.slice/petscan.service")
        );
        // cgroup v1 only: no unified hierarchy line.
        assert_eq!(parse_cgroup_path("12:memory:/docker/abc\n"), None);
    }

    #[test]
    fn test_status_fields() {
        let raw = "Name:\tmain\nVmRSS:\t  123456 kB\nThreads:\t17\n";
        assert_eq!(status_field(raw, "VmRSS:"), Some(123456));
        assert_eq!(status_field(raw, "Threads:"), Some(17));
        assert_eq!(status_field(raw, "VmSwap:"), None);
    }

    #[test]
    fn test_parse_anon_rss() {
        let raw = "VmRSS:\t 3000000 kB\nRssAnon:\t  400000 kB\nRssFile:\t 2599000 kB\nRssShmem:\t    1000 kB\n";
        assert_eq!(parse_anon_rss(raw), Some(401000));
        assert_eq!(parse_anon_rss("RssAnon:\t 5 kB\n"), Some(5));
        // Kernels before 4.5 have VmRSS only.
        assert_eq!(parse_anon_rss("VmRSS:\t 3000000 kB\n"), None);
    }

    #[test]
    fn test_parse_memory_limit() {
        assert_eq!(parse_memory_limit("3670016000\n"), Some(3670016000));
        assert_eq!(parse_memory_limit("max\n"), None);
        assert_eq!(parse_memory_limit("9223372036854771712\n"), None);
        assert_eq!(parse_memory_limit("0\n"), None);
    }

    #[test]
    fn test_parse_cpu_seconds() {
        // A command name with spaces and a ')' must not shift the fields.
        let raw = "4242 (my (odd) prog) S 1 4242 4242 0 -1 4194560 100 0 0 0 250 50 0 0 20 0 8 0 1000 0 0";
        assert_eq!(parse_cpu_seconds(raw), Some(3.0));
        assert_eq!(parse_cpu_seconds("garbage"), None);
    }

    #[test]
    fn test_parse_cpu_max() {
        assert_eq!(parse_cpu_max("200000 100000\n"), Some(2.0));
        assert_eq!(parse_cpu_max("max 100000\n"), None);
        assert_eq!(parse_cpu_max("100000 0\n"), None);
    }

    #[test]
    fn test_parse_oom_kills() {
        let raw = "low 0\nhigh 0\nmax 12\noom 3\noom_kill 2\noom_group_kill 0\n";
        assert_eq!(parse_oom_kills(raw), Some(2));
    }

    #[test]
    fn test_read_does_not_panic() {
        // On Linux this reads real values, elsewhere it yields Nones.
        let _ = ProcessStats::read();
        let _ = MemoryStats::read();
    }

    #[test]
    fn test_used_fraction() {
        let stats = |rss, limit| MemoryStats {
            rss_bytes: rss,
            anon_rss_bytes: None,
            limit_bytes: limit,
        };
        assert_eq!(stats(Some(750), Some(1000)).used_fraction(), Some(0.75));
        assert_eq!(stats(Some(750), None).used_fraction(), None);
        assert_eq!(stats(None, Some(1000)).used_fraction(), None);
        assert_eq!(stats(Some(750), Some(0)).used_fraction(), None);
    }
}
