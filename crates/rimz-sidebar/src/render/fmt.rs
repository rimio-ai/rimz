//! Time and path formatting helpers shared by every section renderer.

use std::path::Path;

use jiff::Timestamp;

pub(super) fn worktree_from_path(path: Option<&str>) -> String {
    path.and_then(|path| Path::new(path).file_name())
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Workspace")
        .to_owned()
}

pub(super) fn time_ago(at: Timestamp) -> String {
    let seconds = Timestamp::now().duration_since(at).as_secs();
    if seconds <= 0 {
        "just now".to_owned()
    } else if seconds < 60 {
        format!("{seconds}s ago")
    } else if seconds < 60 * 60 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 60 * 60 * 24 {
        format!("{}h ago", seconds / 3600)
    } else {
        format!("{}d ago", seconds / 86_400)
    }
}

/// Elapsed time without the trailing "ago" — for banners that already have
/// their own preposition (e.g. "degraded for 8s").
pub(super) fn elapsed_short(since: Timestamp) -> String {
    let seconds = Timestamp::now().duration_since(since).as_secs();
    if seconds <= 0 {
        "0s".to_owned()
    } else if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 60 * 60 {
        format!("{}m", seconds / 60)
    } else if seconds < 60 * 60 * 24 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

pub(super) fn time_remaining(deadline: Timestamp) -> String {
    let seconds = deadline.duration_since(Timestamp::now()).as_secs();
    if seconds <= 0 {
        "budget elapsed".to_owned()
    } else if seconds < 60 {
        format!("{seconds}s left")
    } else {
        format!("{}m left", seconds / 60)
    }
}

pub(super) fn clip(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    value
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>()
        + "..."
}
