use super::*;

mod api_errors;
mod auto_continue;
mod compaction;
mod stall;
mod turn_complete;
mod turn_interrupted;
mod waiting;

fn unprojectable_spent_window(resets_in_secs: i64) -> RateLimitWindow {
    RateLimitWindow {
        duration_mins: None,
        ..window(100, resets_in_secs)
    }
}
