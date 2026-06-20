//! Performance-oriented integration checks. These assert bounded work or
//! single-flight behavior, not product semantics already covered elsewhere.

mod concurrent_writers;
mod consumer_enrichment;
mod enrichment_cadence;
mod fold_incremental;
mod ledger_bytes;
mod ledger_fsync;
mod produce_budget;
mod sidebar_diff_stats;
mod spending_incremental;
