//! Tolerant projection of Qwen Code's command-statusline JSON.

use std::collections::HashMap;

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
    models: HashMap<String, ModelMetrics>,
    files: FileMetrics,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ModelMetrics {
    tokens: MetricTokens,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct MetricTokens {
    prompt: Option<u64>,
    completion: Option<u64>,
    cached: Option<u64>,
    thoughts: Option<u64>,
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
        let mut prompt = 0;
        let mut cached = 0;
        let mut output = 0;
        let mut has_metrics = false;
        for metrics in self.metrics.models.values() {
            has_metrics = true;
            prompt += metrics.tokens.prompt.unwrap_or(0);
            cached += metrics.tokens.cached.unwrap_or(0);
            output += metrics.tokens.completion.unwrap_or(0) + metrics.tokens.thoughts.unwrap_or(0);
        }
        let current_usage = if has_metrics {
            Some(AgentCurrentUsage {
                input_tokens: Some(prompt.saturating_sub(cached)),
                output_tokens: Some(output),
                cache_creation_input_tokens: None,
                cache_read_input_tokens: Some(cached),
            })
        } else {
            self.context_window
                .current_usage
                .map(|used| AgentCurrentUsage {
                    input_tokens: Some(used),
                    output_tokens: None,
                    cache_creation_input_tokens: None,
                    cache_read_input_tokens: None,
                })
        };
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
            turn_interrupted: None,
            observed_at,
        }
    }
}
