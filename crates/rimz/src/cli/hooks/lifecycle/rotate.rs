//! Auto-rotation trigger for lifecycle event logs.

use super::*;

pub(super) const AUTO_ROTATE_STAMP: &str = "auto-rotate.stamp";
pub(super) const AUTO_ROTATE_DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(60);

pub(super) fn spawn_auto_rotation_if_due(workspace: &ResolvedWorkspace, ledger: &Ledger) {
    let Ok(meta) = std::fs::metadata(&ledger.paths().events_log) else {
        return;
    };
    if !auto_rotation_size_due(meta.len()) {
        return;
    }
    if !auto_rotation_stamp_due(auto_rotate_stamp_age(ledger)) {
        return;
    }
    touch_auto_rotate_stamp(ledger);
    spawn_refresh_detached(&rimz::agents::RefreshSpawn {
        args: vec![
            "--root".to_owned(),
            workspace.project_root.display().to_string(),
            "workspace".to_owned(),
            "rotate-events".to_owned(),
        ],
    });
}

pub(super) fn auto_rotation_size_due(log_len: u64) -> bool {
    log_len >= crate::cli::workspace::DEFAULT_EVENT_LOG_ROTATE_BYTES
}

pub(super) fn auto_rotation_stamp_due(stamp_age: Option<std::time::Duration>) -> bool {
    stamp_age.is_none_or(|age| age >= AUTO_ROTATE_DEBOUNCE)
}

pub(super) fn auto_rotate_stamp_age(ledger: &Ledger) -> Option<std::time::Duration> {
    let modified = std::fs::metadata(ledger.paths().locks_dir.join(AUTO_ROTATE_STAMP))
        .ok()?
        .modified()
        .ok()?;
    std::time::SystemTime::now().duration_since(modified).ok()
}

pub(super) fn touch_auto_rotate_stamp(ledger: &Ledger) {
    let _ = std::fs::write(ledger.paths().locks_dir.join(AUTO_ROTATE_STAMP), b"");
}
