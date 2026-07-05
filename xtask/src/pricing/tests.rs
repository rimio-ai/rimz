use serde_json::{Map, Value};

use super::*;

fn price_field(fields: &Map<String, Value>, name: &str) -> f64 {
    fields.get(name).and_then(Value::as_f64).unwrap()
}

#[test]
fn litellm_compaction_keeps_tiers_and_provider_fast_only() {
    let table = compact_litellm(
        r#"{
            "claude-x": {
                "input_cost_per_token": 0.000003,
                "output_cost_per_token": 0.000015,
                "input_cost_per_token_above_200k_tokens": 0.000006,
                "max_input_tokens": 1000000,
                "provider_specific_entry": {"fast": 2.0, "other": "drop"},
                "source": "drop"
            }
        }"#,
    )
    .unwrap();

    let fields = table.get("claude-x").and_then(Value::as_object).unwrap();
    assert!((price_field(fields, "input_cost_per_token_above_200k_tokens") - 6e-6).abs() < 1e-18);
    assert_eq!(
        fields.get("max_input_tokens").and_then(Value::as_u64),
        Some(1_000_000)
    );
    let provider = fields
        .get("provider_specific_entry")
        .and_then(Value::as_object)
        .unwrap();
    assert_eq!(provider.len(), 1);
    assert_eq!(provider.get("fast").and_then(Value::as_f64), Some(2.0));
    assert!(!fields.contains_key("source"));
}

#[test]
fn models_dev_compaction_converts_per_million_costs() {
    let table = compact_models_dev(
        r#"{
            "anthropic": {"models": {
                "claude-fable-5": {
                    "cost": {
                        "input": 10.0,
                        "output": 50.0,
                        "cache_read": 1.0,
                        "cache_write": 12.5
                    }
                },
                "ignored-missing-output": {"cost": {"input": 1.0}}
            }},
            "gateway": {"models": {
                "claude-fable-5": {
                    "cost": {"input": 99.0, "output": 99.0}
                }
            }},
            "not-a-provider": {"id": "missing models"}
        }"#,
    )
    .unwrap();

    assert_eq!(table.len(), 1);
    let fields = table
        .get("claude-fable-5")
        .and_then(Value::as_object)
        .unwrap();
    assert!((price_field(fields, "input_cost_per_token") - 1e-5).abs() < 1e-18);
    assert!((price_field(fields, "output_cost_per_token") - 5e-5).abs() < 1e-18);
    assert!((price_field(fields, "cache_read_input_token_cost") - 1e-6).abs() < 1e-18);
    assert!((price_field(fields, "cache_creation_input_token_cost") - 1.25e-5).abs() < 1e-18);
}
