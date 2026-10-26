use super::*;

use crate::agents::AgentHookClass;
use serde_json::json;

mod ask;
mod context;
mod install;
mod lifecycle;

// Capability and coverage-table honesty is cross-checked against behavior for
// every adapter in `agents::conformance`; this slice only pins what is
// pi-specific behavior beyond those flags.

fn decode(event: &str, payload: &Value) -> HookOutput {
    PiAdapter
        .decode_hook(event, payload)
        .expect("pi hook decodes")
}

/// The lifecycle observation the decode produced. Panics on enrichment-only
/// events — use [`signal`] to assert that an event carries no observation.
fn observe(event: &str, payload: &Value) -> AgentLifecycleObservation {
    decode(event, payload)
        .lifecycle()
        .cloned()
        .expect("lifecycle observation")
}

fn signal(event: &str, payload: &Value) -> Option<LifecycleSignal> {
    decode(event, payload)
        .lifecycle()
        .map(|observation| observation.signal.clone())
}

#[test]
fn pi_activity_filter_excludes_the_blocking_gate() {
    // Completed-work events touch activity; the blocking `tool_call` gate is
    // excluded so creating the ask never instantly un-blocks the row.
    for event in [
        "tool_execution_end",
        "agent_end",
        "message_update",
        "turn_end",
    ] {
        assert!(
            decode(event, &json!({ "session_id": "sess-1" })).records_progress(),
            "{event} records progress"
        );
    }
    for event in ["tool_call", "session_shutdown"] {
        assert!(
            !decode(event, &json!({ "session_id": "sess-1" })).records_progress(),
            "{event} does not record progress"
        );
    }
}

#[test]
fn launch_resume_and_fork_argv_match_pi() {
    assert_eq!(
        PiAdapter.spec().launch.picker,
        Some(["pi", "--resume"].as_slice())
    );
    assert_eq!(
        PiAdapter.launch_command(&[], None),
        Some(vec!["pi".to_owned()])
    );
    assert_eq!(
        PiAdapter.launch_command(
            &["--model".to_owned(), "large".to_owned()],
            Some("review this"),
        ),
        Some(vec![
            "pi".to_owned(),
            "--model".to_owned(),
            "large".to_owned(),
            "--".to_owned(),
            "review this".to_owned(),
        ])
    );
    assert_eq!(
        PiAdapter.resume_command("0199aaf2", Path::new("/tmp")),
        Some(vec![
            "pi".to_owned(),
            "--session".to_owned(),
            "0199aaf2".to_owned(),
        ])
    );
    assert_eq!(
        PiAdapter.spec().launch.fork_command("0199aaf2"),
        Some(vec![
            "pi".to_owned(),
            "--fork".to_owned(),
            "0199aaf2".to_owned(),
        ])
    );
}

#[test]
fn neutral_decision_shape_is_pinned() {
    let rendered = decode("agent_end", &Value::Null).json_reply().cloned();
    insta::assert_snapshot!(format!("{rendered:?}"), @"None");
}

#[test]
fn named_login_scopes_spending_and_session_transcripts() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let named = temp.path().join("work");
    let file_name = "2026-06-02T10-00-00-000Z_shared-session.jsonl";
    let native_file = home.join(".pi/agent/sessions/project").join(file_name);
    let named_file = named.join("sessions/project").join(file_name);
    for path in [&native_file, &named_file] {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }
    let ambient = BTreeMap::from([("HOME".into(), home.to_string_lossy().into_owned())]);
    let login_env = crate::agents::ProviderLogin::named(
        crate::ids::AgentKind::new_unchecked("pi"),
        "work".parse().unwrap(),
        named,
    )
    .unwrap()
    .env(&ambient);
    let files = PiAdapter.transcript_files(&login_env);
    assert_eq!(files, vec![named_file.clone()]);
    assert_eq!(
        PiAdapter.session_transcript("shared-session", None, &login_env),
        Some(named_file),
    );
}

#[test]
fn session_directory_override_scopes_transcripts() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project/override-session.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "").unwrap();
    let env = BTreeMap::from([(
        "PI_CODING_AGENT_SESSION_DIR".into(),
        temp.path().to_string_lossy().into_owned(),
    )]);
    assert_eq!(PiAdapter.transcript_files(&env), vec![path.clone()]);
    assert_eq!(
        PiAdapter.session_transcript("override-session", None, &env),
        Some(path),
    );
}

