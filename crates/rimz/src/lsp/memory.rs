//! Memory accounting for admission and the watchdog: server process trees and
//! the host reading from `proc::memory`.

use super::Result;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Reading {
    pub rss_kb: u64,
    pub swap_kb: u64,
    pub peak_kb: u64,
}

pub fn tree(pid: u32) -> Reading {
    tree_with(pid, crate::proc::tree_memory_kb, |pid| {
        crate::proc::tree_totals(pid).map(|totals| totals.rss_kb)
    })
}

pub fn for_entry(entry: &super::registry::Entry) -> Reading {
    if !entry.state.is_running() {
        return Reading::default();
    }
    entry
        .server_pid
        .filter(|pid| crate::proc::process_is_live(*pid, entry.server_start_token.as_deref()))
        .map_or_else(Reading::default, tree)
}

fn tree_with(
    pid: u32,
    status: impl FnOnce(u32) -> Option<crate::proc::TreeMemory>,
    rss: impl FnOnce(u32) -> Option<u64>,
) -> Reading {
    let status = status(pid).unwrap_or_default();
    let rss_kb = rss(pid).unwrap_or(0);
    Reading {
        rss_kb,
        swap_kb: status.swap_kb,
        peak_kb: status
            .peak_rss_kb
            .max(rss_kb.saturating_add(status.swap_kb)),
    }
}

/// What a server can draw on: available memory plus free swap, both from one `proc::memory`
/// sample and so already clamped to cgroup headroom. Admission and the watchdog compare this
/// figure, never `available_bytes` alone, against the reserve.
pub(super) fn free_bytes(available_bytes: u64, swap_free_bytes: u64) -> u64 {
    available_bytes.saturating_add(swap_free_bytes)
}

pub(super) fn sample() -> Result<crate::proc::memory::Memory> {
    crate::proc::memory::sample().map_err(|error| match error {
        crate::proc::memory::MemoryErr::Io(error) => error.into(),
        crate::proc::memory::MemoryErr::Invalid(message) => super::LspErr::Protocol(message),
    })
}

pub(super) fn raise_oom_score(pid: u32) -> Result<()> {
    std::fs::write(format!("/proc/{pid}/oom_score_adj"), "800")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_peak_uses_high_water_with_sampled_floor() {
        for (peak, rss, expected) in [
            (Some(200), Some(100), 200),
            (Some(100), Some(200), 250),
            (None, Some(100), 100),
            (Some(100), None, 100),
            (None, None, 0),
        ] {
            assert_eq!(
                tree_with(
                    1,
                    |_| peak.map(|peak_rss_kb| crate::proc::TreeMemory {
                        peak_rss_kb,
                        swap_kb: 50
                    }),
                    |_| rss
                ),
                Reading {
                    rss_kb: rss.unwrap_or(0),
                    swap_kb: if peak.is_some() { 50 } else { 0 },
                    peak_kb: expected
                }
            );
        }
    }
}
