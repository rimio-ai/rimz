use super::*;

use crate::cli::render;
use rimz::feed::pending_ask_for;

pub(super) fn list_agents(
    json: bool,
    all: bool,
    worktree: Option<String>,
    globals: &GlobalFlags,
) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let ledger = crate::cli::open_ledger(&workspace)?;
    let runtime = rimz::RuntimePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing runtime paths")?;
    let context_records = rimz::ledger::agent_context::read_all(&runtime);

    let mut snapshot = ledger.snapshot_cached().context("reading agent snapshot")?;
    apply_cached_daemon_reap(&mut snapshot, &runtime, &workspace.session_name);
    // Group by the room's worktree checkouts the way the sidebar does: a
    // worktree parked outside the project root still earns its own pod. The
    // cached enumeration is read-only and best-effort, matching the sidebar's
    // consumer path; `--json` skips it since the flat array never groups.
    if !json && snapshot.project_root.is_some() {
        snapshot =
            snapshot.with_worktree_roots(rimz::sidebar::enrich::cached_worktree_roots(&runtime));
    }
    // Fold each session's rich statusline context so the `CTX` column reads the
    // real used/window fill, not the carried-forward `context_pct`.
    let snapshot = snapshot.with_agent_context(context_records);

    let channel = list_channel_filter(all, worktree.as_deref(), &workspace);
    let agents: Vec<&AgentState> = snapshot
        .agents
        .iter()
        .filter(|agent| agent.parent_agent_id.is_none())
        .filter(|agent| {
            channel
                .as_deref()
                .is_none_or(|filter| rimz::harness::target::agent_in_worktree(agent, filter))
        })
        .collect();
    if json {
        supervised::output::print_json(&agents)?;
        return Ok(());
    }

    let mut out = render::out();
    render_agents_table(&mut out, &snapshot, &agents, jiff::Timestamp::now())?;
    Ok(())
}

pub(crate) fn render_agents_table(
    w: &mut impl std::io::Write,
    snapshot: &rimz::SidebarSnapshot,
    agents: &[&AgentState],
    now: jiff::Timestamp,
) -> std::io::Result<()> {
    let groups = rimz::ledger::snapshot::group_live_agents_by_worktree(agents, snapshot);
    let ordered_agents: Vec<&AgentState> = groups
        .iter()
        .flat_map(|group| group.agents.iter().copied())
        .collect();
    let mut table = render::Table::new([
        "AGENT", "STATUS", "CHANNEL", "MODEL", "CTX", "TOKENS", "AGE",
    ])
    .right(&[4, 5, 6]);
    for &agent in &ordered_agents {
        table.row(agent_row(agent, &ordered_agents, now));
    }
    table.render(w)
}

fn list_channel_filter(
    all: bool,
    worktree: Option<&str>,
    workspace: &rimz::ResolvedWorkspace,
) -> Option<String> {
    list_channel_filter_for_current(all, worktree, crate::cli::current_channel(workspace))
}

fn list_channel_filter_for_current(
    all: bool,
    worktree: Option<&str>,
    current_channel: Option<String>,
) -> Option<String> {
    match (worktree, all) {
        (Some(worktree), _) => Some(worktree.to_owned()),
        (None, true) => None,
        (None, false) => current_channel,
    }
}

