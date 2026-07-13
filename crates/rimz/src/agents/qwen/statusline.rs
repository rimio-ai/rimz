//! Tolerant projection of Qwen Code's command-statusline JSON.

use jiff::Timestamp;
use serde::Deserialize;

use crate::agents::context::{AgentContext, AgentCost, AgentCurrentUsage, AgentTokenUsage};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct StatuslinePayload {
    version: Option<String>,
    model: Model,
    context_window: ContextWindow,
    metrics: Metrics,
    vim: Vim,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Model {
    display_name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ContextWindow {
    context_window_size: Option<u64>,
    used_percentage: Option<f64>,
    remaining_percentage: Option<f64>,
    current_usage: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Metrics {
    files: FileMetrics,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FileMetrics {
    total_lines_added: Option<u64>,
    total_lines_removed: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Vim {
    mode: Option<String>,
}

fn clamp_pct(value: Option<f64>) -> Option<u8> {
    value.map(|value| value.round().clamp(0.0, 100.0) as u8)
}

impl StatuslinePayload {
    pub(crate) fn into_context(self, source: &str, observed_at: Timestamp) -> AgentContext {
        // `current_usage` is Qwen's latest prompt-token gauge and is the
        // numerator behind `used_percentage`. `metrics.models` is cumulative
        // across the whole session (and every routed model), so mapping those
        // totals here would make the live context breakdown grow without bound.
        let current_usage = self
            .context_window
            .current_usage
            .map(|used| AgentCurrentUsage {
                input_tokens: Some(used),
                output_tokens: None,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            });
        let tokens = (self.context_window.context_window_size.is_some()
            || self.context_window.used_percentage.is_some()
            || self.context_window.remaining_percentage.is_some()
            || current_usage.is_some())
        .then(|| AgentTokenUsage {
            context_window_size: self.context_window.context_window_size,
            used_percentage: clamp_pct(self.context_window.used_percentage),
            remaining_percentage: clamp_pct(self.context_window.remaining_percentage),
            current_usage,
        });
        let cost = (self.metrics.files.total_lines_added.is_some()
            || self.metrics.files.total_lines_removed.is_some())
        .then(|| AgentCost {
            total_lines_added: self.metrics.files.total_lines_added,
            total_lines_removed: self.metrics.files.total_lines_removed,
            ..AgentCost::default()
        });
        AgentContext {
            source: source.to_owned(),
            session_name: None,
            session_preview: None,
            model_id: None,
            model_display_name: self.model.display_name,
            effort: None,
            thinking_enabled: None,
            output_style: None,
            vim_mode: self.vim.mode,
            agent_version: self.version,
            exceeds_200k_tokens: None,
            cost,
            tokens,
            rate_limits: None,
            pr: None,
            account: None,
            turn_opened_by: Vec::new(),
            turn_error: None,
            turn_complete: None,
            plan_proposed: None,
            turn_interrupted: None,
            observed_at,
        }
    }
}
