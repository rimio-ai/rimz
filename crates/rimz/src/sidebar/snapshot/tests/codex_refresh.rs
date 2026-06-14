use super::*;

#[test]
fn codex_rate_limit_refresh_targets_sessions_before_metered_idle_accounts() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());

    let mut active_with_windows = snapshot_with_panels(
        workspace.clone(),
        vec![provider_panel("codex", vec![rl_window(42, None)])],
    );
    active_with_windows
        .agents
        .push(root_agent("codex", "sess-active", Some("gpt-5.5-codex")));
    assert_eq!(
        codex_rate_limit_refreshes(&active_with_windows),
        vec![CodexRateLimitRefresh::Session {
            session_id: "sess-active".to_owned(),
            model_hint: Some("gpt-5.5-codex".to_owned()),
        }],
        "active Codex sessions refresh their sidecars even when the dashboard already has windows"
    );

    let idle_metered =
        snapshot_with_panels(workspace.clone(), vec![provider_panel("codex", Vec::new())]);
    assert_eq!(
        codex_rate_limit_refreshes(&idle_metered),
        vec![CodexRateLimitRefresh::Account],
        "idle Codex accounts refresh the shared cache even before windows exist"
    );

    let mut active_no_model =
        snapshot_with_panels(workspace.clone(), vec![provider_panel("codex", Vec::new())]);
    active_no_model
        .agents
        .push(root_agent("codex", "sess-active", None));
    assert_eq!(
        codex_rate_limit_refreshes(&active_no_model),
        vec![CodexRateLimitRefresh::Session {
            session_id: "sess-active".to_owned(),
            model_hint: None,
        }],
        "a stale account cache cannot refresh underneath a live Codex sidecar"
    );

    let mut unmetered_codex = provider_panel("codex", Vec::new());
    unmetered_codex.metered = false;
    let snapshot = snapshot_with_panels(
        workspace,
        vec![unmetered_codex, provider_panel("claude", Vec::new())],
    );

    assert!(
        codex_rate_limit_refreshes(&snapshot).is_empty(),
        "only metered Codex has an out-of-band budget read"
    );
}

/// The per-target throttle marker gates the out-of-band fetch: the first call is
/// due (and touches the marker), the immediate next is not.
#[test]
fn codex_rate_limit_probe_throttles_per_target() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let account = CodexRateLimitRefresh::Account;
    let session = CodexRateLimitRefresh::Session {
        session_id: "sess/one".to_owned(),
        model_hint: None,
    };
    let other_session = CodexRateLimitRefresh::Session {
        session_id: "sess/two".to_owned(),
        model_hint: None,
    };

    assert!(codex_rate_limit_probe_due(&runtime, &account));
    assert!(
        !codex_rate_limit_probe_due(&runtime, &account),
        "a freshly-stamped account backs off"
    );
    assert!(codex_rate_limit_probe_due(&runtime, &session));
    assert!(
        !codex_rate_limit_probe_due(&runtime, &session),
        "a freshly-stamped session backs off"
    );
    assert!(
        codex_rate_limit_probe_due(&runtime, &other_session),
        "a different session has its own marker"
    );

    let old = SystemTime::now()
        .checked_sub(CODEX_RATE_LIMIT_REFRESH_INTERVAL + Duration::from_secs(1))
        .unwrap();
    std::fs::File::open(codex_rate_limit_probe_marker(&runtime, &session))
        .unwrap()
        .set_modified(old)
        .unwrap();
    assert!(
        codex_rate_limit_probe_due(&runtime, &session),
        "the session becomes due again after the 60s interval"
    );
}

