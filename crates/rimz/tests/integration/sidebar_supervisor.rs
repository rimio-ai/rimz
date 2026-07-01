//! `rimz sidebar serve` supervisor crash capture.

#![cfg(unix)]

use std::time::Duration;

use rimz::schema::diag::{DiagEnvelope, DiagEvent};

use crate::common::{CommandTimeoutExt, Env};

#[test]
fn sidebar_supervisor_records_worker_abort() {
    let env = Env::new();
    let mut cmd = env.rimz();
    cmd.args([
        "sidebar",
        "serve",
        "--workspace-id",
        env.workspace_id.as_str(),
        "--mux",
        "tmux",
        "--session-name",
        "rimz-test",
    ])
    .env("RIMZ_TEST_SIDEBAR_WORKER_FAULT", "abort");

    let output = cmd
        .bounded_output_within(Duration::from_secs(10))
        .expect("sidebar serve returns");

    assert!(
        !output.status.success(),
        "worker abort should make supervisor exit non-zero"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("rimz test sidebar worker abort"),
        "supervisor should tee worker stderr"
    );

    let diag_path = rimz::diag::DiagSink::under(
        env.state_path_for(&env.project_root).root,
        env.workspace_id.clone(),
        "rimz-test",
        None,
    )
    .log_path();
    let text = std::fs::read_to_string(&diag_path)
        .unwrap_or_else(|err| panic!("read {}: {err}", diag_path.display()));
    let record: DiagEnvelope = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("diag record"))
        .find(|record: &DiagEnvelope| matches!(record.event, DiagEvent::RendererSignalDeath { .. }))
        .expect("renderer signal death diag");

    match record.event {
        DiagEvent::RendererSignalDeath {
            signal,
            exit_code,
            stderr_excerpt,
        } => {
            assert_eq!(signal, Some(6));
            assert_eq!(exit_code, None);
            assert!(stderr_excerpt.contains("rimz test sidebar worker abort"));
        }
        other => panic!("unexpected event: {other:?}"),
    }
}
