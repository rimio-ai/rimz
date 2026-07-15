//! Tolerant projection of Qwen Code's command-statusline JSON.

use std::collections::BTreeMap;

use jiff::Timestamp;
use serde::Deserialize;

use super::payloads::normalized_generated_output;
use crate::agents::context::{
    AgentContext, AgentCost, AgentSessionUsage, AgentTokenUsage, CostCoverage, clamp_pct,
};
use crate::agents::model_display::display_model;
use crate::agents::pricing::PriceBook;
use crate::agents::transcript_fs::deserialize_optional_u64_lossy;

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
    #[serde(deserialize_with = "deserialize_models_lossy")]
    models: BTreeMap<String, ModelMetrics>,
    files: FileMetrics,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ModelMetrics {
    tokens: ModelTokens,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ModelTokens {
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    prompt: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    completion: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    total: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    cached: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
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

fn deserialize_models_lossy<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, ModelMetrics>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    let Some(models) = value.as_object() else {
        return Ok(BTreeMap::new());
    };
    Ok(models
        .iter()
        .filter_map(|(model, value)| {
            serde_json::from_value(value.clone())
                .ok()
                .map(|metrics| (model.clone(), metrics))
        })
        .collect())
}

fn add_optional(target: &mut Option<u64>, value: Option<u64>) {
    if let Some(value) = value {
        *target = Some(target.unwrap_or(0).saturating_add(value));
    }
}

impl ModelTokens {
    fn session_usage(&self) -> Option<AgentSessionUsage> {
        let generated =
            normalized_generated_output(self.prompt, self.completion, self.thoughts, self.total);
        let thinking_tokens = generated
            .zip(self.thoughts)
            .map(|(generated, thoughts)| thoughts.min(generated));
        let usage = AgentSessionUsage {
            input_tokens: self
                .prompt
                .map(|prompt| prompt.saturating_sub(self.cached.unwrap_or(0))),
            output_tokens: generated
                .map(|generated| generated.saturating_sub(thinking_tokens.unwrap_or(0))),
            cache_creation_input_tokens: None,
            cache_read_input_tokens: self.cached,
            thinking_tokens,
        };
        (usage.input_tokens.is_some()
            || usage.output_tokens.is_some()
            || usage.cache_read_input_tokens.is_some()
            || usage.thinking_tokens.is_some())
        .then_some(usage)
    }
}

impl Metrics {
    fn session_usage(&self) -> Option<AgentSessionUsage> {
        let mut aggregate = AgentSessionUsage::default();
        let mut present = false;
        for usage in self
            .models
            .values()
            .filter_map(|metrics| metrics.tokens.session_usage())
        {
            present = true;
            add_optional(&mut aggregate.input_tokens, usage.input_tokens);
            add_optional(&mut aggregate.output_tokens, usage.output_tokens);
            add_optional(
                &mut aggregate.cache_read_input_tokens,
                usage.cache_read_input_tokens,
            );
            add_optional(&mut aggregate.thinking_tokens, usage.thinking_tokens);
        }
        present.then_some(aggregate)
    }
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
    pub(crate) fn cost(&self, prices: &PriceBook) -> Option<AgentCost> {
        let mut total_cost_usd = 0.0;
        for (model, metrics) in &self.metrics.models {
            let model = model.trim();
            if model.is_empty() {
                continue;
            }
            let Some(usage) = metrics.tokens.session_usage() else {
                continue;
            };
            let Some(price) = prices.price(model) else {
                continue;
            };
            let cost = price.cost(
                usage.input_tokens.unwrap_or(0),
                usage.displayed_output_tokens(),
                0,
                0,
                usage.cache_read_input_tokens.unwrap_or(0),
                false,
            );
            if cost.is_finite() {
                total_cost_usd += cost;
            }
        }
        (total_cost_usd.is_finite() && total_cost_usd > 0.0).then(|| AgentCost {
            total_cost_usd: Some(total_cost_usd),
            coverage: CostCoverage::Session,
            ..AgentCost::default()
        })
    }

    pub(crate) fn into_context(self, source: &str, observed_at: Timestamp) -> AgentContext {
        let session_usage = self.metrics.session_usage();
        let tokens = (self.context_window.context_window_size.is_some()
            || self.context_window.used_percentage.is_some()
            || self.context_window.remaining_percentage.is_some()
            || session_usage.is_some())
        .then(|| AgentTokenUsage {
            context_window_size: self.context_window.context_window_size,
            used_percentage: clamp_pct(self.context_window.used_percentage),
            remaining_percentage: clamp_pct(self.context_window.remaining_percentage),
            current_usage: None,
            session_usage,
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
