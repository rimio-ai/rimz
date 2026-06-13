use super::*;

use crate::cli::render;

pub(super) fn list_agents(
    json: bool,
    all: bool,
    worktree: Option<String>,
    globals: &GlobalFlags,
) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let ledger = crate::cli::open_ledger(&workspace)?;
    let agents = if all {
        ledger
            .runtime_projection(rimz::RuntimeScope::Audit)
            .context("reading audit agent rollup")?
            .agents
    } else {
        ledger
            .snapshot_cached()
            .context("reading agent snapshot")?
            .agents
    };
    let live_keys = if all && !json {
        Some(
            ledger
                .snapshot_cached()
                .context("reading live agent snapshot")?
                .agents
                .iter()
                .map(agent_key)
                .collect::<std::collections::BTreeSet<_>>(),
        )
    } else {
        None
    };
    let agents: Vec<&AgentState> = agents
        .iter()
        .filter(|agent| agent.parent_agent_id.is_none())
        .filter(|agent| {
            worktree
                .as_deref()
                .is_none_or(|filter| agent_in_worktree(agent, filter))
        })
        .collect();
    if json {
        supervised::output::print_json(&agents)?;
        return Ok(());
    }
    let now = jiff::Timestamp::now();
    let mut out = render::out();
    if let Some(live_keys) = live_keys.as_ref() {
        let mut table = render::Table::new([
            "NAME",
            "KIND",
            "STATUS",
            "LIFECYCLE",
            "MODEL",
            "CTX",
            "TOKENS",
            "TODO",
            "AGE",
            "WORKTREE",
            "PANE",
        ])
        .right(&[5, 6, 7, 8]);
        for agent in agents {
            let mut cells = agent_row(agent, now);
            cells.insert(3, lifecycle_cell(agent, live_keys));
            table.row(cells);
        }
        table.render(&mut out)?;
    } else {
        let mut table = render::Table::new([
            "NAME", "KIND", "STATUS", "MODEL", "CTX", "TOKENS", "TODO", "AGE", "WORKTREE", "PANE",
        ])
        .right(&[4, 5, 6, 7]);
        for agent in agents {
            table.row(agent_row(agent, now));
        }
        table.render(&mut out)?;
    }
    Ok(())
}

fn agent_key(agent: &AgentState) -> (AgentKind, AgentSessionId) {
    (agent.kind.clone(), agent.agent_id.clone())
}

fn lifecycle_label(
    agent: &AgentState,
    live_keys: &std::collections::BTreeSet<(AgentKind, AgentSessionId)>,
) -> &'static str {
    if live_keys.contains(&agent_key(agent)) {
        "live"
    } else {
        "stale"
    }
}

