//! Host memory reading: `/proc/meminfo`, with the available figure clamped to
//! the headroom of every cgroup-v2 `memory.max` above this process and the free
//! swap figure clamped to every `memory.swap.max` headroom (an unreadable swap
//! limit counts as no free swap), plus the memory pressure-stall reading.

use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Memory {
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_free_bytes: u64,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum MemoryErr {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Invalid(String),
}

type Result<T> = std::result::Result<T, MemoryErr>;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Pressure {
    pub some_avg10: f64,
    pub full_avg10: f64,
}

fn parse_pressure(text: &str) -> Option<Pressure> {
    let avg10 = |kind: &str| -> Option<f64> {
        text.lines()
            .find_map(|line| line.strip_prefix(kind)?.strip_prefix(' '))?
            .split_whitespace()
            .find_map(|field| field.strip_prefix("avg10="))?
            .parse()
            .ok()
    };
    Some(Pressure {
        some_avg10: avg10("some")?,
        full_avg10: avg10("full")?,
    })
}

/// The kernel's memory stall averages, or `None` where PSI is off or unreadable.
pub(crate) fn pressure() -> Option<Pressure> {
    pressure_from(Path::new("/proc"))
}

fn pressure_from(proc_root: &Path) -> Option<Pressure> {
    parse_pressure(&std::fs::read_to_string(proc_root.join("pressure/memory")).ok()?)
}

/// `limit - usage` for one cgroup directory; `None` when there is no numeric limit.
fn headroom(path: &Path, limit: &str, usage: &str) -> Result<Option<u64>> {
    let limit = match std::fs::read_to_string(path.join(limit)) {
        Ok(limit) => limit,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if limit.trim() == "max" {
        return Ok(None);
    }
    let current = std::fs::read_to_string(path.join(usage))?;
    let parse = |raw: &str| {
        raw.trim()
            .parse::<u64>()
            .map_err(|_| MemoryErr::Invalid("invalid cgroup memory value".into()))
    };
    Ok(Some(parse(&limit)?.saturating_sub(parse(&current)?)))
}

pub(crate) fn parse_meminfo(text: &str) -> Result<Memory> {
    let read = |key: &str| -> Result<u64> {
        text.lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                (name == key).then_some(value)
            })
            .and_then(|value| value.trim().strip_suffix(" kB"))
            .and_then(|value| value.parse::<u64>().ok())
            .and_then(|value| value.checked_mul(1024))
            .ok_or_else(|| MemoryErr::Invalid(format!("missing or invalid {key} in meminfo")))
    };
    Ok(Memory {
        total_bytes: read("MemTotal")?,
        available_bytes: read("MemAvailable")?,
        swap_total_bytes: read("SwapTotal")?,
        swap_free_bytes: read("SwapFree")?,
    })
}

/// Both `available_bytes` and `swap_free_bytes` are clamped to cgroup headroom.
pub(crate) fn sample() -> Result<Memory> {
    sample_from(Path::new("/proc"), Path::new("/sys/fs/cgroup"))
}

