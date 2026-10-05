//! Host memory reading: `/proc/meminfo`, with the available figure clamped to
//! the headroom of every cgroup-v2 `memory.max` above this process.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Memory {
    pub total_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum MemoryErr {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Invalid(String),
}

type Result<T> = std::result::Result<T, MemoryErr>;

fn parse_meminfo(text: &str) -> Result<Memory> {
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
    })
}

pub(crate) fn sample() -> Result<Memory> {
    let mut memory = parse_meminfo(&std::fs::read_to_string("/proc/meminfo")?)?;
    let cgroups = std::fs::read_to_string("/proc/self/cgroup")?;
    if let Some(group) = cgroups.lines().find_map(|line| line.strip_prefix("0::")) {
        let root = std::path::Path::new("/sys/fs/cgroup");
        let relative = std::path::Path::new(group.trim_start_matches('/'));
        if relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(MemoryErr::Invalid("invalid cgroup path".into()));
        }
        let path = root.join(relative);
        for path in path.ancestors().take_while(|path| path.starts_with(root)) {
            let maximum = match std::fs::read_to_string(path.join("memory.max")) {
                Ok(maximum) => maximum,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            if maximum.trim() == "max" {
                continue;
            }
            let current = std::fs::read_to_string(path.join("memory.current"))?;
            let parse = |raw: &str| {
                raw.trim()
                    .parse::<u64>()
                    .map_err(|_| MemoryErr::Invalid("invalid cgroup memory value".into()))
            };
            memory.available_bytes = memory
                .available_bytes
                .min(parse(&maximum)?.saturating_sub(parse(&current)?));
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
            parse_meminfo("MemTotal: 100 kB\nMemAvailable: 20 kB\n").unwrap(),
            Memory {
                total_bytes: 102400,
                available_bytes: 20480
            }
        );
        assert!(parse_meminfo("MemTotal: 100 kB\n").is_err());
    }
}