#[test]
fn empty_session_directory_override_reads_home_sessions() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("sessions/project/home-session.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "").unwrap();
    let env = BTreeMap::from([
        (
            "PI_CODING_AGENT_DIR".into(),
            temp.path().to_string_lossy().into_owned(),
        ),
        ("PI_CODING_AGENT_SESSION_DIR".into(), String::new()),
    ]);
    assert_eq!(PiAdapter.transcript_files(&env), vec![path]);
}

#[test]
fn comma_list_override_takes_precedence_over_session_directory() {
    let temp = tempfile::tempdir().unwrap();
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    let paths = [first.join("one.jsonl"), second.join("two.jsonl")];
    for path in &paths {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }
    let env = BTreeMap::from([
        (
            "PI_AGENT_DIR".into(),
            format!(" {}, , {} ", first.display(), second.display()),
        ),
        (
            "PI_CODING_AGENT_SESSION_DIR".into(),
            temp.path().join("unused").to_string_lossy().into_owned(),
        ),
    ]);
    let mut files = PiAdapter.transcript_files(&env);
    files.sort();
    assert_eq!(files, paths);
}

#[test]
fn account_probe_uses_login_auth_and_session_roots() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("auth.json"),
        r#"{"aaa":{"type":"api_key"},"zzz":{"type":"oauth"}}"#,
    )
    .unwrap();
    let root = temp.path().join("sessions/project");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("session.jsonl"),
        r#"{"type":"model_change","provider":"zzz"}"#,
    )
    .unwrap();
    let env = BTreeMap::from([(
        "PI_CODING_AGENT_DIR".into(),
        temp.path().to_string_lossy().into_owned(),
    )]);
    let crate::agents::account::AccountProbe::Found(account) = PiAdapter.probe_account(&env) else {
        panic!("login's auth and session must select its provider");
    };
    assert_eq!(account.sub_provider.as_deref(), Some("zzz"));
    assert_eq!(account.metered, Some(true));
}

#[test]
fn usage_probe_uses_login_auth_and_session_roots() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("auth.json"),
        json!({
            "aaa": {"type": "oauth", "access": "first", "expires": i64::MAX},
            "zzz": {"type": "oauth", "access": "second", "expires": i64::MAX},
        })
        .to_string(),
    )
    .unwrap();
    let root = temp.path().join("sessions");
    std::fs::create_dir_all(&root).unwrap();
    let session = root.join("session.jsonl");
    let env = BTreeMap::from([(
        "PI_CODING_AGENT_DIR".into(),
        temp.path().to_string_lossy().into_owned(),
    )]);
    let mut keys = Vec::new();
    for provider in ["aaa", "zzz"] {
        std::fs::write(
            &session,
            json!({"type": "model_change", "provider": provider}).to_string(),
        )
        .unwrap();
        let crate::agents::AccountUsageProbe::NoCredentials(identity) =
            PiAdapter.probe_account_usage(&env)
        else {
            panic!("unsupported providers need no network request");
        };
        assert!(identity.credentials_stamp.is_some());
        keys.push(identity.account_key.expect("selected OAuth identity"));
    }
    assert_ne!(
        keys[0], keys[1],
        "the login's session selects its credential"
    );
}

#[test]
fn account_stamp_tracks_only_login_auth_and_session_roots() {
    let temp = tempfile::tempdir().unwrap();
    let auth = temp.path().join("auth.json");
    let root = temp.path().join("sessions");
    std::fs::create_dir_all(&root).unwrap();
    let env = BTreeMap::from([(
        "PI_CODING_AGENT_DIR".into(),
        temp.path().to_string_lossy().into_owned(),
    )]);
    let before_auth = PiAdapter.account_inputs_stamp(&env);
    std::fs::write(&auth, "{}").unwrap();
    let before_session = PiAdapter.account_inputs_stamp(&env);
    assert_ne!(before_auth, before_session, "the login's auth changed");
    std::fs::File::open(&root)
        .unwrap()
        .set_modified(std::time::SystemTime::UNIX_EPOCH)
        .unwrap();
    let before_session = PiAdapter.account_inputs_stamp(&env);
    std::fs::write(root.join("new.jsonl"), "{}").unwrap();
    assert_ne!(
        before_session,
        PiAdapter.account_inputs_stamp(&env),
        "the login's session root changed",
    );
}
