use super::*;

mod api_errors;
mod card_status;
mod compaction;
mod native_permission;
mod no_request;
mod raised;
mod sleeping;
mod stall;
mod stuck;
mod turn_complete;
mod turn_interrupted;
mod waiting;

/// Report the provider version from which Claude records each turn's request start.
fn records_request_starts(mut agent: AgentState) -> AgentState {
    agent.context.get_or_insert_with(bare_context).agent_version = Some("2.1.300".to_owned());
    agent
}

fn unprojectable_spent_window(resets_in_secs: i64) -> RateLimitWindow {
    RateLimitWindow {
        duration_mins: None,
        ..window(100, resets_in_secs)
    }
}
