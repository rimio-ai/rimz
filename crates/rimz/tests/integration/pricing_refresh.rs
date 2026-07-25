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
}