fn sample_from(proc_root: &Path, cgroup_root: &Path) -> Result<Memory> {
    let mut memory = parse_meminfo(&std::fs::read_to_string(proc_root.join("meminfo"))?)?;
    let cgroups = std::fs::read_to_string(proc_root.join("self/cgroup"))?;
    if let Some(group) = cgroups.lines().find_map(|line| line.strip_prefix("0::")) {
        let relative = Path::new(group.trim_start_matches('/'));
        if relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(MemoryErr::Invalid("invalid cgroup path".into()));
        }
        let path = cgroup_root.join(relative);
        for path in path
            .ancestors()
            .take_while(|path| path.starts_with(cgroup_root))
        {
            if let Some(headroom) = headroom(path, "memory.max", "memory.current")? {
                memory.available_bytes = memory.available_bytes.min(headroom);
            }
            match headroom(path, "memory.swap.max", "memory.swap.current") {
                Ok(Some(headroom)) => memory.swap_free_bytes = memory.swap_free_bytes.min(headroom),
                Ok(None) => {}
                Err(_) => memory.swap_free_bytes = 0,
            }
        }
    }
    Ok(memory)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meminfo_requires_available_and_converts_kib() {
        assert_eq!(
            parse_meminfo(
                "MemTotal: 100 kB\nMemAvailable: 20 kB\nSwapTotal: 50 kB\nSwapFree: 30 kB\n"
            )
            .unwrap(),
            Memory {
                total_bytes: 102400,
                available_bytes: 20480,
                swap_total_bytes: 51200,
                swap_free_bytes: 30720,
            }
        );
        assert!(parse_meminfo("MemTotal: 100 kB\n").is_err());
    }

    const MEMINFO: &str =
        "MemTotal: 1000 kB\nMemAvailable: 800 kB\nSwapTotal: 500 kB\nSwapFree: 400 kB\n";

    /// A fake `/proc` and cgroup root with the process in `/a/b`.
    fn fake_host() -> tempfile::TempDir {
        let host = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(host.path().join("proc/self")).unwrap();
        std::fs::create_dir_all(host.path().join("cgroup/a/b")).unwrap();
        std::fs::write(host.path().join("proc/meminfo"), MEMINFO).unwrap();
        std::fs::write(host.path().join("proc/self/cgroup"), "0::/a/b\n").unwrap();
        host
    }

    fn write(host: &tempfile::TempDir, file: &str, text: &str) {
        std::fs::write(host.path().join("cgroup").join(file), text).unwrap();
    }

    fn sampled(host: &tempfile::TempDir) -> Memory {
        sample_from(&host.path().join("proc"), &host.path().join("cgroup")).unwrap()
    }

    #[test]
    fn swap_is_capped_by_the_tightest_cgroup_headroom() {
        let host = fake_host();
        assert_eq!(sampled(&host).swap_free_bytes, 400 * 1024);
        write(&host, "a/memory.swap.max", "300000\n");
        write(&host, "a/memory.swap.current", "100000\n");
        write(&host, "a/b/memory.swap.max", "250000\n");
        write(&host, "a/b/memory.swap.current", "100000\n");
        assert_eq!(sampled(&host).swap_free_bytes, 150_000);
        write(&host, "a/b/memory.swap.max", "max\n");
        assert_eq!(sampled(&host).swap_free_bytes, 200_000);
        write(&host, "a/memory.swap.current", "999999\n");
        assert_eq!(sampled(&host).swap_free_bytes, 0);
    }

    #[test]
    fn unlimited_or_absent_swap_limits_leave_swap_free_unchanged() {
        let host = fake_host();
        write(&host, "a/memory.swap.max", "max\n");
        write(&host, "a/b/memory.swap.max", "max\n");
        assert_eq!(sampled(&host).swap_free_bytes, 400 * 1024);
    }

    #[test]
    fn unreadable_or_garbage_swap_limits_count_as_no_free_swap() {
        let host = fake_host();
        std::fs::create_dir(host.path().join("cgroup/a/b/memory.swap.max")).unwrap();
        let memory = sampled(&host);
        assert_eq!(memory.swap_free_bytes, 0);
        assert_eq!(memory.available_bytes, 800 * 1024);
        std::fs::remove_dir(host.path().join("cgroup/a/b/memory.swap.max")).unwrap();
        write(&host, "a/b/memory.swap.max", "garbage\n");
        write(&host, "a/b/memory.swap.current", "0\n");
        assert_eq!(sampled(&host).swap_free_bytes, 0);
        write(&host, "a/b/memory.swap.max", "1000\n");
        write(&host, "a/b/memory.swap.current", "x\n");
        assert_eq!(sampled(&host).swap_free_bytes, 0);
    }

    #[test]
    fn memory_max_still_caps_available_bytes() {
        let host = fake_host();
        write(&host, "a/b/memory.max", "500000\n");
        write(&host, "a/b/memory.current", "200000\n");
        let memory = sampled(&host);
        assert_eq!(memory.available_bytes, 300_000);
        assert_eq!(memory.swap_free_bytes, 400 * 1024);
        write(&host, "a/b/memory.current", "bad\n");
        assert!(
            sample_from(&host.path().join("proc"), &host.path().join("cgroup")).is_err(),
            "a bad memory.max sample still fails"
        );
    }

    #[test]
    fn pressure_reads_avg10_from_both_lines_or_none() {
        let text = "some avg10=1.25 avg60=0.50 avg300=0.10 total=123\nfull avg10=0.75 avg60=0.20 avg300=0.05 total=45\n";
        assert_eq!(
            parse_pressure(text),
            Some(Pressure {
                some_avg10: 1.25,
                full_avg10: 0.75
            })
        );
        assert_eq!(
            parse_pressure("some avg10=1.25 avg60=0 avg300=0 total=1\n"),
            None
        );
        assert_eq!(parse_pressure(""), None);
        let host = tempfile::tempdir().unwrap();
        assert_eq!(pressure_from(host.path()), None);
        std::fs::create_dir(host.path().join("pressure")).unwrap();
        std::fs::write(host.path().join("pressure/memory"), text).unwrap();
        assert_eq!(pressure_from(host.path()).unwrap().full_avg10, 0.75);
    }
}
