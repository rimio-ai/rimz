use super::*;
use crate::lsp::registry::State;

#[test]
fn query_folder_uses_only_a_canonical_folder_inside_the_resolved_checkout() {
    let root = Path::new("/checkout");
    let actual = [
        Ok(PathBuf::from("/checkout/src")),
        Ok(PathBuf::from("/checkout")),
        Ok(PathBuf::from("/other-checkout/src")),
        Err(std::io::Error::other("deleted cwd")),
    ]
    .map(|cwd| query_folder(root, cwd));
    assert_eq!(actual, ["src", "", "", ""].map(PathBuf::from));
}

#[test]
fn query_server_names_include_config_and_checkout_rows_only() {
    let config = serde_json::from_value(
        serde_json::json!({"command":["stub"],"extensions":["rs"],"root-markers":[]}),
    )
    .unwrap();
    let mut context = QueryContext {
        root: "/checkout".into(),
        folder: PathBuf::new(),
        dir: PathBuf::new(),
        entries: vec![
            entry("/checkout", "registered", State::Ready),
            entry("/elsewhere", "elsewhere", State::Ready),
        ],
        servers: std::collections::BTreeMap::from([("configured".into(), config)]),
        usage: None,
    };
    let error = context.validate_server(Some("nope")).unwrap_err();
    assert_eq!(
        error.to_string(),
        "no language server named nope; configured: configured, registered"
    );
    assert!(context.validate_server(None).is_ok());
    assert!(context.validate_server(Some("configured")).is_ok());
    assert!(context.validate_server(Some("registered")).is_ok());
    assert!(context.validate_server(Some("elsewhere")).is_err());
    context.servers.clear();
    context.entries.clear();
    assert_eq!(
        context
            .validate_server(Some("nope"))
            .unwrap_err()
            .to_string(),
        "no language server named nope; configured: none configured"
    );
}

#[test]
fn exit_rank_uses_severity_not_argument_order() {
    let codes = [0, 5, 6, 7, 1, 4, 3];
    for (rank, code) in codes.iter().enumerate() {
        for other in &codes[..=rank] {
            assert_eq!(exit_rank(*code, *other), *code);
            assert_eq!(exit_rank(*other, *code), *code);
        }
    }
}

#[test]
fn query_failures_reuse_server_conditions_only() {
    for code in [1, 3, 4, 8, 9, 10] {
        let mut failures = QueryFailures::default();
        let mut requests = 0;
        for _ in 0..3 {
            let result = failures.execute("rust", || {
                requests += 1;
                Err(match code {
                    3 => QueryErr::Unavailable {
                        root: "/repo".into(),
                        reason: query::UnavailableReason::NotRunning,
                    },
                    4 => QueryErr::Indexing {
                        server: "rust".into(),
                        seconds: 30,
                    },
                    8 => QueryErr::MemoryShort {
                        root: "/repo".into(),
                        server: "rust".into(),
                        shortfall: None,
                    },
                    9 => QueryErr::MemoryPressure {
                        root: "/repo".into(),
                        server: "rust".into(),
                    },
                    10 => QueryErr::Unanswered {
                        server: "rust".into(),
                        seconds: 60,
                    },
                    _ => crate::lsp::LspErr::Protocol("bad request".into()).into(),
                })
            });
            assert_eq!(
                result.err().unwrap().exit_code(),
                if code >= 8 { 4 } else { code }
            );
        }
        assert_eq!(requests, if code == 1 { 3 } else { 1 });
        assert!(
            failures
                .execute("python", || Ok(query::Output::Answer {
                    result: serde_json::Value::Null,
                    document_uri: None
                }))
                .is_ok()
        );
    }
}

fn entry(root: &str, server: &str, state: State) -> registry::Entry {
    registry::Entry {
        kind: None,
        editor_check_on_save: None,
        root: root.into(),
        project: None,
        server: server.into(),
        nonce: String::new(),
        broker_pid: 1,
        broker_start_token: String::new(),
        server_pid: Some(2),
        server_start_token: None,
        state,
        started_at_ms: 0,
        ready_at_ms: None,
        estimate_bytes: 0,
        settings_hash: String::new(),
        request_count: 3,
        last_request_at_ms: None,
        peak_rss_kb: 4,
        restarts: 2,
        last_crash: None,
        leases: vec![],
        attached: vec![],
    }
}
