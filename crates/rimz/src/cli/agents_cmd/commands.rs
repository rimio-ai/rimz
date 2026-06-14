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
    let runtime = rimz::RuntimePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing runtime paths")?;
    let context_records = rimz::ledger::agent_context::read_all(&runtime);

    let (mut snapshot, live_keys) = if all {
        let audit = ledger
            .runtime_projection(rimz::RuntimeScope::Audit)
            .context("reading audit agent rollup")?
            .agents;
        let mut snapshot = rimz::SidebarSnapshot::build_with_agents(
            workspace.workspace_id.clone(),
            Vec::new(),
            audit,
            jiff::Timestamp::now(),
        );
        // The audit projection carries no workspace identity. Copy the live
        // snapshot's root and class so an out-of-project stale agent tails into
        // `external` instead of earning its own pod above project work — the
        // same identity the sidebar groups by. Only the grouped human view needs
        // it; `--json` emits a flat array, so it skips the extra read.
        let live_keys = if json {
            None
        } else {
            let live = ledger
                .snapshot_cached()
                .context("reading live agent snapshot")?;
            snapshot = snapshot
                .with_root_class(live.root_class)
                .with_project_root(live.project_root.clone());
            Some(
                live.agents
                    .iter()
                    .map(agent_key)
                    .collect::<std::collections::BTreeSet<_>>(),
            )
        };
        (snapshot, live_keys)
    } else {
        (
            ledger.snapshot_cached().context("reading agent snapshot")?,
            None,
        )
    };
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

    let agents: Vec<&AgentState> = snapshot
        .agents
        .iter()
        .filter(|agent| agent.parent_agent_id.is_none())
        .filter(|agent| {
            worktree
                .as_deref()
                .is_none_or(|filter| rimz::target::agent_in_worktree(agent, filter))
        })
        .collect();
    if json {
        supervised::output::print_json(&agents)?;
        return Ok(());
    }

    let groups = rimz::ledger::snapshot::group_live_agents_by_worktree(&agents, &snapshot);
    let now = jiff::Timestamp::now();
    let mut out = render::out();
    let mut table = if live_keys.is_some() {
        render::Table::new([
            "AGENT",
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
        .right(&[4, 5, 6, 7])
    } else {
        render::Table::new([
            "AGENT", "STATUS", "MODEL", "CTX", "TOKENS", "TODO", "AGE", "WORKTREE", "PANE",
        ])
        .right(&[3, 4, 5, 6])
    };
    for group in &groups {
        table.section(format!("{} ({})", group.label, group.agents.len()));
        for &agent in &group.agents {
            let mut cells = agent_row(agent, &group.agents, now);
            if let Some(live_keys) = live_keys.as_ref() {
                cells.insert(2, lifecycle_cell(agent, live_keys));
            }
            table.row(cells);
        }
    }
    table.render(&mut out)?;
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
    let runtime = rimz::RuntimePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing runtime paths")?;
    // Fold the rich statusline context so the shown card — and the `--json`
    // payload — carries the real token window, not the carried-forward
    // `context_pct`.
    let snapshot = ledger
        .snapshot_cached()
        .context("reading agent snapshot")?
        .with_agent_context(rimz::ledger::agent_context::read_all(&runtime));
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
    let peers: Vec<&AgentState> = snapshot
        .agents
        .iter()
        .filter(|candidate| candidate.parent_agent_id.is_none())
        .collect();
    kv.push(
        "agent",
        render::cell(rimz::target::agent_handle(agent, &peers, true)).fg(render::palette::ACCENT),
    );
    kv.push(
        "kind",
        render::cell(agent.kind.to_string()).fg(render::palette::META),
    );
    if let Some(name) = agent.name.as_deref() {
        kv.push("name", render::cell(name));
    }
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
    kv.push("context", context_cell(agent));
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
    let live_agent = crate::cli::resolve_agent_one(
        &snapshot,
        &reference,
        None,
        crate::cli::current_channel(&workspace).as_deref(),
    )
    .ok();
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
        None => crate::cli::resolve_agent_one(
            &snapshot,
            &reference,
            None,
            crate::cli::current_channel(&workspace).as_deref(),
        )?,
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
    apply_launch_mode_and_passthrough(
        &mut layout,
        Some(mode_application),
        &launch_override_preset(&args)?,
        &args.passthrough,
    )?;
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
    let record = if output_format == OutputFormat::StreamJson {
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
    match output_format {
        OutputFormat::Text => supervised::output::print_run_output(&record)?,
        OutputFormat::Json => supervised::output::print_json(&record)?,
        // stream-json already emitted its events as the run progressed.
        OutputFormat::StreamJson => {}
    }
    drop(socket_guard);
    std::process::exit(record.status.exit_code());
}

/// Resolve the supervised prompt from the positional argument or, for
/// `--input-format stream-json`, from stream-json user messages on stdin.
fn resolve_print_prompt(args: &AgentsArgs, input_format: InputFormat) -> Result<String> {
    match input_format {
        InputFormat::Text => args
            .prompt
            .clone()
            .filter(|prompt| !prompt.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("expected a prompt for `rimz agents <spec> -p`")),
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

/// The columns shared by `agents list` in both its default and `--all` shapes,
/// in display order; the `--all` view inserts `LIFECYCLE` at index 2. The lead
/// `AGENT` cell is the canonical `@kind` handle, disambiguated within the group
/// (whose worktree heads the section, so the row omits the `#channel`).
fn agent_row(agent: &AgentState, peers: &[&AgentState], now: jiff::Timestamp) -> Vec<render::Cell> {
    vec![
        render::cell(rimz::target::agent_handle(agent, peers, false)).fg(render::palette::ACCENT),
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
    let pct = context_pct_display(agent);
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

/// The real context-window fill (0..=100): the live token composition over the
/// model's window — "from sidebar or model". Prefers the precise used/window
/// fraction, then the statusline's reported `used_percentage`, then the carried
/// `context_pct`, so a session with no rich context still reads its last gauge.
fn context_pct_display(agent: &AgentState) -> Option<f64> {
    match (context_used_tokens(agent), resolved_context_window(agent)) {
        (Some(used), Some(window)) if window > 0 => {
            Some((used as f64 / window as f64 * 100.0).clamp(0.0, 100.0))
        }
        _ => agent
            .context
            .as_ref()
            .and_then(|context| context.tokens.as_ref())
            .and_then(|tokens| tokens.used_percentage)
            .or(agent.context_pct)
            .map(f64::from),
    }
}

/// Tokens currently occupying the window: the statusline's rich breakdown, else
/// the per-call split (`cache_read + fresh_input`) the rollout tail feeds.
fn context_used_tokens(agent: &AgentState) -> Option<u64> {
    agent
        .context
        .as_ref()
        .and_then(|context| context.tokens.as_ref())
        .and_then(rimz::agents::AgentTokenUsage::used_tokens)
        .or_else(|| {
            let fresh = agent.fresh_input_tokens?;
            Some(agent.cache_read_input_tokens.unwrap_or(0) + fresh)
        })
}

/// The window denominator: the statusline's `context_window_size`, else the
/// adapter-resolved `context_window`, else the model descriptor's default.
fn resolved_context_window(agent: &AgentState) -> Option<u64> {
    agent
        .context
        .as_ref()
        .and_then(|context| context.tokens.as_ref())
        .and_then(|tokens| tokens.context_window_size)
        .or(agent.context_window)
        .or_else(|| {
            rimz::agents::descriptor_by_kind(agent.kind.as_str())
                .and_then(|descriptor| descriptor.default_context_window)
        })
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

/// The agent's channel for the `WORKTREE` column, dashed when it runs outside
/// any worktree. The channel itself comes from [`rimz::target::agent_channel`],
/// the single source of truth; this only chooses the table's `-` placeholder
/// over the resolver's prose label.
fn worktree_label(agent: &AgentState) -> String {
    rimz::target::agent_channel(agent).unwrap_or_else(|| "-".to_owned())
}

fn pane_label(agent: &AgentState) -> String {
    agent
        .pane
        .as_ref()
        .map(|pane| pane.pane_id.to_string())
        .unwrap_or_else(|| "-".to_owned())
}
