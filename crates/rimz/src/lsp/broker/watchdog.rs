//! Machine-lock election and least-used-server pressure relief.

use super::{memory, registry};
use crate::lsp::Result;
use registry::{State, StopReason};
use std::time::Duration;

pub(super) fn check(percent: u8) -> Result<()> {
    let sample = memory::sample()?;
    let floor = crate::lsp::admission::reserve_bytes(sample.total_bytes, percent, 0);
    if sample.available_bytes >= floor {
        return Ok(());
    }
    let Some(_lock) = crate::disk::lock::WorkspaceLock::try_acquire(
        &crate::disk::paths::lsp_runtime_dir().join("admission.lock"),
    )?
    else {
        return Ok(());
    };
    let mut entries = registry::read_entries()?;
    entries.retain(|entry| {
        !matches!(entry.state, State::Stopped { .. })
            && crate::proc::process_is_live(entry.broker_pid, Some(&entry.broker_start_token))
    });
    registry::kill_order(&mut entries);
    for mut entry in entries {
        let free_before = memory::sample()?.available_bytes;
        if free_before >= floor {
            break;
        }
        let victim_rss_kb = entry
            .server_pid
            .and_then(crate::proc::tree_totals)
            .map_or(0, |totals| totals.rss_kb);
        entry.peak_rss_kb = entry.peak_rss_kb.max(victim_rss_kb);
        let response = registry::request(
            &entry,
            &serde_json::json!({"op": "stop", "reason": StopReason::MemoryPressure}),
            Duration::from_secs(2),
        );
        if response.is_ok_and(|response| response["ok"] == true)
            && entry.broker_pid != std::process::id()
        {
            // Give the acknowledged broker time to complete graceful shutdown.
            std::thread::sleep(Duration::from_secs(2));
        }
        if let (Some(pid), Some(token)) = (entry.server_pid, entry.server_start_token.as_deref())
            && crate::proc::process_is_live(pid, Some(token))
        {
            entry.peak_rss_kb = entry
                .peak_rss_kb
                .max(crate::proc::tree_totals(pid).map_or(0, |totals| totals.rss_kb));
            match nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(pid as i32),
                nix::sys::signal::Signal::SIGKILL,
            ) {
                Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
                Err(error) => return Err(std::io::Error::from(error).into()),
            }
            // Memory accounting catches up asynchronously with process death.
            for _ in 0..100 {
                if !crate::proc::process_is_live(pid, Some(token)) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        entry.state = State::Stopped {
            reason: StopReason::MemoryPressure,
            at_ms: crate::utils::time::unix_now_ms(),
        };
        registry::publish(&entry)?;
        crate::diag::lsp::append(&kill_record(
            &entry,
            free_before,
            memory::sample()?.available_bytes,
            victim_rss_kb,
        ));
        if let Some(published) = registry::read_entries()?
            .into_iter()
            .find(|published| published.nonce == entry.nonce)
        {
            super::record_stop(&published);
        }
    }
    Ok(())
}

fn kill_record(
    entry: &registry::Entry,
    free_before: u64,
    free_after: u64,
    victim_rss_kb: u64,
) -> crate::diag::lsp::Record {
    crate::diag::lsp::Record {
        at: jiff::Timestamp::now(),
        root: entry.root.clone(),
        server: entry.server.clone(),
        event: "killed".into(),
        details: serde_json::json!({"reason": StopReason::MemoryPressure, "peak_rss_kb": entry.peak_rss_kb, "free_before_bytes": free_before, "free_after_bytes": free_after, "victim_rss_kb": victim_rss_kb}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn killed_record_carries_pressure_samples_and_victim_rss() {
        let entry = serde_json::from_value(serde_json::json!({"root": "/checkout", "server": "rust", "nonce": "n", "broker_pid": 1, "broker_start_token": "t", "server_pid": null, "server_start_token": null, "state": "ready", "started_at_ms": 0, "ready_at_ms": null, "estimate_bytes": 0, "settings_hash": "s", "request_count": 0, "last_request_at_ms": null, "peak_rss_kb": 500, "leases": []})).unwrap();
        let record = kill_record(&entry, 100, 200, 400);
        assert_eq!(record.event, "killed");
        assert_eq!(
            record.details,
            serde_json::json!({"reason": "memory pressure", "peak_rss_kb": 500, "free_before_bytes": 100, "free_after_bytes": 200, "victim_rss_kb": 400})
        );
    }
}
