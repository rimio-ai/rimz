//! Provider-neutral out-of-band context refresh services.

use std::path::Path;
#[cfg(test)]
use std::time::Duration;

use jiff::Timestamp;

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

pub fn serve_broker(
    kind: &str,
    session_name: Option<&str>,
    socket_path: &Path,
) -> std::io::Result<()> {
    super::find_definition(kind).map_or_else(
        || {
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                format!("{kind} has no context broker"),
            ))
        },
        |definition| definition.serve_context_broker(session_name, socket_path),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn refresh_transcript_context(
    kind: &str,
    session_id: &str,
    model_hint: Option<&str>,
    prior_transcript_path: Option<&str>,
    prior_transcript_stat: Option<&TranscriptStat>,
    prior_spend_fold: Option<&LocalSpendFold>,
    pricing_cache_path: &Path,
) -> Option<LocalContextRefresh> {
    super::find_definition(kind)?.refresh_transcript_context_runtime(
        session_id,
        model_hint,
        prior_transcript_path,
        prior_transcript_stat,
        prior_spend_fold,
        pricing_cache_path,
    )
}

pub fn rich_refresh_due(
    kind: &str,
    record: Option<&crate::store::agent_context::AgentContextRecord>,
    within: i64,
) -> bool {
    super::find_definition(kind)
        .is_some_and(|definition| definition.rich_context_refresh_due(record, within))
}

pub fn refresh_runtime_enrichment(
    kind: &str,
    session_id: Option<&str>,
    model_hint: Option<&str>,
    broker_socket: Option<&Path>,
) -> Option<RuntimeEnrichment> {
    super::find_definition(kind)?.refresh_runtime_enrichment(session_id, model_hint, broker_socket)
}

pub fn merge_runtime_context(
    kind: &str,
    runtime: &crate::RuntimePaths,
    session_id: &str,
    context: AgentContext,
) -> anyhow::Result<()> {
    super::find_definition(kind).map_or(Ok(()), |definition| {
        definition.merge_runtime_context(runtime, session_id, context)
    })
}

pub fn refresh_embedded_context(
    kind: &str,
    server_url: &str,
    session_id: &str,
    current_model: Option<&str>,
    prior: Option<&AgentContext>,
    rich_observed_at: Option<Timestamp>,
    now: Timestamp,
) -> Option<AgentContext> {
    super::find_definition(kind)?.refresh_embedded_context(
        server_url,
        session_id,
        current_model,
        prior,
        rich_observed_at,
        now,
    )
}

pub fn merge_embedded_context(
    kind: &str,
    current: &mut AgentContext,
    observed: &AgentContext,
) -> bool {
    super::find_definition(kind)
        .is_some_and(|definition| definition.merge_embedded_context(current, observed))
}