pub(super) fn show_agent(reference: String, json: bool, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let ledger = crate::cli::open_ledger(&workspace)?;
    let snapshot = ledger.snapshot_cached().context("reading agent snapshot")?;
    let agent_result = crate::cli::resolve_agent_card(&snapshot, &reference, None);
    let mut agent = agent_result.as_ref().ok().map(|agent| (*agent).clone());
    let mut stale = false;
    let mut audit_error = None;
    if agent.is_none() {
        match resolve_audit_agent(&ledger, &workspace, &reference) {
            Ok(Some(audit_agent)) => {
                agent = Some(audit_agent);
                stale = true;
            }
            Ok(None) => {}
            Err(err) => audit_error = Some(err),
        }
    }
    let run = newest_run_by_ref(&ledger, &reference, agent.as_ref())?;
    if json {
        #[derive(serde::Serialize)]
        struct Show<'a> {
            #[serde(skip_serializing_if = "Option::is_none")]
            agent: Option<&'a AgentState>,
            #[serde(skip_serializing_if = "is_false")]
            stale: bool,
            #[serde(skip_serializing_if = "Option::is_none")]
            run: Option<RunRecord>,
        }
        supervised::output::print_json(&Show {
            agent: agent.as_ref(),
            stale,
            run,
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
    kv.push(
        "name",
        render::cell(agent_name(agent)).fg(render::palette::ACCENT),
    );
    kv.push(
        "kind",
        render::cell(agent_kind_label(agent)).fg(render::palette::META),
    );
    kv.push("session", render::cell(agent.agent_id.to_string()));
    kv.push(
        "status",
        render::cell(agent_status_label(agent))
            .fg(render::status::agent(agent.status, agent.phase)),
    );
    if stale {
        kv.push(
            "lifecycle",
            render::cell("stale").fg(render::palette::FAINT),
        );
    }
    kv.push("model", render::cell(model_label(agent)).dash());
    kv.push("worktree", render::cell(worktree_label(agent)).dash());
    kv.push("pane", render::cell(pane_label(agent)).dash());
    kv.render(&mut render::out())?;
    if let Some(run) = run.or_else(|| newest_run_for_agent(&ledger, agent).ok().flatten()) {
        print_run_line(&run)?;
    }
    Ok(())
}

fn resolve_audit_agent(
    ledger: &rimz::Ledger,
    workspace: &rimz::ResolvedWorkspace,
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
    );
    match crate::cli::resolve_agent_card(&snapshot, reference, None) {
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
    let agent = crate::cli::resolve_agent_card(&snapshot, &reference, None)?;
    let pane = agent
        .pane
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("agent {} has no bound pane", agent_name(agent)))?;
    let backend = rimz::mux::backend_for(pane.pane_id.mux());
    backend.focus_pane(&pane.pane_id).map_err(Into::into)
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
    let live_agent = crate::cli::resolve_agent_card(&snapshot, &reference, None).ok();
    if let Some(run) = newest_run_by_ref(&ledger, &reference, live_agent)?
        && (!run.status.is_terminal() || live_agent.is_none() || run.run_id.as_str() == reference)
    {
        return wait_run_record(&ledger, &run, timeout, stream_output, from_start, json);
    }
    if live_agent.is_none() {
        crate::cli::resolve_agent_card(&snapshot, &reference, None)?;
    }
    let deadline = timeout.map(|duration| Instant::now() + duration);
    loop {
        let snapshot = ledger.snapshot_cached().context("reading agent snapshot")?;
        let agent = crate::cli::resolve_agent_card(&snapshot, &reference, None)?;
        if gate_open(DeliveryGate::Done, agent.status) {
            if json {
                supervised::output::print_json(agent)?;
            }
            std::process::exit(0);
        }
        if agent.status == rimz::feed::AgentStatus::Failed {
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
    let live_agent = crate::cli::resolve_agent_card(&snapshot, &reference, None).ok();
    if let Some(run) = newest_run_by_ref(&ledger, &reference, live_agent)? {
        if run.status.is_terminal()
            && run.run_id.as_str() != reference
            && let Some(agent) = live_agent.as_ref()
        {
            return close_agent_pane(&workspace, agent);
        }
        let (record, wrote) = rimz::run::cancel(ledger.paths(), &run.run_id)?;
        if wrote {
            rimz::ledger::wakeup::wake_run(ledger.runtime_paths(), &record)
                .context("waking run waiter")?;
            if let Ok(backend) =
                supervised::pane::backend_for_workspace_session(&workspace, globals)
            {
                supervised::pane::close_stopped_run_pane_after_grace(
                    backend.as_ref(),
                    &ledger,
                    &workspace.session_name,
                    &record,
                    supervised::pane::STOP_BACKSTOP_GRACE,
                );
            }
        }
        return Ok(());
    }
    let agent = match live_agent {
        Some(agent) => agent,
        None => crate::cli::resolve_agent_card(&snapshot, &reference, None)?,
    };
    close_agent_pane(&workspace, agent)
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

pub(super) fn run_print(args: AgentsArgs, globals: &GlobalFlags) -> Result<()> {
    let prompt = args
        .prompt
        .clone()
        .filter(|prompt| !prompt.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("expected a prompt for `rimz agents <spec> -p`"))?;
    if args.detach && args.json {
        bail!("--json cannot be combined with --detach");
    }
    let workspace = supervised::resolve_run_workspace(globals)?;
    let machine_config = crate::cli::machine_config()?;
    let mut layout = rimz::agents_spec::resolve_layout(
        args.spec.as_deref(),
        &machine_config.agents.aliases,
        &machine_config.agents.layouts,
    )?;
    reject_prompt_that_looks_like_spec(
        args.spec.as_deref(),
        args.prompt.as_deref(),
        &machine_config.agents.aliases,
        &machine_config.agents.layouts,
    )?;
    let mode_application = supervised_permission_mode_from_flags(args.ask, args.yolo)?;
    apply_launch_mode_and_passthrough(&mut layout, Some(mode_application), &args.passthrough);
    let agent_cells = agent_cells(&layout);
    if agent_cells.len() != 1 {
        bail!("--print requires a layout with exactly one agent cell");
    }
    if layout_cell_count(&layout) != 1 {
        bail!("--print requires a single-cell agent layout");
    }
    let (kind, agent_args, cell_mode) = agent_cells[0];
    let adapter = rimz::agents::find_adapter(kind)
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{kind}`"))?;
    let launch_env = full_agent_launch_env(&workspace.project_root, adapter, None, None)?;
    supervised::preflight_agent(adapter)?;
    supervised::preflight_program(adapter, agent_args, &prompt, &launch_env)?;

    let launch = crate::cli::agents_launch::resolve_cwd(
        &workspace,
        &machine_config.worktree,
        args.worktree.as_deref(),
    )?;
    let mux = rimz::mux::auto_detect_backend(globals.mux)?;
    let backend = rimz::mux::backend_for(mux);
    let mux_config = rimz::config::MultiplexerConfig::from(&machine_config);
    let width = rimz::mux::SidebarWidth::from_config(&machine_config.sidebar);
    let detected_size = rimz::mux::detect_terminal_size();
    let ledger = crate::cli::open_ledger(&workspace)?;
    backend.ensure_session(&rimz::mux::SessionOptions {
        session_name: workspace.session_name.clone(),
        workspace_id: workspace.workspace_id.clone(),
        project_root: workspace.project_root.clone(),
        cwd: launch.cwd.clone(),
        config: mux_config.clone(),
        detected_size,
    })?;
    let room = RoomTarget {
        workspace_id: &workspace.workspace_id,
        project_root: &workspace.project_root,
        session_name: &workspace.session_name,
        cwd: &launch.cwd,
        mux_config: &mux_config,
        width,
        detected_size,
        refresh_ms: None,
    };
    crate::cli::launch_sidebar_for_workspace(backend.as_ref(), &room, None, &[]);
    crate::cli::gate_room_before_attach(backend.as_ref(), &room, None, &[])?;
    crate::cli::ensure_presence_plugin(
        backend.as_ref(),
        &workspace.session_name,
        &workspace.workspace_id,
    );

    let permission_mode = cell_mode.unwrap_or(mode_application.mode);
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
            prompt: Some(prompt.clone()),
            state: rimz::schema::event::AgentLaunchState::Starting,
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
        launch_id: Some(&launch_identity.agent_id),
        cwd: &launch.cwd,
        prompt: &prompt,
        cleanup_worktree: args.worktree.is_some(),
        permission_args: agent_args,
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
    rimz::run::create(ledger.paths(), &record).context("recording run")?;
    let open_result = backend.open_tab(&TabOptions {
        session_name: workspace.session_name.clone(),
        title: format!("run: {}", adapter.descriptor().kind),
        cwd: launch.cwd.clone(),
        panes: LayoutPanes {
            columns: vec![vec![pane]],
        },
        focus: false,
        sidebar: crate::cli::build_sidebar_opts(&room, Vec::new())?,
    });
    if let Err(err) = open_result {
        let _ = rimz::run::fail(ledger.paths(), &run_id);
        let _ = append_launch_event(
            &ledger,
            &workspace,
            &launch_identity,
            LaunchEventParams {
                cwd: &launch.cwd,
                worktree_name: launch.worktree_name.as_deref(),
                prompt: Some(&prompt),
                state: rimz::schema::event::AgentLaunchState::Failed,
                pane_id: None,
            },
        );
        return Err(err).context("opening run tab");
    }
    if args.detach {
        #[expect(clippy::print_stdout, reason = "command result is the agent name")]
        {
            println!("{}", launch_identity.name);
        }
        return Ok(());
    }
    let Some((sock, _sock_path)) = bound else {
        bail!("blocking run did not bind its completion socket");
    };
    let expected = ExpectedRunFrame {
        workspace_id: workspace.workspace_id.clone(),
        run_id: run_id.clone(),
    };
    let record = if args.stream {
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
    if !args.keep {
        supervised::pane::close_run_pane(
            backend.as_ref(),
            &ledger,
            &workspace.session_name,
            &record,
        );
    }
    if args.json {
        supervised::output::print_json(&record)?;
    } else if !args.stream {
        supervised::output::print_run_output(&record)?;
    }
    drop(socket_guard);
    std::process::exit(record.status.exit_code());
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
        let current = rimz::run::load(ledger.paths(), &run.run_id)?;
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
    let mut records = rimz::run::list(ledger.paths())?;
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

/// The ten columns shared by `agents list` in both its default and `--all`
/// shapes, in display order; the `--all` view inserts `LIFECYCLE` at index 3.
fn agent_row(agent: &AgentState, now: jiff::Timestamp) -> Vec<render::Cell> {
    vec![
        render::cell(agent_name(agent)).fg(render::palette::ACCENT),
        render::cell(agent_kind_label(agent)).fg(render::palette::META),
        render::cell(agent_status_label(agent))
            .fg(render::status::agent(agent.status, agent.phase)),
        render::cell(model_label(agent)).dash(),
        context_cell(agent),
        render::cell(tokens_label(agent)).dash(),
        render::cell(todo_label(agent)).dash(),
        render::cell(age_label(now, agent.last_seen)),
        render::cell(worktree_label(agent)).dash(),
        render::cell(pane_label(agent)).dash(),
    ]
}

/// Context fill warms as it climbs: gold past 75%, rose past 90%.
fn context_cell(agent: &AgentState) -> render::Cell {
    let c = render::cell(context_label(agent));
    match agent.context_pct {
        Some(pct) if pct >= 90 => c.fg(render::palette::ALARM),
        Some(pct) if pct >= 75 => c.fg(render::palette::WARN),
        Some(_) => c,
        None => c.dash(),
    }
}

fn lifecycle_cell(
    agent: &AgentState,
    live_keys: &std::collections::BTreeSet<(AgentKind, AgentSessionId)>,
) -> render::Cell {
    let label = lifecycle_label(agent, live_keys);
    let cell = render::cell(label);
    if label == "stale" {
        cell.fg(render::palette::FAINT)
    } else {
        cell
    }
}

fn agent_cells(layout: &LayoutSpec) -> Vec<(&str, &[String], Option<PermissionMode>)> {
    layout
        .columns
        .iter()
        .flat_map(|column| {
            column.rows.iter().filter_map(|cell| match cell {
                Cell::Agent { kind, args, mode } => Some((kind.as_str(), args.as_slice(), *mode)),
                Cell::Command { .. } => None,
            })
        })
        .collect()
}

fn layout_cell_count(layout: &LayoutSpec) -> usize {
    layout.columns.iter().map(|column| column.rows.len()).sum()
}

fn agent_name(agent: &AgentState) -> &str {
    agent.name.as_deref().unwrap_or(agent.agent_id.as_str())
}

fn agent_kind_label(agent: &AgentState) -> String {
    match agent.kind_ordinal {
        Some(ordinal) => format!("{}-{}", agent.kind, ordinal),
        None => agent.kind.to_string(),
    }
}

fn agent_status_label(agent: &AgentState) -> String {
    if agent.phase == rimz::agents::TurnPhase::Idle {
        agent.status.as_str().to_owned()
    } else {
        format!("{}:{}", agent.status.as_str(), phase_label(agent.phase))
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

fn context_label(agent: &AgentState) -> String {
    agent
        .context_pct
        .map(|pct| format!("{pct}%"))
        .unwrap_or_else(|| "-".to_owned())
}

fn tokens_label(agent: &AgentState) -> String {
    agent
        .total_tokens
        .map(compact_count)
        .unwrap_or_else(|| "-".to_owned())
}

fn todo_label(agent: &AgentState) -> String {
    match (agent.todo_done, agent.todo_total) {
        (Some(done), Some(total)) => format!("{done}/{total}"),
        _ => "-".to_owned(),
    }
}

fn age_label(now: jiff::Timestamp, last_seen: jiff::Timestamp) -> String {
    let secs = now.duration_since(last_seen).as_secs().max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3_600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3_600)
    } else {
        format!("{}d", secs / 86_400)
    }
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

fn worktree_label(agent: &AgentState) -> String {
    agent
        .worktree_branch
        .clone()
        .or_else(|| {
            agent
                .worktree_path
                .as_deref()
                .and_then(|path| path.rsplit('/').next())
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "-".to_owned())
}

fn pane_label(agent: &AgentState) -> String {
    agent
        .pane
        .as_ref()
        .map(|pane| pane.pane_id.to_string())
        .unwrap_or_else(|| "-".to_owned())
}

fn agent_in_worktree(agent: &AgentState, filter: &str) -> bool {
    agent.worktree_branch.as_deref() == Some(filter)
        || agent
            .worktree_path
            .as_deref()
            .is_some_and(|path| path == filter || path.rsplit('/').next() == Some(filter))
}
