//! Integration coverage for `rimz coverage`.

use serde_json::Value;

use crate::common::Env;

#[test]
fn coverage_human_report_renders_both_matrices_and_detail() {
    let env = Env::new();
    let output = env.rimz().arg("coverage").output().expect("spawn coverage");
    assert!(
        output.status.success(),
        "coverage failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");

    for needle in [
        "Rimz coverage",
        "AGENT COVERAGE",
        "HOOKS MATRIX",
        "CONCERN",
        "SIGNAL",
        "legend",
        "DETAIL",
        "plan",
        "no plan-approval gate",
        "SessionStart/UserPromptSubmit/Stop",
    ] {
        assert!(stdout.contains(needle), "missing {needle}:\n{stdout}");
    }
}

#[test]
fn coverage_json_emits_both_matrices() {
    let env = Env::new();
    let output = env
        .rimz()
        .args(["coverage", "--json"])
        .output()
        .expect("spawn coverage json");
    assert!(
        output.status.success(),
        "coverage --json failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let report: Value =
        serde_json::from_slice(&output.stdout).expect("coverage --json emits valid json");
    assert!(report["coverage"]["agents"].is_array(), "{report}");
    assert!(report["coverage"]["rows"].is_array(), "{report}");
    assert!(report["hooks_matrix"]["agents"].is_array(), "{report}");
    assert!(report["hooks_matrix"]["rows"].is_array(), "{report}");
}
