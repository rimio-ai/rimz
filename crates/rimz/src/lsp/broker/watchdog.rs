//! Machine-lock election and least-used-server pressure relief.

use super::{memory, registry};
use crate::lsp::Result;
use registry::State;
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
        if memory::sample()?.available_bytes >= floor {
            break;
        }
        let response = registry::request(
            &entry,
            &serde_json::json!({"op": "stop", "reason": "memory pressure"}),
            Duration::from_secs(2),
        );
        if response.is_ok_and(|response| response["ok"] == true) {
            if entry.broker_pid == std::process::id() {
                break;
            }
            // The acknowledged broker completes shutdown before the next pressure sample.
            std::thread::sleep(Duration::from_secs(2));
            continue;
        }
        if let (Some(pid), Some(token)) = (entry.server_pid, entry.server_start_token.as_deref())
            && crate::proc::process_is_live(pid, Some(token))
        {
            entry.peak_rss_kb = entry
                .peak_rss_kb
                .max(crate::proc::tree_totals(pid).map_or(0, |totals| totals.rss_kb));
            nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid as i32),
                nix::sys::signal::Signal::SIGKILL,
            )
            .map_err(std::io::Error::from)?;
        }
        entry.state = State::Stopped {
            reason: "memory pressure".into(),
            at_ms: crate::utils::time::unix_now_ms(),
        };
        registry::publish(&entry)?;
        if let Some(published) = registry::read_entries()?
            .into_iter()
            .find(|published| published.nonce == entry.nonce)
        {
            super::record_stop(&published);
        }
    }
    Ok(())
}
