//! Integration coverage for `rimz coverage`.

use serde_json::Value;

use crate::common::Env;

/// The bare command answers the question a person actually has — what each
/// agent gives them — and leaves the mechanism grids behind `--wiring`.
#[test]
fn coverage_leads_with_the_user_facing_capabilities() {
    let env = Env::new();
    let output = env.rimz().arg("coverage").output().expect("spawn coverage");
    assert!(
        output.status.success(),
        "coverage failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");

    for needle in [
        "RimZ coverage",
        "WHAT EACH AGENT GIVES YOU",
        "CAPABILITY",
        "legend",
        "DETAIL",
        "state",
        "live",
        "history",
        "account",
        "ask",
        "subagents",
    ] {
        assert!(stdout.contains(needle), "missing {needle}:\n{stdout}");
    }
    for absent in ["WIRING — INTEGRATION CONCERNS", "WIRING — LIFECYCLE HOOKS"] {
        assert!(
            !stdout.contains(absent),
            "mechanism grid leaks into the default report: {absent}\n{stdout}"
        );
    }
}

#[test]
fn coverage_wiring_adds_the_mechanism_matrices() {
    let env = Env::new();
    let output = env
        .rimz()
        .args(["coverage", "--wiring"])
        .output()
        .expect("spawn coverage --wiring");
    assert!(
        output.status.success(),
        "coverage --wiring failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");

    for needle in [
        "WHAT EACH AGENT GIVES YOU",
        "WIRING — INTEGRATION CONCERNS",
        "WIRING — LIFECYCLE HOOKS",
        "CONCERN",
        "SIGNAL",
        "plan",
        "no plan-approval gate",
        "SessionStart/UserPromptSubmit/Stop",
    ] {
        assert!(stdout.contains(needle), "missing {needle}:\n{stdout}");
    }
}

#[test]
fn coverage_json_emits_capabilities_and_gates_wiring() {
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
    assert!(report["capabilities"]["agents"].is_array(), "{report}");
    assert!(report["capabilities"]["rows"].is_array(), "{report}");
    assert!(report["coverage"].is_null(), "{report}");
    assert!(report["hooks_matrix"].is_null(), "{report}");

    let output = env
        .rimz()
        .args(["coverage", "--json", "--wiring"])
        .output()
        .expect("spawn coverage json wiring");
    let report: Value =
        serde_json::from_slice(&output.stdout).expect("coverage --json --wiring emits valid json");
    assert!(report["capabilities"]["rows"].is_array(), "{report}");
    assert!(report["coverage"]["rows"].is_array(), "{report}");
    assert!(report["hooks_matrix"]["rows"].is_array(), "{report}");
}

/// The published matrices are the same claim the adapters declare. A hand-edited
/// table that drifts from the code is the failure this pins: readers trust the
/// README, and the README has no other way to stay true.
#[test]
fn published_matrices_match_the_declared_capabilities() {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    const AGENTS: [(&str, &str); 13] = [
        ("Claude Code", "claude"),
        ("Codex", "codex"),
        ("Pi", "pi"),
        ("OpenCode", "opencode"),
        ("Antigravity", "antigravity"),
        ("Copilot", "copilot"),
        ("Droid", "droid"),
        ("Cursor", "cursor"),
        ("Amp", "amp"),
        ("Kiro", "kiro"),
        ("Qwen", "qwen"),
        ("Kimi", "kimi"),
        ("Grok", "grok"),
    ];
    const CAPABILITIES: [&str; 6] = ["state", "live", "history", "account", "ask", "subagents"];

    let env = Env::new();
    let output = env
        .rimz()
        .args(["coverage", "--json"])
        .output()
        .expect("spawn coverage json");
    let report: Value = serde_json::from_slice(&output.stdout).expect("valid json");

    let agents: Vec<String> = report["capabilities"]["agents"]
        .as_array()
        .expect("agents array")
        .iter()
        .map(|agent| agent.as_str().expect("agent kind").to_owned())
        .collect();

    // kind -> capability -> mark, as the adapters declare it.
    let mut declared: BTreeMap<String, BTreeMap<String, char>> = BTreeMap::new();
    for row in report["capabilities"]["rows"].as_array().expect("rows") {
        let label = row["label"].as_str().expect("row label").to_owned();
        for (index, cell) in row["cells"].as_array().expect("cells").iter().enumerate() {
            let mark = match cell["state"].as_str().expect("cell state") {
                "ok" => '●',
                "partial" => '◐',
                "absent" => '✗',
                other => panic!("unknown cell state {other}"),
            };
            declared
                .entry(agents[index].clone())
                .or_default()
                .insert(label.clone(), mark);
        }
    }

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    for doc in ["README.md", "docs/reference/agent-support.md"] {
        let text = std::fs::read_to_string(root.join(doc)).expect("read published doc");
        for (display, kind) in AGENTS {
            let row = text
                .lines()
                .find(|line| {
                    line.starts_with('|')
                        && line
                            .split('|')
                            .nth(1)
                            .is_some_and(|cell| cell.trim() == display)
                })
                .unwrap_or_else(|| panic!("{doc} has no matrix row for {display}"));
            let marks: Vec<char> = row
                .split('|')
                .filter_map(|cell| {
                    let cell = cell.trim();
                    matches!(cell, "●" | "◐" | "✗").then(|| cell.chars().next().expect("mark"))
                })
                .collect();
            assert_eq!(
                marks.len(),
                CAPABILITIES.len(),
                "{doc} row for {display} must carry one mark per capability, found {marks:?}"
            );
            for (capability, published) in CAPABILITIES.into_iter().zip(marks) {
                let declared = declared[kind][capability];
                assert_eq!(
                    published, declared,
                    "{doc} publishes {published} for {display} {capability}; \
                     the adapter declares {declared}. Update the table or the adapter."
                );
            }
        }
    }
}
