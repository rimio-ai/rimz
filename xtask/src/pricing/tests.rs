use serde_json::{Map, Value};

use super::*;

fn price_field(fields: &Map<String, Value>, name: &str) -> f64 {
    fields.get(name).and_then(Value::as_f64).unwrap()
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
