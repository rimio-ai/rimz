//! Memory accounting for admission and the watchdog: server process trees and
//! the host reading from `proc::memory`.

use super::Result;

pub fn tree_peak_kb(pid: u32) -> u64 {
    tree_peak_kb_with(pid, crate::proc::tree_peak_rss_kb, |pid| {
        crate::proc::tree_totals(pid).map(|totals| totals.rss_kb)
    })
}

fn tree_peak_kb_with(
    pid: u32,
    peak: impl FnOnce(u32) -> Option<u64>,
    rss: impl FnOnce(u32) -> Option<u64>,
) -> u64 {
    peak(pid).unwrap_or(0).max(rss(pid).unwrap_or(0))
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
            (Some(100), Some(200), 200),
            (None, Some(100), 100),
            (Some(100), None, 100),
            (None, None, 0),
        ] {
            assert_eq!(tree_peak_kb_with(1, |_| peak, |_| rss), expected);
        }
    }
}