#[test]
fn codex_transcript_backstop_is_stat_gated() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let path = dir.path().join("rollout-session.jsonl");
    std::fs::write(
        &path,
        "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5\"}}\n\
             {\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\
             \"last_token_usage\":{\"input_tokens\":50,\"total_tokens\":60},\
             \"model_context_window\":100}}}\n",
    )
    .unwrap();

    let mut record = crate::ledger::agent_context::new_record(
        "codex",
        "sess-1",
        crate::ledger::agent_context::empty_context("codex", Timestamp::now()),
    );
    record.transcript_path = Some(path.to_string_lossy().into_owned());
    crate::ledger::agent_context::write_record(&runtime, &record).unwrap();

    refresh_codex_transcript_context(&runtime, "sess-1", Some("gpt-5"));
    let first = crate::ledger::agent_context::read_one(&runtime, "codex", "sess-1").unwrap();
    // The sidecar carries the derivation inputs (window + current usage), not a
    // baked percentage; the gauge derives 50% (50 of 100) downstream.
    let first_tokens = first
        .context
        .tokens
        .as_ref()
        .expect("first refresh writes tokens");
    assert_eq!(first_tokens.context_window_size, Some(100));
    assert_eq!(
        first_tokens
            .current_usage
            .as_ref()
            .and_then(|usage| usage.input_tokens),
        Some(50)
    );
    let observed_at = first.context.observed_at;
    let stat = first.transcript_stat;

    refresh_codex_transcript_context(&runtime, "sess-1", Some("gpt-5"));
    let second = crate::ledger::agent_context::read_one(&runtime, "codex", "sess-1").unwrap();
    assert_eq!(second.context.observed_at, observed_at);
    assert_eq!(second.transcript_stat, stat);

    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(
            b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\
              \"last_token_usage\":{\"input_tokens\":80,\"total_tokens\":90},\
              \"model_context_window\":100}}}\n",
        )
        .unwrap();
    refresh_codex_transcript_context(&runtime, "sess-1", Some("gpt-5"));
    let third = crate::ledger::agent_context::read_one(&runtime, "codex", "sess-1").unwrap();
    assert_eq!(
        third
            .context
            .tokens
            .as_ref()
            .and_then(|t| t.current_usage.as_ref())
            .and_then(|usage| usage.input_tokens),
        Some(80)
    );
    assert_ne!(third.transcript_stat, stat);
}

/// Only Codex exposes an account-scoped window read today; Claude's windows
/// have no source outside a live statusline, so it never triggers a fetch.
#[test]
fn only_codex_has_an_out_of_band_window_read() {
    assert!(provider_has_out_of_band_windows("codex"));
    assert!(!provider_has_out_of_band_windows("claude"));
    assert!(!provider_has_out_of_band_windows("pi"));
}

/// The config fold stamps every *agent* row's context-severity verdict from
/// the `[sidebar.context]` bands — the one classification the renderer's color
/// ramp and any future signal emitter read — and leaves process rows `None`.
#[test]
fn config_fold_stamps_agent_context_severity() {
    let agent_row = |pct: Option<u8>| crate::SidebarRow {
        id: "row".to_owned(),
        name: "claude".to_owned(),
        pane: None,
        worktree_path: None,
        worktree_branch: None,
        unread: false,
        inactive: false,
        last_activity: jiff::Timestamp::now(),
        card: crate::RowCard::Agent(Box::new(crate::AgentCard {
            status: Some(AgentStatus::Running),
            phase: TurnPhase::Idle,
            context_pct: pct,
            ..crate::AgentCard::default()
        })),
    };
    let process_row = || crate::SidebarRow {
        id: "row".to_owned(),
        name: "zsh".to_owned(),
        pane: None,
        worktree_path: None,
        worktree_branch: None,
        unread: false,
        inactive: false,
        last_activity: jiff::Timestamp::now(),
        card: crate::RowCard::Process(crate::ProcessCard::default()),
    };
    let mut groups = vec![crate::SidebarWorktreeGroup {
        key: "/repo/main".to_owned(),
        label: "main".to_owned(),
        kind: crate::SidebarWorktreeKind::Worktree,
        status_counts: Vec::new(),
        rows: vec![agent_row(Some(85)), agent_row(Some(5)), process_row()],
        hidden_count: 0,
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
        clean: None,
    }];

    stamp_context_severity(
        &mut groups,
        &crate::config::ContextSeverityConfig::default(),
    );

    let rows = &groups[0].rows;
    assert_eq!(
        rows[0].as_agent().and_then(|agent| agent.context_severity),
        Some(crate::feed::ContextSeverity::Amber),
        "85% crosses the default amber band"
    );
    assert_eq!(
        rows[1].as_agent().and_then(|agent| agent.context_severity),
        Some(crate::feed::ContextSeverity::Calm)
    );
    assert_eq!(
        rows[2].as_agent().and_then(|agent| agent.context_severity),
        None,
        "a process row carries no context verdict"
    );
}
