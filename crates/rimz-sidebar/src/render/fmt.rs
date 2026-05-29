//! Time and text formatting helpers shared by the renderer.

use jiff::Timestamp;

pub(super) fn age_short(at: Timestamp) -> String {
    let seconds = Timestamp::now().duration_since(at).as_secs();
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

/// How recently an agent must have acted for its row to keep animating. Past
/// this window the running head freezes, so a wedged or quiet agent never
/// spins. Sized a few animation frames above the tick so a genuinely-busy
/// agent — which emits lifecycle events continuously — stays in motion.
const FRESH_WINDOW_SECS: i64 = 4;

/// Whether `at` is recent enough that animating the row reflects real work.
pub(super) fn is_fresh(at: Timestamp) -> bool {
    Timestamp::now().duration_since(at).as_secs() < FRESH_WINDOW_SECS
}

pub(super) fn time_remaining(deadline: Timestamp) -> String {
    let seconds = deadline.duration_since(Timestamp::now()).as_secs();
    if seconds <= 0 {
        "0s".to_owned()
    } else if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m", seconds / 60)
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
