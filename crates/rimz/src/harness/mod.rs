//! Agent harness — spawn, address, drive, and reclaim agent sessions.

pub mod ancestry;
pub mod assist_log;
pub(crate) mod auto_continue;
pub mod auto_gc;
pub mod auto_redeem;
pub mod board;
pub mod budget;
pub mod deadline;
pub mod fleet;
pub mod idle_compact;
pub mod launch;
mod launch_context;
mod launch_env;
pub mod launch_plan;
pub mod launch_reminders;
pub mod orphan_sweep;
pub mod owed;
pub mod parent_watch;
pub mod plan;
pub mod prompt_compose;
pub mod rebirth;
pub mod resume;
pub mod run;
pub mod run_timeout;
pub mod run_wake;
pub mod schedule;
pub mod scratch;
pub mod spec;
pub mod subagent_policy;
pub mod team_prompt;
pub mod team_stage;

pub use auto_continue::AutoContinueRequest;
