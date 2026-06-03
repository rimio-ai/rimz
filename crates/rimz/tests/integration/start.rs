//! `rimz start` run from inside a session of the selected mux: a same-mux room
//! can't be nested, so the default launch reports the directory's room and
//! exits before any side effect instead of emitting a doomed nested
//! `attach --create`.

use rimz::workspace::WorkspaceResolver;

use crate::common::{CommandTimeoutExt, Env};

#[test]
fn start_inside_selected_mux_reports_and_skips_launch() {
    let env = Env::new();
    let workspace = WorkspaceResolver::resolve(&env.project_root, None).expect("resolve");

    let output = env
        .rimz()
        .arg("start")
        // Pretend we're already inside a Zellij session: `auto_detect_backend`
        // selects Zellij from `ZELLIJ` alone, with no binary on PATH.
        .env("ZELLIJ", "1")
        .env_remove("ZELLIJ_PANE_ID")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .bounded_output()
        .expect("run rimz start");

    assert!(
        output.status.success(),
        "a nested run is a no-op success, got: {:?}",
        output.status,
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("attach --create"),
        "a nested run must not emit the doomed attach command, got stdout: {stdout}"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&workspace.session_name),
        "stderr should name the directory's room, got: {stderr}"
    );
    assert!(
        stderr.contains("nested"),
        "stderr should explain it can't nest a room, got: {stderr}"
    );
    // The guard returns before `ensure_detected_agent_hooks`, so the first-run
    // hook consent gate never prints — proving the bypass skips the ceremony.
    assert!(
        !stderr.contains("Rimz first run"),
        "the nested bypass must run before hook install, got: {stderr}"
    );
}
