use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::common::{Env, tmux_pane};

#[test]
fn sidebar_mark_unread_and_mark_read_drive_snapshot_unread_bit() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(
        &env,
        "sess-unread",
        "feature-unread",
        &[("TMUX_PANE", "%1")],
    );
    let pane = tmux_pane("%1", "claude", &env.project_root);
    let pane_fixture = env.write_pane_fixture(std::slice::from_ref(&pane));

    let out = env
        .rimz()
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .args(["--mux", "tmux", "sidebar", "mark-unread", "@claude"])
        .output()
        .expect("mark unread");
    assert!(
        out.status.success(),
        "mark-unread failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let unread_file: serde_json::Value =
        serde_json::from_slice(&std::fs::read(env.runtime_paths().unread_path()).unwrap())
            .expect("unread json");
    assert!(
        unread_file["episodes"]["sess-unread"].as_i64().is_some(),
        "mark-unread writes unread.json: {unread_file}"
    );
    assert!(row_unread(
        &env.snapshot_json_with_panes(std::slice::from_ref(&pane)),
        "sess-unread"
    ));

    let out = env
        .rimz()
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .args(["--mux", "tmux", "sidebar", "mark-read", "@claude"])
        .output()
        .expect("mark read");
    assert!(
        out.status.success(),
        "mark-read failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !row_unread(&env.snapshot_json_with_panes(&[pane]), "sess-unread"),
        "read marks clear the derived unread bit"
    );
    let unread_file: serde_json::Value =
        serde_json::from_slice(&std::fs::read(env.runtime_paths().unread_path()).unwrap())
            .expect("unread json after mark-read");
    assert!(
        unread_file["episodes"]["sess-unread"].is_null(),
        "mark-read removes the open episode immediately: {unread_file}"
    );
    let manual_marks =
        std::fs::read_to_string(env.runtime_paths().read_marks_dir.join("manual.json"))
            .expect("manual read mark");
    assert!(
        manual_marks.contains("sess-unread"),
        "mark-read writes a durable manual receipt: {manual_marks}"
    );
}

#[test]
fn sidebar_notify_test_spawns_configured_command_with_notify_env() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(
        &env,
        "sess-notify",
        "feature-notify",
        &[("TMUX_PANE", "%1")],
    );
    let pane_fixture = env.write_pane_fixture(&[tmux_pane("%1", "claude", &env.project_root)]);
    let log_path = env.home_root.join("notify-test.log");
    write_machine_config(
        &env,
        r#"[notifications]
command = '''printf '%s|%s|%s|%s\n' "$RIMZ_NOTIFY_KIND" "$RIMZ_NOTIFY_AGENT" "$RIMZ_NOTIFY_TITLE" "$RIMZ_NOTIFY_BODY" >> "$RIMZ_NOTIFY_TEST_LOG"'''
"#,
    );

    let out = env
        .rimz()
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .env("RIMZ_NOTIFY_TEST_LOG", &log_path)
        .args([
            "--mux",
            "tmux",
            "sidebar",
            "notify-test",
            "@claude",
            "--title",
            "Test title",
            "--body",
            "Test body",
            "--kind",
            "success",
            "--force-bell",
        ])
        .output()
        .expect("notify test");
    assert!(
        out.status.success(),
        "notify-test failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let logged = wait_for_text(&log_path, Duration::from_secs(2));
    assert!(
        logged.contains("success|") && logged.contains("|Test title|Test body"),
        "notify command env missing: {logged:?}"
    );
}

fn register_running_agent(env: &Env, session_id: &str, branch: &str, pane_env: &[(&str, &str)]) {
    run_hook(
        env,
        json!({
            "hook_event_name": "SessionStart",
            "session_id": session_id,
            "worktree_branch": branch,
        }),
        pane_env,
    );
    run_hook(
        env,
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": session_id,
            "prompt": "work",
            "worktree_branch": branch,
        }),
        pane_env,
    );
}

fn run_hook(env: &Env, payload: serde_json::Value, pane_env: &[(&str, &str)]) {
    let payload = serde_json::to_string(&payload).expect("payload");
    let output = env.run_installed_hook_in_pane("claude", &payload, pane_env);
    assert!(
        output.status.success(),
        "hook failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn row_unread(snapshot: &serde_json::Value, row_id: &str) -> bool {
    snapshot["worktree_groups"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|group| group["rows"].as_array().into_iter().flatten())
        .find(|row| row["id"] == row_id)
        .and_then(|row| row["unread"].as_bool())
        .unwrap_or(false)
}

fn write_machine_config(env: &Env, text: &str) {
    let dir = env.config_root().join("rimz");
    std::fs::create_dir_all(&dir).expect("mkdir config dir");
    std::fs::write(dir.join("config.toml"), text).expect("write config");
}

fn wait_for_text(path: &Path, timeout: Duration) -> String {
    let until = Instant::now() + timeout;
    while Instant::now() < until {
        if let Ok(text) = std::fs::read_to_string(path)
            && !text.is_empty()
        {
            return text;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    std::fs::read_to_string(path).unwrap_or_default()
}
