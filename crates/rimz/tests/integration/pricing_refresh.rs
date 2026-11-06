//! Out-of-process coverage for the hidden pricing projection helper.

use assert_cmd::assert::OutputAssertExt;

use crate::common::Env;

#[test]
fn pricing_refresh_uses_local_documents_and_writes_the_projected_snapshot() {
    let env = Env::new();
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixtures = manifest.join("src/agents/pricing/tests/fixtures");
    let out = env.project_root.join("pricing.json");

    env.rimz()
        .args(["pricing-refresh", "--out"])
        .arg(&out)
        .env("RIMZ_PRICING_JSON_PATH", fixtures.join("litellm.json"))
        .env(
            "RIMZ_PRICING_MODELS_DEV_JSON_PATH",
            fixtures.join("models-dev.json"),
        )
        .assert()
        .success();

    let snapshot: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
    assert!(snapshot.get("claude-3-5-haiku-20241022").is_some());
    assert_eq!(
        snapshot["gpt-5.6-sol"]["long_context_threshold"].as_u64(),
        Some(272_000)
    );
    let astra = &snapshot["gpt-6-astra"];
    assert_eq!(astra["long_context_threshold"].as_u64(), Some(272_000));
    assert_eq!(astra["max_input_tokens"].as_u64(), Some(922_000));
    assert_eq!(astra["input_cost_per_token"].as_f64(), Some(10e-6));
    assert_eq!(
        astra["cache_creation_input_token_cost_above_200k_tokens"].as_f64(),
        Some(25e-6)
    );
}

#[test]
fn pricing_refresh_check_names_the_missing_models_dev_override() {
    let env = Env::new();
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixtures = manifest.join("src/agents/pricing/tests/fixtures");

    // A LiteLLM-only override projects a deliberately partial table, so
    // coverage has to report the missing document rather than eight renames.
    let output = env
        .rimz()
        .args(["pricing-refresh", "--check"])
        .env("RIMZ_PRICING_JSON_PATH", fixtures.join("litellm.json"))
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&output.get_output().stderr).to_string();
    assert!(
        stderr.contains("RIMZ_PRICING_MODELS_DEV_JSON_PATH"),
        "unexpected stderr: {stderr}"
    );
    assert!(
        !stderr.contains("has no priced models"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn pricing_refresh_check_reports_scoped_rows_and_requires_only_first_party_providers() {
    let env = Env::new();
    let mut litellm: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
        include_str!("../../src/agents/pricing/tests/fixtures/litellm.json"),
    )
    .unwrap();
    let tiered = serde_json::json!({
        "input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6,
        "input_cost_per_token_above_200k_tokens": 2e-6,
        "output_cost_per_token_above_200k_tokens": 4e-6
    });
    for index in 0..1000 {
        litellm.insert(format!("coverage-model-{index}"), tiered.clone());
    }
    for model in
        rimz::agents::all_definitions().filter_map(|definition| definition.spec().default_model)
    {
        litellm
            .entry(model.to_owned())
            .or_insert_with(|| tiered.clone());
    }
    for model in ["gpt-5.4", "gpt-5.4-pro", "gpt-5.5-pro", "gpt-5.6"] {
        litellm
            .entry(model.to_owned())
            .or_insert_with(|| tiered.clone());
    }
    let litellm_path = env.project_root.join("litellm.json");
    std::fs::write(&litellm_path, serde_json::to_vec(&litellm).unwrap()).unwrap();
    let models_dev_path = env.project_root.join("models-dev.json");
    let mut models_dev: serde_json::Value = serde_json::from_str(include_str!(
        "../../src/agents/pricing/tests/fixtures/models-dev.json"
    ))
    .unwrap();
    let mut observed = Vec::new();
    for removed in [None, Some("opencode-go"), Some("anthropic")] {
        if let Some(provider) = removed {
            models_dev.as_object_mut().unwrap().remove(provider);
        }
        std::fs::write(&models_dev_path, serde_json::to_vec(&models_dev).unwrap()).unwrap();
        let output = env
            .rimz()
            .args(["pricing-refresh", "--check"])
            .env("RIMZ_PRICING_JSON_PATH", &litellm_path)
            .env("RIMZ_PRICING_MODELS_DEV_JSON_PATH", &models_dev_path)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let expected_count = if removed.is_none() { 21 } else { 19 };
        let correct = if removed == Some("anthropic") {
            !output.status.success()
                && stderr.contains("models.dev provider `anthropic` is missing")
        } else {
            output.status.success() && stdout.contains(&format!(", scoped {expected_count}, "))
        };
        observed.push((correct, stdout, stderr));
    }
    assert!(
        observed.iter().all(|(correct, _, _)| *correct),
        "coverage outcomes: {observed:?}"
    );
}
