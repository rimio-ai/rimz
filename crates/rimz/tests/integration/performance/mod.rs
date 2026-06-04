//! Performance-oriented integration checks. These assert bounded work or
//! single-flight behavior, not product semantics already covered elsewhere.

mod concurrent_writers;
mod sidebar_diff_stats;
mod spending_incremental;