pub(super) fn show_agent(
    reference: String,
    json: bool,
    capture: bool,
    ansi: bool,
    globals: &GlobalFlags,
) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let ledger = crate::cli::open_ledger(&workspace)?;
    let runtime = rimz::RuntimePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing runtime paths")?;
    // Fold the rich statusline context so the shown card — and the `--json`
    // payload — carries the real token window, not the carried-forward
    // `context_pct`.
    let mut snapshot = ledger.snapshot_cached().context("reading agent snapshot")?;
    apply_cached_daemon_reap(&mut snapshot, &runtime, &workspace.session_name);
    let snapshot = snapshot.with_agent_context(rimz::ledger::agent_context::read_all(&runtime));
    let agent_result = crate::cli::resolve_agent_one(
        &snapshot,
        &reference,
        None,
        crate::cli::current_channel(&workspace).as_deref(),
    );
    let mut agent = agent_result.as_ref().ok().map(|agent| (*agent).clone());
    let mut stale = false;
    let mut audit_error = None;
    if agent.is_none() {
        match resolve_audit_agent(&ledger, &workspace, &runtime, &reference) {
            Ok(Some(audit_agent)) => {
                agent = Some(audit_agent);
                stale = true;
            }
            Ok(None) => {}
            Err(err) => audit_error = Some(err),
        }
    }
    let run = newest_run_by_ref(&ledger, &reference, agent.as_ref())?;
    let feed_items = ledger.list_feed_items()?;
    let ask_item = agent
        .as_ref()
        .and_then(|agent| pending_ask_for(agent, feed_items.iter()));
    let ask = ask_item.map(crate::cli::transcript::ask_view);
    let pane_capture = if capture {
        let agent = agent.as_ref().ok_or_else(|| {
            anyhow::anyhow!("no live agent matches `{reference}`; nothing to capture")
        })?;
        let pane = agent
            .pane
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("agent {} has no bound pane", agent_name(agent)))?;
        let backend = rimz::mux::backend_for(pane.pane_id.mux());
        Some(
            backend
                // rimz-invariant: explicit-agent-show-capture
                .capture_pane(&pane.pane_id, None, ansi)
                .context("capturing agent pane")?,
        )
    } else {
        None
    };
    if json {
        #[derive(serde::Serialize)]
        struct Show<'a> {
            #[serde(skip_serializing_if = "Option::is_none")]
            agent: Option<&'a AgentState>,
            #[serde(skip_serializing_if = "is_false")]
            stale: bool,
            #[serde(skip_serializing_if = "Option::is_none")]
            run: Option<RunRecord>,
            #[serde(skip_serializing_if = "Option::is_none")]
            ask: Option<crate::cli::transcript::AskView>,
            #[serde(skip_serializing_if = "Option::is_none")]
            capture: Option<rimz::mux::PaneCapture>,
        }
        supervised::output::print_json(&Show {
            agent: agent.as_ref(),
            stale,
            run,
            ask,
            capture: pane_capture,
        })?;
        return Ok(());
    }
    let Some(agent) = agent.as_ref() else {
        if let Some(run) = run {
            print_run_line(&run)?;
            return Ok(());
        }
        if let Some(err) = audit_error {
            return Err(err);
        }
        return match agent_result {
            Err(err) => Err(err),
            Ok(_) => unreachable!("agent is present above"),
        };
    };
    let mut kv = render::KeyVals::new();
    let peers: Vec<&AgentState> = snapshot
        .agents
        .iter()
        .filter(|candidate| candidate.parent_agent_id.is_none())
        .collect();
    kv.push(
        "agent",
        render::cell(rimz::harness::target::agent_handle(agent, &peers, true))
            .fg(render::palette::ACCENT),
    );
    kv.push(
        "kind",
        render::cell(agent.kind.to_string()).fg(render::palette::META),
    );
    if let Some(profile) = agent.profile.as_deref() {
        kv.push("profile", render::cell(profile).fg(render::palette::META));
    }
    if let Some(name) = agent.name.as_deref() {
        kv.push("name", render::cell(name));
    }
    kv.push("session", render::cell(agent.agent_id.to_string()));
    kv.push(
        "status",
        render::cell(agent_status_label(agent)).fg(agent_status_style(agent)),
    );
    if let Some((_, label)) = agent.displayed_turn_error() {
        kv.push(
            "error",
            render::cell(label.unwrap_or("provider API error")).fg(render::palette::ALARM),
        );
    }
    if let Some(ask) = ask.as_ref() {
        kv.push(
            "ask",
            render::cell(crate::cli::transcript::ask_summary(ask)).fg(render::palette::WARN),
        );
    }
    if stale {
        kv.push(
            "lifecycle",
            render::cell("stale").fg(render::palette::FAINT),
        );
    }
    kv.push("model", render::cell(model_label(agent)).dash());
    kv.push("context", context_cell(agent));
    kv.push("worktree", render::cell(worktree_label(agent)).dash());
    push_pane_anchor(&mut kv, agent);
    if let Some(registered_at) = agent.registered_at {
        kv.push("registered_at", render::cell(registered_at.to_string()));
    }
    kv.render(&mut render::out())?;
    if let Some(run) = run.or_else(|| newest_run_for_agent(&ledger, agent).ok().flatten()) {
        print_run_line(&run)?;
    }
    if let Some(capture) = pane_capture {
        use std::io::Write;

        let mut out = render::out();
        writeln!(
            out,
            "{} {}",
            render::paint(render::palette::MUTED, "capture:"),
            capture.pane_id,
        )?;
        write!(out, "{}", capture.raw_text)?;
    }
    Ok(())
}

pub(super) fn apply_cached_daemon_reap(
    snapshot: &mut rimz::SidebarSnapshot,
    runtime: &rimz::RuntimePaths,
    session: &str,
) {
    let cache = rimz::sidebar::refresh::read_codex_daemon_reap(runtime).unwrap_or_default();
    let live_panes = rimz::sidebar::cache::read_snapshot_cache(&runtime.pane_frame_path(), session)
        .map(|frame| rimz::SidebarSnapshot::card_admitted_live_panes(frame.to_pane_refs(), None));
    snapshot.reap_runtime(rimz::ledger::snapshot::RuntimeReapInputs {
        daemon_pids: &cache.daemon_pids,
        loaded: cache.loaded.as_ref(),
        live_panes: live_panes.as_deref(),
    });
}

