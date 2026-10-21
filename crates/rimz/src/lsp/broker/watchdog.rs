//! Machine-lock election and least-used-server pressure relief.

use super::{memory, registry};
use crate::lsp::Result;
use crate::proc::memory::{Memory, Pressure};
use registry::StopReason;
use std::time::Duration;

fn pressured(sample: Memory, floor: u64) -> bool {
    memory::free_bytes(sample.available_bytes, sample.swap_free_bytes) < floor
}

pub(super) fn check(policy: &crate::config::LspConfig) -> Result<()> {
    let sample = memory::sample()?;
    let floor = crate::lsp::admission::reserve_bytes(policy, sample.total_bytes)?;
    if !pressured(sample, floor) {
        return Ok(());
    }
    let Some(_lock) = crate::disk::lock::WorkspaceLock::try_acquire(
        &crate::disk::paths::lsp_runtime_dir().join("admission.lock"),
    )?
    else {
        return Ok(());
    };
    let mut entries = registry::read_entries()?;
    entries.retain(|entry| entry.state.is_running() && entry.broker_is_alive());
    registry::kill_order(&mut entries);
    for mut entry in entries {
        let before = memory::sample()?;
        if !pressured(before, floor) {
            break;
        }
        let victim_rss_kb = entry
            .server_pid
            .and_then(crate::proc::tree_totals)
            .map_or(0, |totals| totals.rss_kb);
        entry.peak_rss_kb = entry
            .peak_rss_kb
            .max(entry.server_pid.map_or(0, memory::tree_peak_kb));
        stop_victim(&entry, StopReason::MemoryPressure, true)?;
        crate::diag::lsp::append(&kill_record(
            &entry,
            before,
            memory::sample()?,
            victim_rss_kb,
            floor,
            crate::proc::memory::pressure(),
        ));
    }
    Ok(())
}

pub(in crate::lsp) fn stop_victim(
    entry: &registry::Entry,
    reason: StopReason,
    kill_fallback: bool,
) -> Result<bool> {
    let started = std::time::Instant::now();
    let response = registry::request(
        entry,
        &serde_json::json!({"op": "stop", "reason": reason}),
        Duration::from_secs(2),
    );
    let acknowledged = response.is_ok_and(|response| response["ok"] == true);
    let (Some(pid), Some(token)) = (entry.server_pid, entry.server_start_token.as_deref()) else {
        return Ok(acknowledged);
    };
    if acknowledged && entry.broker_pid != std::process::id() {
        let grace = if kill_fallback {
            Duration::from_secs(2)
        } else {
            Duration::from_secs(3)
        };
        while started.elapsed() < grace && crate::proc::process_is_live(pid, Some(token)) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    if kill_fallback && crate::proc::process_is_live(pid, Some(token)) {
        match nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(pid as i32),
            nix::sys::signal::Signal::SIGKILL,
        ) {
            Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
            Err(error) => return Err(std::io::Error::from(error).into()),
        }
        while started.elapsed() < Duration::from_secs(3)
            && crate::proc::process_is_live(pid, Some(token))
        {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    Ok(!crate::proc::process_is_live(pid, Some(token)))
}

fn kill_record(
    entry: &registry::Entry,
    before: Memory,
    after: Memory,
    victim_rss_kb: u64,
    reserve_bytes: u64,
    pressure: Option<Pressure>,
) -> crate::diag::lsp::Record {
    crate::diag::lsp::Record {
        at: jiff::Timestamp::now(),
        root: entry.root.clone(),
        server: entry.server.clone(),
        event: "killed".into(),
        details: serde_json::json!({"reason": StopReason::MemoryPressure, "peak_rss_kb": entry.peak_rss_kb, "free_before_bytes": before.available_bytes, "free_after_bytes": after.available_bytes, "swap_free_before_bytes": before.swap_free_bytes, "swap_free_after_bytes": after.swap_free_bytes, "victim_rss_kb": victim_rss_kb, "reserve_bytes": reserve_bytes, "pressure_some_avg10": pressure.map(|p| p.some_avg10), "pressure_full_avg10": pressure.map(|p| p.full_avg10)}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory(available_bytes: u64, swap_free_bytes: u64) -> Memory {
        Memory {
            total_bytes: 1000,
            available_bytes,
            swap_total_bytes: 1000,
            swap_free_bytes,
        }
    }

    #[test]
    fn trigger_counts_swap_toward_the_floor() {
        assert!(!pressured(memory(150, 0), 100));
        assert!(!pressured(memory(100, 0), 100));
        assert!(pressured(memory(99, 0), 100));
        assert!(
            !pressured(memory(40, 60), 100),
            "RAM short, RAM + swap enough"
        );
        assert!(pressured(memory(40, 59), 100));
        assert!(!pressured(memory(u64::MAX, 1), u64::MAX));
    }

    #[test]
    fn killed_record_carries_pressure_samples_and_victim_rss() {
        let entry = serde_json::from_value(serde_json::json!({"root": "/checkout", "server": "rust", "nonce": "n", "broker_pid": 1, "broker_start_token": "t", "server_pid": null, "server_start_token": null, "state": "ready", "started_at_ms": 0, "ready_at_ms": null, "estimate_bytes": 0, "settings_hash": "s", "request_count": 0, "last_request_at_ms": null, "peak_rss_kb": 500, "leases": []})).unwrap();
        let pressure = Pressure {
            some_avg10: 1.5,
            full_avg10: 0.25,
        };
        let record = kill_record(
            &entry,
            memory(100, 7),
            memory(200, 9),
            400,
            150,
            Some(pressure),
        );
        assert_eq!(record.event, "killed");
        assert_eq!(
            record.details,
            serde_json::json!({"reason": "memory pressure", "peak_rss_kb": 500, "free_before_bytes": 100, "free_after_bytes": 200, "swap_free_before_bytes": 7, "swap_free_after_bytes": 9, "victim_rss_kb": 400, "reserve_bytes": 150, "pressure_some_avg10": 1.5, "pressure_full_avg10": 0.25})
        );
        let record = kill_record(&entry, memory(100, 7), memory(200, 9), 400, 150, None);
        assert!(record.details["pressure_some_avg10"].is_null());
        assert!(record.details["pressure_full_avg10"].is_null());
        assert_eq!(record.details.as_object().unwrap().len(), 10);
    }
}
