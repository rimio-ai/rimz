//! Tolerant projection of Qwen Code's command-statusline JSON.

use jiff::Timestamp;
use serde::Deserialize;

use crate::agents::context::{AgentContext, AgentCost, AgentTokenUsage, clamp_pct};
use crate::agents::model_display::display_model;

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

fn model_display_name(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let original = value.trim();
        if original.is_empty() {
            return None;
        }
        let undecorated = original
            .strip_prefix('[')
            .and_then(|rest| rest.find(']').map(|end| rest[end + 1..].trim_start()))
            .filter(|suffix| !suffix.is_empty())
            .unwrap_or(original);
        Some(display_model(undecorated))
    })
}

impl StatuslinePayload {
    pub(crate) fn into_context(self, source: &str, observed_at: Timestamp) -> AgentContext {
        let tokens = (self.context_window.context_window_size.is_some()
            || self.context_window.used_percentage.is_some()
            || self.context_window.remaining_percentage.is_some())
        .then(|| AgentTokenUsage {
            context_window_size: self.context_window.context_window_size,
            used_percentage: clamp_pct(self.context_window.used_percentage),
            remaining_percentage: clamp_pct(self.context_window.remaining_percentage),
            current_usage: None,
            session_usage: None,
        });
        let cost = (self.metrics.files.total_lines_added.is_some()
            || self.metrics.files.total_lines_removed.is_some())
        .then(|| AgentCost {
            total_lines_added: self.metrics.files.total_lines_added,
            total_lines_removed: self.metrics.files.total_lines_removed,
            ..AgentCost::default()
        });
        AgentContext {
            model_display_name: model_display_name(self.model.display_name),
            vim_mode: self.vim.mode,
            agent_version: self.version,
            cost,
            tokens,
            ..AgentContext::new(source, observed_at)
        }
    }
}