fn resolve_audit_agent(
    ledger: &rimz::Ledger,
    workspace: &rimz::ResolvedWorkspace,
    runtime: &rimz::RuntimePaths,
    reference: &str,
) -> Result<Option<AgentState>> {
    let audit = ledger
        .runtime_projection(rimz::RuntimeScope::Audit)
        .context("reading audit agent rollup")?;
    if audit.agents.is_empty() {
        return Ok(None);
    }
    let snapshot = rimz::SidebarSnapshot::build_with_agents(
        workspace.workspace_id.clone(),
        Vec::new(),
        audit.agents,
        jiff::Timestamp::now(),
    )
    .with_agent_context(rimz::ledger::agent_context::read_all(runtime));
    match crate::cli::resolve_agent_one(&snapshot, reference, None, None) {
        Ok(agent) => Ok(Some(agent.clone())),
        Err(err) => Err(err),
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

pub(super) fn focus_agent(reference: String, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let ledger = crate::cli::open_ledger(&workspace)?;
    let snapshot = ledger.snapshot_cached().context("reading agent snapshot")?;
    let agent = crate::cli::resolve_agent_one(
        &snapshot,
        &reference,
        None,
        crate::cli::current_channel(&workspace).as_deref(),
    )?;
    let pane = agent
        .pane
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("agent {} has no bound pane", agent_name(agent)))?;
    let backend = rimz::mux::backend_for(pane.pane_id.mux());
    backend
        .focus_pane(&pane.pane_id, Some(&workspace.session_name))
        .map_err(Into::into)
}

pub(super) fn wait_agent(
    reference: String,
    timeout: Option<Duration>,
    stream_output: bool,
    from_start: bool,
    json: bool,
    globals: &GlobalFlags,
) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let ledger = crate::cli::open_ledger(&workspace)?;
    let snapshot = ledger.snapshot_cached().context("reading agent snapshot")?;
    let live_agent = crate::cli::resolve_agent_one(
        &snapshot,
        &reference,
        None,
        crate::cli::current_channel(&workspace).as_deref(),
    )
    .ok();
    if let Some(run) = newest_run_by_ref(&ledger, &reference, live_agent)?
        && (!run.status.is_terminal() || live_agent.is_none() || run.run_id.as_str() == reference)
    {
        return wait_run_record(&ledger, &run, timeout, stream_output, from_start, json);
    }
    if live_agent.is_none() {
        crate::cli::resolve_agent_one(
            &snapshot,
            &reference,
            None,
            crate::cli::current_channel(&workspace).as_deref(),
        )?;
    }
    let deadline = timeout.map(|duration| Instant::now() + duration);
    loop {
        let snapshot = ledger.snapshot_cached().context("reading agent snapshot")?;
        let agent = crate::cli::resolve_agent_one(
            &snapshot,
            &reference,
            None,
            crate::cli::current_channel(&workspace).as_deref(),
        )?;
        if gate_open(DeliveryGate::Done, agent.status) {
            if json {
                supervised::output::print_json(agent)?;
            }
            std::process::exit(0);
        }
        if agent.status == rimz::agents::AgentStatus::Failed {
            if json {
                supervised::output::print_json(agent)?;
            }
            std::process::exit(RunStatus::Failed.exit_code());
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            std::process::exit(RunStatus::TimedOut.exit_code());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

pub(super) fn stop_agent(reference: String, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let ledger = crate::cli::open_ledger(&workspace)?;
    let snapshot = ledger.snapshot_cached().context("reading agent snapshot")?;
    let live_agent = crate::cli::resolve_agent_one(
        &snapshot,
        &reference,
        None,
        crate::cli::current_channel(&workspace).as_deref(),
    )
    .ok();
    if let Some(run) = newest_run_by_ref(&ledger, &reference, live_agent)? {
        if run_stop_should_cancel(&run) {
            let (record, wrote) = rimz::harness::run::cancel(ledger.paths(), &run.run_id)?;
            if wrote {
                rimz::ledger::wakeup::wake_run(ledger.runtime_paths(), &record)
                    .context("waking run waiter")?;
            }
        }
        if let Ok(backend) = supervised::pane::backend_for_workspace_session(&workspace, globals) {
            supervised::pane::close_stopped_run_pane_after_grace(
                backend.as_ref(),
                &ledger,
                &workspace.session_name,
                &run,
                supervised::pane::STOP_BACKSTOP_GRACE,
            );
        }
        return Ok(());
    }
    let agent = match live_agent {
        Some(agent) => agent,
        None => crate::cli::resolve_agent_one(
            &snapshot,
            &reference,
            None,
            crate::cli::current_channel(&workspace).as_deref(),
        )?,
    };
    close_agent_pane(&workspace, agent)
}

/// Whether `stop` must cancel a run's supervision before reclaiming its pane.
/// Live runs are canceled so a blocking `-p` waiter wakes. Terminal runs, such
/// as completed `--keep` agents, keep their record and only lose the pane.
pub(super) fn run_stop_should_cancel(run: &RunRecord) -> bool {
    !run.status.is_terminal()
}

fn close_agent_pane(workspace: &rimz::ResolvedWorkspace, agent: &AgentState) -> Result<()> {
    let pane = agent
        .pane
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("agent {} has no bound pane", agent_name(agent)))?;
    let backend = rimz::mux::backend_for(pane.pane_id.mux());
    backend
        .close_pane(&workspace.session_name, &pane.pane_id)
        .map_err(Into::into)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RunPlacement {
    Split,
    Tab,
}

/// A supervised `-p` run hosts its agent pane in a split of the current tab so
/// focus stays with the caller; it opens a new tab only when forced or when
/// there is no ambient pane to split.
pub(super) fn run_placement(force_new_tab: bool, has_ambient_pane: bool) -> RunPlacement {
    if force_new_tab || !has_ambient_pane {
        RunPlacement::Tab
    } else {
        RunPlacement::Split
    }
}

pub(super) fn run_print(args: AgentsArgs, globals: &GlobalFlags) -> Result<Option<RunRecord>> {
    let output_format = args.output_format.unwrap_or_default();
    let record = run_supervised(args, globals)?;
    let Some(record_ref) = record.as_ref() else {
        return Ok(record);
    };
    match output_format {
        OutputFormat::Text => {
            let mut stdout = render::out();
            let mut stderr = render::err();
            supervised::output::print_run_output(record_ref, &mut stdout, &mut stderr)?
        }
        OutputFormat::Json => supervised::output::print_json(record_ref)?,
        // stream-json already emitted its events as the run progressed.
        OutputFormat::StreamJson => {}
    }
    Ok(record)
}

pub(super) fn run_supervised(args: AgentsArgs, globals: &GlobalFlags) -> Result<Option<RunRecord>> {
    if args.json {
        bail!("on `-p`, choose output with `--output-format json` (`--json` is for `list`)");
    }
    let output_format = args.output_format.unwrap_or_default();
    let input_format = args.input_format.unwrap_or_default();
    if args.detach && output_format == OutputFormat::StreamJson {
        bail!("--output-format stream-json cannot be combined with --detach");
    }
    let prompt = resolve_print_prompt(&args, input_format)?;
    let workspace = supervised::resolve_run_workspace(globals)?;
    let machine_config = crate::cli::machine_config();
    let mode = supervised_permission_mode_from_flags(args.ask, args.yolo)?;
    let PreparedLaunch {
        profiles: _profiles,
        teams: _teams,
        mut layout,
        team_name: _team_name,
    } = prepare_launch_layout(&args, &workspace, &machine_config, Some(mode), None)?;
    if let Some(limit) = args.max_turns {
        apply_supervised_turn_limit(&mut layout, limit)?;
    }
    let agent_cells = agent_cells(&layout);
    if agent_cells.len() != 1 {
        bail!("--print requires a layout with exactly one agent cell");
    }
    if layout_cell_count(&layout) != 1 {
        bail!("--print requires a single-cell agent layout");
    }
    let agent_cell = agent_cells[0];
    let adapter = rimz::agents::find_adapter(agent_cell.kind)
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", agent_cell.kind))?;
    let launch_invocation = rimz::harness::launch::ExecInvocation {
        kind: agent_cell.kind,
        action: rimz::harness::launch::ExecAction::Launch {
            prompt: Some(&prompt),
            extra_args: &[],
        },
        run_id: None,
        worktree_path: None,
        close_pane_on_exit: false,
        exit_on_run_completion: false,
        identity: rimz::harness::launch::ExecIdentity {
            profile: agent_cell.profile,
            role: agent_cell.role,
            channel: args.channel.as_deref(),
            model: agent_cell.model,
            effort: agent_cell.effort,
            ..rimz::harness::launch::ExecIdentity::default()
        },
    };
    let launch_env = full_agent_launch_env(
        &workspace.project_root,
        adapter,
        machine_config.harness.rtk,
        &launch_invocation,
    )?;
    supervised::preflight_agent(adapter)?;
    supervised::preflight_program(adapter, agent_cell.args, &prompt, &launch_env)?;

    let launch = crate::cli::agents_launch::resolve_cwd(
        &workspace,
        &machine_config.agents.worktree,
        args.worktree.as_deref(),
        args.from_pr.as_ref(),
    )?;
    let mux = rimz::mux::auto_detect_backend(globals.mux)?;
    let backend = rimz::mux::backend_for(mux);
    let mux_config = rimz::config::MultiplexerConfig::from(&machine_config);
    let width = rimz::mux::SidebarWidth::from_config(&machine_config.theme.display);
    let detected_size = rimz::mux::detect_terminal_size();
    let was_live = backend.list_sessions()?.contains(&workspace.session_name);
    let ledger = crate::cli::open_ledger(&workspace)?;
    if let Some(channel) = args.channel.as_deref() {
        crate::cli::channel::ensure_named_channel_available(&workspace, channel)?;
        rimz::channel::register(ledger.paths(), channel)?;
    }
    backend.ensure_session(&rimz::mux::SessionOptions {
        session_name: workspace.session_name.clone(),
        workspace_id: workspace.workspace_id.clone(),
        project_root: workspace.project_root.clone(),
        cwd: launch.cwd.clone(),
        config: mux_config.clone(),
        detected_size,
        truecolor: rimz::tui::truecolor(),
    })?;
    // A supervised run can birth the room, so the focus chord registers here
    // too (tmux binds it; Zellij routes it through the presence plugin below) —
    // the key reaches the sidebar from any pane regardless of how the room came
    // to be.
    crate::cli::room::register_focus_key(backend.as_ref(), &machine_config);
    let room = RoomTarget {
        workspace_id: &workspace.workspace_id,
        project_root: &workspace.project_root,
        session_name: &workspace.session_name,
        cwd: &launch.cwd,
        mux_config: &mux_config,
        width,
        detected_size: if was_live { None } else { detected_size },
        refresh_ms: None,
    };
    crate::cli::room::launch_sidebar_for_workspace(backend.as_ref(), &room, None, &[]);
    crate::cli::room::gate_room_before_attach(backend.as_ref(), &room, None, &[])?;
    crate::cli::room::ensure_presence_plugin(
        backend.as_ref(),
        &workspace.session_name,
        &workspace.workspace_id,
        &mux_config.zellij,
        machine_config.sidebar.focus_key_label(),
    );

    let permission_mode = agent_cell.mode.unwrap_or(mode);
    let mut record = RunRecord::new(
        workspace.workspace_id.clone(),
        AgentKind::new_unchecked(adapter.descriptor().kind),
        permission_mode,
        prompt.clone(),
        launch.cwd.clone(),
    );
    let run_id = record.run_id.clone();
    let launch_requests = launch_identity_requests(
        &layout,
        args.name.as_deref(),
        generated_worktree_name(&launch),
        None,
        None,
        args.channel.as_deref(),
    )?;
    let launch_requests = launch_requests
        .into_iter()
        .map(|mut request| {
            request.run_id = Some(run_id.clone());
            request
        })
        .collect::<Vec<_>>();
    let mut launch_identities = ledger.append_agent_launches_allocating(
        &launch_requests,
        &AgentLaunchAppend {
            workspace_id: workspace.workspace_id.clone(),
            session_name: workspace.session_name.clone(),
            cwd: launch.cwd.clone(),
            worktree_name: launch.worktree_name.clone(),
            channel: args.channel.clone(),
            prompt: Some(prompt.clone()),
            description: args.description.clone(),
            state: rimz::ledger::event::AgentLaunchState::Starting,
            pane_id: None,
        },
    )?;
    let launch_identity = launch_identities
        .pop()
        .ok_or_else(|| anyhow::anyhow!("--print requires one agent cell"))?;
    record.agent_name = Some(launch_identity.name.clone());
    let pane = supervised::run_pane_cmd(supervised::RunPaneCmdArgs {
        adapter,
        run_id: &run_id,
        agent_name: Some(&launch_identity.name),
        agent_profile: agent_cell.profile,
        agent_role: agent_cell.role,
        agent_model: agent_cell.model,
        agent_effort: agent_cell.effort,
        launch_id: Some(&launch_identity.agent_id),
        cwd: &launch.cwd,
        prompt: &prompt,
        cleanup_worktree: args.worktree.is_some() || args.from_pr.is_some(),
        permission_args: agent_cell.args,
        self_cleanup_on_completion: args.detach && !args.keep,
    })?;
    let bound = if args.detach {
        None
    } else {
        Some(bridge::bind_run(ledger.runtime_paths(), &run_id).context("binding run socket")?)
    };
    let socket_guard = bound
        .as_ref()
        .map(|(_sock, sock_path)| SocketGuard::new(sock_path.clone()));
    rimz::harness::run::create(ledger.paths(), &record).context("recording run")?;
    let target = own_pane_id(mux);
    let open_result = match run_placement(args.new_tab, target.is_some()) {
        RunPlacement::Split => backend.split_pane(SplitPaneOptions {
            target_pane_id: target,
            cwd: Some(launch.cwd.to_string_lossy().into_owned()),
            command: Some(pane.argv.clone()),
            env: crate::cli::agents_launch::launch_identity_env(
                &workspace,
                args.channel.as_deref(),
                args.worktree.is_none() && args.from_pr.is_none(),
            ),
            focus: false,
        }),
        RunPlacement::Tab => backend.open_tab(&TabOptions {
            session_name: workspace.session_name.clone(),
            title: format!("run {}", adapter.descriptor().kind),
            cwd: launch.cwd.clone(),
            panes: LayoutPanes {
                columns: vec![LayoutColumn {
                    panes: vec![pane],
                    stacked: false,
                }],
            },
            focus: false,
            dock_sidebar: true,
            sidebar: crate::cli::room::build_sidebar_opts(&room, Vec::new())?,
        }),
    };
    if let Err(err) = open_result {
        let _ = rimz::harness::run::fail(ledger.paths(), &run_id);
        let _ = append_launch_event(
            &ledger,
            &workspace,
            &launch_identity,
            LaunchEventParams {
                cwd: &launch.cwd,
                worktree_name: launch.worktree_name.as_deref(),
                channel: args.channel.as_deref(),
                prompt: Some(&prompt),
                state: rimz::ledger::event::AgentLaunchState::Failed,
                pane_id: None,
            },
        );
        return Err(err).context("opening run pane");
    }
    if args.detach {
        #[expect(clippy::print_stdout, reason = "command result is the agent name")]
        {
            println!("{}", launch_identity.name);
        }
        return Ok(None);
    }
    let Some((sock, _sock_path)) = bound else {
        bail!("blocking run did not bind its completion socket");
    };
    let expected = ExpectedRunFrame {
        workspace_id: workspace.workspace_id.clone(),
        run_id: run_id.clone(),
    };
    let mut record = if output_format == OutputFormat::StreamJson {
        supervised::stream::stream_blocking_run(
            sock,
            expected,
            &ledger,
            &run_id,
            adapter,
            args.timeout,
        )?
    } else {
        let outcome = supervised::wait_for_run(sock, expected, args.timeout)?;
        supervised::terminal_record_after_wait(ledger.paths(), &run_id, outcome)?
    };
    record = record_failure_tail_before_cleanup(
        backend.as_ref(),
        &ledger,
        &workspace.session_name,
        record,
    );
    if !args.keep {
        supervised::pane::close_run_pane(
            backend.as_ref(),
            &ledger,
            &workspace.session_name,
            &record,
        );
    }
    drop(socket_guard);
    Ok(Some(record))
}

fn record_failure_tail_before_cleanup(
    backend: &dyn rimz::mux::MuxBackend,
    ledger: &rimz::Ledger,
    session_name: &str,
    record: RunRecord,
) -> RunRecord {
    if record.status == RunStatus::Completed || record.failure_tail.is_some() {
        return record;
    }
    let Some(pane) = supervised::pane::resolve_run_pane(ledger, session_name, &record) else {
        return record;
    };
    let Some(tail) = supervised::pane::capture_failure_tail(backend, &pane.pane_id) else {
        return record;
    };
    match rimz::harness::run::record_failure_tail(ledger.paths(), &record.run_id, &tail) {
        Ok(record) => record,
        Err(err) => {
            tracing::debug!(
                run_id = %record.run_id,
                pane = %pane.pane_id,
                error = %err,
                "could not record supervised run failure pane tail",
            );
            record
        }
    }
}

/// Resolve the supervised prompt from text input or, for `--input-format
/// stream-json`, from stream-json user messages on stdin.
fn resolve_print_prompt(args: &AgentsArgs, input_format: InputFormat) -> Result<String> {
    match input_format {
        InputFormat::Text => {
            let piped = supervised::read_piped_text_prompt()?;
            supervised::combine_text_prompt(args.prompt.as_deref(), piped.as_deref())
        }
        InputFormat::StreamJson => {
            if args.prompt.as_deref().is_some_and(|p| !p.trim().is_empty()) {
                bail!(
                    "--input-format stream-json reads the prompt from stdin; drop the positional PROMPT"
                );
            }
            let prompt = supervised::read_stream_json_prompt(std::io::stdin().lock())
                .context("reading stream-json prompt from stdin")?;
            if prompt.trim().is_empty() {
                bail!("--input-format stream-json received no user message text on stdin");
            }
            Ok(prompt)
        }
    }
}

fn wait_run_record(
    ledger: &rimz::Ledger,
    run: &RunRecord,
    timeout: Option<Duration>,
    stream_output: bool,
    from_start: bool,
    json: bool,
) -> Result<()> {
    let adapter = rimz::agents::find_adapter(run.kind.as_str())
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", run.kind))?;
    if stream_output {
        match supervised::stream::stream_attached_run(
            ledger,
            &run.run_id,
            adapter,
            from_start,
            timeout,
        )? {
            Some(record) => std::process::exit(record.status.exit_code()),
            None => std::process::exit(RunStatus::TimedOut.exit_code()),
        }
    }
    let deadline = timeout.map(|duration| Instant::now() + duration);
    loop {
        let current = rimz::harness::run::load(ledger.paths(), &run.run_id)?;
        if current.status.is_terminal() {
            if json {
                supervised::output::print_json(&current)?;
            }
            std::process::exit(current.status.exit_code());
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            std::process::exit(RunStatus::TimedOut.exit_code());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn newest_run_for_agent(ledger: &rimz::Ledger, agent: &AgentState) -> Result<Option<RunRecord>> {
    newest_run_by_ref(ledger, agent.name.as_deref().unwrap_or(""), Some(agent))
}

fn newest_run_by_ref(
    ledger: &rimz::Ledger,
    reference: &str,
    agent: Option<&AgentState>,
) -> Result<Option<RunRecord>> {
    let mut records = rimz::harness::run::list(ledger.paths())?;
    records.retain(|record| {
        if record.run_id.as_str() == reference || record.agent_name.as_deref() == Some(reference) {
            return true;
        }
        if let Some(agent) = agent {
            return record.kind == agent.kind
                && (record.agent_id.as_ref() == Some(&agent.agent_id)
                    || record.agent_name.as_deref() == agent.name.as_deref());
        }
        false
    });
    records.sort_by_key(|record| std::cmp::Reverse(record.started_at));
    Ok(records.into_iter().next())
}

fn print_run_line(run: &RunRecord) -> std::io::Result<()> {
    use std::io::Write;
    let status = supervised::output::status_label(run.status);
    writeln!(
        render::out(),
        "{} {} {} {}",
        render::paint(render::palette::MUTED, "run:"),
        run.run_id,
        render::paint(render::status::run(run.status), status),
        run.prompt,
    )
}

/// The columns shared by `agents list` in display order. The lead `AGENT` cell
/// omits `#channel`; the `CHANNEL` cell carries that scope.
fn agent_row(agent: &AgentState, peers: &[&AgentState], now: jiff::Timestamp) -> Vec<render::Cell> {
    vec![
        render::cell(rimz::harness::target::agent_handle(agent, peers, false))
            .fg(render::palette::ACCENT),
        render::cell(agent_status_label(agent)).fg(agent_status_style(agent)),
        render::cell(worktree_label(agent)).dash(),
        model_cell(agent),
        context_cell(agent),
        render::cell(tokens_label(agent)).dash(),
        render::cell(render::age_short(agent.last_seen, now)),
    ]
}

/// Context fill warms as it climbs: gold past 75%, rose past 90%.
fn context_cell(agent: &AgentState) -> render::Cell {
    let pct = agent.context_fill_pct();
    let text = pct
        .map(|pct| format!("{}%", pct.round() as u8))
        .unwrap_or_else(|| "-".to_owned());
    let c = render::cell(text);
    match pct {
        Some(pct) if pct >= 90.0 => c.fg(render::palette::ALARM),
        Some(pct) if pct >= 75.0 => c.fg(render::palette::WARN),
        Some(_) => c,
        None => c.dash(),
    }
}

/// A launchable agent cell from a resolved layout.
#[derive(Clone, Copy)]
struct AgentCell<'a> {
    kind: &'a str,
    args: &'a [String],
    mode: Option<PermissionMode>,
    profile: Option<&'a str>,
    role: Option<&'a str>,
    model: Option<&'a str>,
    effort: Option<&'a str>,
}

fn agent_cells(layout: &LayoutSpec) -> Vec<AgentCell<'_>> {
    layout
        .agent_cells()
        .filter_map(|cell| match cell {
            Cell::Agent {
                kind,
                args,
                mode,
                profile,
                role,
                model,
                effort,
                ..
            } => Some(AgentCell {
                kind: kind.as_str(),
                args: args.as_slice(),
                mode: *mode,
                profile: profile.as_deref(),
                role: role.as_deref(),
                model: model.as_deref(),
                effort: effort.as_deref(),
            }),
            Cell::Command { .. } => None,
        })
        .collect()
}

fn layout_cell_count(layout: &LayoutSpec) -> usize {
    layout.columns.iter().map(|column| column.rows.len()).sum()
}

fn agent_name(agent: &AgentState) -> &str {
    agent.name.as_deref().unwrap_or(agent.agent_id.as_str())
}

fn agent_status_label(agent: &AgentState) -> String {
    let (status, phase) = agent_status_projection(agent);
    if phase == rimz::agents::TurnPhase::Idle {
        status.as_str().to_owned()
    } else {
        format!("{}:{}", status.as_str(), phase_label(phase))
    }
}

fn agent_status_style(agent: &AgentState) -> anstyle::Style {
    let (status, phase) = agent_status_projection(agent);
    render::status::agent(status, phase)
}

fn agent_status_projection(
    agent: &AgentState,
) -> (rimz::agents::AgentStatus, rimz::agents::TurnPhase) {
    match agent.displayed_turn_error().map(|(class, _)| class) {
        Some(rimz::agents::TurnErrorClass::PausedRateLimit)
        | Some(rimz::agents::TurnErrorClass::PausedSpendLimit)
        | Some(rimz::agents::TurnErrorClass::PausedOverloaded) => (
            rimz::agents::AgentStatus::Paused,
            rimz::agents::TurnPhase::Idle,
        ),
        Some(rimz::agents::TurnErrorClass::Failed) => (
            rimz::agents::AgentStatus::Failed,
            rimz::agents::TurnPhase::Idle,
        ),
        None => (agent.status, agent.phase),
    }
}

fn phase_label(phase: rimz::agents::TurnPhase) -> &'static str {
    match phase {
        rimz::agents::TurnPhase::Idle => "idle",
        rimz::agents::TurnPhase::Reasoning => "reasoning",
        rimz::agents::TurnPhase::Acting => "acting",
        rimz::agents::TurnPhase::Parked => "parked",
    }
}

fn model_label(agent: &AgentState) -> String {
    match (agent.model.as_deref(), agent.effort.as_deref()) {
        (Some(model), Some(effort)) => format!("{model}@{effort}"),
        (Some(model), None) => model.to_owned(),
        (None, Some(effort)) => format!("auto@{effort}"),
        (None, None) => "-".to_owned(),
    }
}

fn model_cell(agent: &AgentState) -> render::Cell {
    let label = model_label(agent);
    if label == "-" {
        return render::cell(label).dash();
    }
    match brand_style(agent.kind.as_str()) {
        Some(style) => render::cell(label).fg(style),
        None => render::cell(label),
    }
}

/// The agent kind's brand tone for truecolor output, or `None` for an unknown kind.
fn brand_style(kind: &str) -> Option<anstyle::Style> {
    let (r, g, b) = rimz::agents::descriptor_by_kind(kind)?.brand.color_rgb;
    Some(render::palette::rgb((r, g, b)))
}

fn tokens_label(agent: &AgentState) -> String {
    agent
        .total_tokens
        .map(compact_count)
        .unwrap_or_else(|| "-".to_owned())
}

fn compact_count(value: u64) -> String {
    if value < 1_000 {
        value.to_string()
    } else if value < 1_000_000 {
        format!("{}k", value / 1_000)
    } else {
        format!("{}m", value / 1_000_000)
    }
}

/// The agent's channel for display, dashed when it runs outside any worktree.
/// The channel itself comes from [`rimz::harness::target::agent_channel`], the single
/// source of truth; this only chooses the `-` placeholder over the resolver's
/// prose label.
fn worktree_label(agent: &AgentState) -> String {
    rimz::harness::target::agent_channel(agent).unwrap_or_else(|| "-".to_owned())
}

fn push_pane_anchor(kv: &mut render::KeyVals, agent: &AgentState) {
    let Some(pane) = agent.pane.as_ref() else {
        kv.push("pane", render::cell("-").dash());
        return;
    };
    kv.push("pane", render::cell(pane.pane_id.to_string()));
    if let Some(view_id) = pane.view_id.as_deref() {
        kv.push("tab", render::cell(view_id));
    }
    if let Some(cwd) = pane.cwd.as_deref() {
        kv.push("pane_cwd", render::cell(cwd));
    }
    if let Some(pid) = pane.pane_pid {
        kv.push("pane_pid", render::cell(pid.to_string()));
    }
    if let Some(start) = pane.pane_process_start {
        kv.push("pane_process_start", render::cell(start.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_channel_filter_resolves_explicit_all_and_current_channel() {
        assert_eq!(
            list_channel_filter_for_current(true, Some("manual"), Some("feature".to_owned()))
                .as_deref(),
            Some("manual")
        );
        assert_eq!(
            list_channel_filter_for_current(true, None, Some("feature".to_owned())),
            None
        );
        assert_eq!(
            list_channel_filter_for_current(false, None, Some("feature".to_owned())).as_deref(),
            Some("feature")
        );
        assert_eq!(list_channel_filter_for_current(false, None, None), None);
    }

    #[test]
    fn brand_style_uses_registered_agent_brand_rgb() {
        for kind in ["claude", "codex"] {
            let expected = rimz::agents::descriptor_by_kind(kind)
                .expect("registered descriptor")
                .brand
                .color_rgb;
            assert_eq!(brand_style(kind), Some(render::palette::rgb(expected)));
        }

        assert_eq!(brand_style("unknown"), None);
    }
}
