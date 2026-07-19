//! Provider-neutral out-of-band context refresh services.
//!
//! Codex's app-server and OpenCode's embedded server are provider-only
//! transports with one implementation each. This module is their neutral
//! façade: it keeps `cli/` clear of the private-adapters boundary while the
//! behavior stays in the deep provider module that owns it.

use std::path::Path;
#[cfg(test)]
use std::time::Duration;

use jiff::Timestamp;

use super::adapters::{codex, opencode};
use super::{
    AgentContext, ExtraCredits, LocalContextRefresh, LocalSpendFold, ResetCredits, TranscriptStat,
};

pub const RICH_REFRESH_THROTTLE_SECS: i64 = 20;
#[cfg(test)]
pub const MAX_REALTIME_ACCOUNT_USAGE_DURATION: Duration = Duration::from_secs(10);

pub struct RuntimeEnrichment {
    pub context: AgentContext,
    pub extra_credits: Option<ExtraCredits>,
    pub reset_credits: Option<ResetCredits>,
}

pub fn serve_broker(session_name: Option<&str>, socket_path: &Path) -> std::io::Result<()> {
    codex::broker::serve(codex::broker::BrokerInfo {
        session: session_name,
        socket_path,
    })
}

pub fn refresh_transcript_context(
    session_id: &str,
    model_hint: Option<&str>,
    prior_transcript_path: Option<&str>,
    prior_transcript_stat: Option<&TranscriptStat>,
    prior_spend_fold: Option<&LocalSpendFold>,
    pricing_cache_path: &Path,
) -> Option<LocalContextRefresh> {
    codex::refresh_transcript_context(
        session_id,
        model_hint,
        prior_transcript_path,
        prior_transcript_stat,
        prior_spend_fold,
        pricing_cache_path,
    )
}

pub fn rich_refresh_due(
    record: Option<&crate::store::agent_context::AgentContextRecord>,
    within: i64,
) -> bool {
    codex::app_server_due(record, within)
}

pub fn refresh_runtime_enrichment(
    session_id: Option<&str>,
    model_hint: Option<&str>,
    broker_socket: Option<&Path>,
) -> Option<RuntimeEnrichment> {
    codex::refresh_app_server_enrichment(session_id, model_hint, broker_socket).map(|enrichment| {
        RuntimeEnrichment {
            context: enrichment.context,
            extra_credits: enrichment.extra_credits,
            reset_credits: enrichment.reset_credits,
        }
    })
}

pub fn merge_runtime_context(
    runtime: &crate::RuntimePaths,
    session_id: &str,
    context: AgentContext,
) -> anyhow::Result<()> {
    codex::merge_app_server_context(runtime, session_id, context)
}

pub fn refresh_embedded_context(
    server_url: &str,
    session_id: &str,
    current_model: Option<&str>,
    prior: Option<&AgentContext>,
    rich_observed_at: Option<Timestamp>,
    now: Timestamp,
) -> Option<AgentContext> {
    opencode::server::refresh_rich_context(
        server_url,
        session_id,
        current_model,
        prior,
        rich_observed_at,
        now,
    )
}

pub fn merge_embedded_context(current: &mut AgentContext, observed: &AgentContext) -> bool {
    opencode::server::merge_rich_context(current, observed)
}
