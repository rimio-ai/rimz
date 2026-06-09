//! `rimz sidebar` — `snapshot` renders the view-model (producer or `--no-produce` consumer read); `serve` runs the terminal renderer loop.
//!
//! The snapshot arm is a thin delegate over the library produce pipeline
//! ([`rimz::sidebar::produce`]): it resolves workspace/session/mux, calls
//! `produce_snapshot` (or the in-process consumer read for `--no-produce`),
//! and emits — the CLI owns argv, fallback intent, and stdout alone. The
//! elder renderer produces in process on its fetch worker, so this arm serves
//! inspection, scripting, and the plugin rail's `--no-produce` read.

use std::io::{self, Read};
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use clap::{Args, Subcommand, ValueEnum};

use super::GlobalFlags;
use rimz::ids::{MuxName, SidebarInstanceId, WorkspaceId};
use rimz::ledger::paths::env_path;
use rimz::ledger::workspace_record;
use rimz::schema::sidebar_event::SidebarEvent;
use rimz::sidebar::consumer::read_published_snapshot;
use rimz::sidebar::produce::{
    ProduceOptions, pane_fixture_active, produce_rollup_snapshot, produce_snapshot,
};
use rimz::sidebar::{cache::write_presence_stamp, consumer::RollupCursor};
use rimz::workspace::WorkspaceResolver;
use rimz::{RuntimePaths, StatePaths};

#[derive(Debug, Args)]
pub struct SidebarArgs {
    #[command(subcommand)]
    command: SidebarSubcmd,
}

#[derive(Debug, Subcommand)]
enum SidebarSubcmd {
    /// Render the current snapshot. The sidebar process reads this.
    Snapshot {
        #[arg(long)]
        workspace_id: Option<String>,
        #[arg(long)]
        mux: Option<MuxName>,
        #[arg(long)]
        session_name: Option<String>,
        #[arg(long)]
        exclude_pane_id: Option<String>,
        /// Require a pane cache produced at or after this Unix millisecond.
        #[arg(long, hide = true)]
        min_pane_cache_ms: Option<u64>,
        #[arg(long)]
        json: bool,
        /// Render read-only from the producer's published cache: never fork
        /// `list-panes` or git. A non-producer renderer (one whose workspace
        /// already has an elder producer) passes this so the per-tab fleet
        /// pays the mux/git round-trip exactly once, on the elder.
        #[arg(long)]
        no_produce: bool,
    },
    /// Run the terminal sidebar renderer.
    Serve {
        #[arg(long)]
        workspace_id: Option<String>,
        #[arg(long)]
        mux: Option<MuxName>,
        #[arg(long)]
        session_name: Option<String>,
        #[arg(long, default_value_t = 1)]
        tick_seconds: u64,
        #[arg(long)]
        refresh_ms: Option<u16>,
    },
    /// Read a snapshot JSON from stdin and render one fixed frame.
    Render {
        #[arg(long, default_value_t = 80)]
        width: u16,
        #[arg(long, default_value_t = 24)]
        height: u16,
    },
    /// Render a deterministic sidebar fixture frame. Hidden — contributor
    /// screenshot infrastructure, not a user-facing sidebar verb.
    #[command(hide = true)]
    Fixture {
        #[arg(value_enum)]
        state: SidebarFixtureState,
        #[arg(long, default_value_t = 54)]
        width: u16,
        #[arg(long, default_value_t = 34)]
        height: u16,
    },
    /// Presence poke from the Zellij presence plugin: refresh the liveness
    /// stamp and wake the sidebar fleet through either an exact-cache shortcut
    /// or a producer refetch. Hidden — plugin infrastructure, not a human verb.
    #[command(hide = true)]
    Wake {
        #[arg(long)]
        workspace_id: Option<String>,
        #[arg(long, value_enum)]
        reason: WakeReason,
        #[arg(long)]
        session_name: Option<String>,
        #[arg(long)]
        pane_id: Option<String>,
        #[arg(long = "command-arg")]
        command_args: Vec<String>,
        #[arg(long = "focused-pane-id")]
        focused_pane_ids: Vec<String>,
        #[arg(long = "unfocused-pane-id")]
        unfocused_pane_ids: Vec<String>,
        #[arg(long = "topology", hide = true)]
        topology: Option<String>,
    },
}

/// Why a presence poke fired. Every reason refreshes the liveness stamp;
/// `alive` is the plugin's keepalive — stamp-only — so an idle-but-healthy
/// channel stays distinguishable from a dead one.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum WakeReason {
    PanesChanged,
    PaneOpened,
    PaneClosed,
    FocusStranded,
    CommandChanged,
    FocusChanged,
    Alive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum SidebarFixtureState {
    Empty,
    Fleet,
    Provider,
}

pub fn run(args: SidebarArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        SidebarSubcmd::Snapshot {
            workspace_id,
            mux,
            session_name,
            exclude_pane_id,
            min_pane_cache_ms,
            json,
            no_produce,
        } => {
            // A producer reads `list-panes`/git and publishes the shared cache;
            // a non-producer renders read-only from that cache. Default is to
            // produce, so bare CLI calls and the plugin rail are unchanged.
            let produce = !no_produce;
            let mut resolved_session = None;
            let workspace_id = match workspace_id {
                Some(raw) => raw.parse::<WorkspaceId>()?,
                None => {
                    let workspace =
                        WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
                    resolved_session = Some(workspace.session_name.clone());
                    workspace.workspace_id
                }
            };
            let state =
                StatePaths::for_workspace(workspace_id.clone()).context("preparing state paths")?;
            let runtime = RuntimePaths::for_workspace(workspace_id.clone())
                .context("preparing runtime paths")?;
            state.ensure_dirs().context("preparing state paths")?;
            runtime.ensure_dirs().context("preparing runtime paths")?;
            let session_name = session_name
                .or(resolved_session)
                .or_else(|| session_name_from_record(&state));
            let exclude = exclude_pane_id
                .as_deref()
                .map(rimz::ids::PaneId::parse)
                .transpose()?;

            let emit = |snapshot: &rimz::SidebarSnapshot| -> Result<()> {
                if json {
                    let rendered = serde_json::to_string_pretty(snapshot)?;
                    #[expect(clippy::print_stdout, reason = "json emitter for sidebar")]
                    {
                        println!("{rendered}");
                    }
                } else {
                    let tally = |status| {
                        snapshot
                            .worktree_groups
                            .iter()
                            .flat_map(|group| &group.status_counts)
                            .filter(|count| count.status == status)
                            .map(|count| count.count)
                            .sum::<usize>()
                    };
                    let waiting = tally(rimz::feed::AgentStatus::Waiting);
                    let failed = tally(rimz::feed::AgentStatus::Failed);
                    #[expect(clippy::print_stdout, reason = "human summary")]
                    {
                        println!("Workspace:       {}", snapshot.display_name);
                        println!("Worktree groups: {}", snapshot.worktree_groups.len());
                        println!("Waiting:         {waiting}");
                        println!("Failed:          {failed}");
                    }
                }
                Ok(())
            };

            // Consumer: render the producer's published frame in process. A
            // cold cache (no publish yet) returns the bare rollup with the
            // same read-only enrichments until the next tick. One-shot CLI
            // process, so a fresh cursor (a cold fold) is the only kind.
            // A pane fixture defers to the produce path, which short-circuits
            // on it — deterministic tests neither poison nor read the cache.
            if !produce
                && !pane_fixture_active()
                && let Some(session) = session_name.as_deref()
            {
                let snapshot = read_published_snapshot(
                    &mut RollupCursor::new(),
                    &state,
                    &runtime,
                    session,
                    exclude.as_ref(),
                )
                .context("reading the consumer snapshot")?;
                return emit(&snapshot);
            }

            // Producer (or a deterministic test fixture, or a bare inspection
            // call): the library pipeline resolves the base — ledger rollup
            // plus live pane list, single-flighted across the fleet — folds
            // the producer enrichments, and publishes the caches consumers
            // read. With no session or no detectable mux there is no pane
            // frame to produce; the frameless arm runs the same metadata
            // enrichments over the bare rollup and emits no groups.
            let mux = mux
                .or(globals.mux)
                .or_else(|| rimz::mux::auto_detect_backend(None).ok());
            let rollup_only = |reason: Option<&dyn std::fmt::Display>| -> Result<()> {
                if let Some(error) = reason {
                    tracing::warn!(%error, "sidebar snapshot pane discovery failed; emitting frameless rollup metadata");
                }
                emit(&produce_rollup_snapshot(
                    &mut RollupCursor::new(),
                    &state,
                    &runtime,
                    exclude.as_ref(),
                    min_pane_cache_ms,
                )?)
            };
            match (session_name, mux) {
                (Some(session_name), Some(mux)) => {
                    let opts = ProduceOptions {
                        mux,
                        session_name,
                        exclude: exclude.clone(),
                        min_pane_cache_ms,
                    };
                    match produce_snapshot(&mut RollupCursor::new(), &state, &runtime, &opts) {
                        Ok(snapshot) => emit(&snapshot),
                        // An inspection call has no live frame to hold (the
                        // serve loop produces in process and owns its own
                        // degraded path); fall back to the ledger rollup.
                        Err(err) => rollup_only(Some(&err)),
                    }
                }
                _ => rollup_only(None),
            }
        }
        SidebarSubcmd::Serve {
            workspace_id,
            mux,
            session_name,
            tick_seconds,
            refresh_ms,
        } => {
            let needs_workspace_resolve = workspace_id.is_none() || session_name.is_none();
            let resolved = if needs_workspace_resolve {
                Some(WorkspaceResolver::resolve_participant(
                    ".",
                    globals.root.clone(),
                )?)
            } else {
                None
            };
            let workspace_id = match workspace_id {
                Some(raw) => raw.parse::<WorkspaceId>()?,
                None => resolved
                    .as_ref()
                    .ok_or_else(|| anyhow!("workspace_id missing but workspace was not resolved"))?
                    .workspace_id
                    .clone(),
            };
            let session_name = match session_name {
                Some(name) => name,
                None => resolved
                    .as_ref()
                    .ok_or_else(|| anyhow!("session_name missing but workspace was not resolved"))?
                    .session_name
                    .clone(),
            };
            let mux = match mux {
                Some(mux) => mux,
                None => rimz::mux::auto_detect_backend(globals.mux)?,
            };
            rimz::sidebar_pane::app::serve(rimz::sidebar_pane::app::ServeConfig {
                workspace_id,
                mux,
                session_name,
                instance_id: SidebarInstanceId::new(),
                tick_seconds,
                refresh_ms_override: refresh_ms,
                notification_prefs: rimz::config::MachineConfig::load()
                    .unwrap_or_default()
                    .notifications,
                own_pane: rimz::mux::own_pane_id(mux),
            })
            .context("serving sidebar")
        }
        SidebarSubcmd::Render { width, height } => {
            let mut buf = String::new();
            io::stdin()
                .read_to_string(&mut buf)
                .context("reading stdin")?;
            let snapshot = serde_json::from_str(&buf).context("parsing snapshot from stdin")?;
            rimz::sidebar_pane::render::render_fixed(io::stdout(), &snapshot, None, width, height)
                .context("rendering snapshot")
        }
        SidebarSubcmd::Fixture {
            state,
            width,
            height,
        } => {
            let snapshot = sidebar_fixture_snapshot(state)?;
            rimz::sidebar_pane::render::render_fixed_line_ansi(
                io::stdout(),
                &snapshot,
                None,
                width,
                height,
            )
            .context("rendering sidebar fixture")
        }
        SidebarSubcmd::Wake {
            workspace_id,
            reason,
            session_name,
            pane_id,
            command_args,
            focused_pane_ids,
            unfocused_pane_ids,
            topology,
        } => {
            // Feather-weight by design: the poke needs only the workspace
            // runtime dir — one stamp write plus at most one datagram — so it
            // never opens the ledger, never lists panes, never touches the
            // mux. The plugin calls this per presence event.
            let workspace_id = match workspace_id {
                Some(raw) => raw.parse::<WorkspaceId>()?,
                None => {
                    WorkspaceResolver::resolve_participant(".", globals.root.clone())?.workspace_id
                }
            };
            let runtime =
                RuntimePaths::for_workspace(workspace_id).context("preparing runtime paths")?;
            // Every reason refreshes the stamp that flips the producer's pane
            // TTL to event mode; the write is best-effort cache-class — a miss
            // only means the channel reads as dead one poke longer.
            write_presence_stamp(&runtime);
            write_topology_cache(&runtime, topology.as_deref());
            let Some(event) = wake_event(
                reason,
                pane_id.as_deref(),
                &command_args,
                &focused_pane_ids,
                &unfocused_pane_ids,
            ) else {
                return Ok(());
            };
            if let Err(err) = rimz::ledger::wakeup::broadcast_sidebar_event(
                &runtime,
                session_name.as_deref(),
                event,
            ) {
                tracing::debug!(error = %err, "presence poke: event datagram failed");
            }
            Ok(())
        }
    }
}

fn sidebar_fixture_snapshot(state: SidebarFixtureState) -> Result<rimz::SidebarSnapshot> {
    let now = fixture_now()?;
    let workspace_id = "ws_0123456789abcdef01234567".parse::<WorkspaceId>()?;
    let mut snapshot = rimz::SidebarSnapshot {
        workspace_id,
        display_name: "query-engine".to_owned(),
        generated_at: now,
        panes_produced_at_ms: Some(1_781_009_600_000),
        now,
        worktree_groups: Vec::new(),
        needs_attention: Vec::new(),
        resolver_working: Vec::new(),
        agents: Vec::new(),
        wired_lazy_kinds: vec!["codex".to_owned()],
        lazy_agent_default_models: std::collections::BTreeMap::new(),
        own_view: None,
        only_daemon_view_remains: false,
        project_root: Some(PathBuf::from("/srv/code/query-engine")),
        worktree_roots: vec![PathBuf::from("/srv/code/query-engine")],
        root_class: rimz::workspace::RootClass::Repo,
        sidebar: rimz::config::SidebarConfig::default(),
        providers: Vec::new(),
        value_tally: None,
        today_spend_live_usd: None,
        reflects_log: None,
    };

    match state {
        SidebarFixtureState::Empty => {}
        SidebarFixtureState::Fleet => add_fleet_fixture(&mut snapshot, now),
        SidebarFixtureState::Provider => {
            add_fleet_fixture(&mut snapshot, now);
            add_provider_fixture(&mut snapshot, now);
        }
    }
    Ok(snapshot)
}

fn fixture_now() -> Result<jiff::Timestamp> {
    "2026-06-09T12:00:00Z"
        .parse()
        .context("parsing sidebar fixture timestamp")
}

fn add_fleet_fixture(snapshot: &mut rimz::SidebarSnapshot, now: jiff::Timestamp) {
    let claude = agent_row(
        "agent:claude:auth",
        "claude",
        "terminal_21",
        "/srv/code/query-engine",
        "feature/auth-router",
        rimz::feed::AgentStatus::Running,
        rimz::agents::TurnPhase::Acting,
        "port auth watcher",
        "Opus 4.1",
        Some((67, 200_000, 128_400)),
        now,
    );
    let codex = agent_row(
        "agent:codex:pricing",
        "codex",
        "terminal_22",
        "/srv/code/query-engine/.rimz/worktrees/pricing",
        "pricing-refresh",
        rimz::feed::AgentStatus::Waiting,
        rimz::agents::TurnPhase::Idle,
        "approve pricing cache write",
        "GPT-5.1-Codex",
        Some((41, 272_000, 96_200)),
        now,
    );
    let pi = agent_row(
        "agent:pi:mux",
        "pi",
        "terminal_23",
        "/srv/code/query-engine/.rimz/worktrees/mux",
        "zellij-health",
        rimz::feed::AgentStatus::Failed,
        rimz::agents::TurnPhase::Idle,
        "debug zellij health probe",
        "Pi",
        None,
        now,
    );
    let process = rimz::SidebarRow {
        id: "process:cargo-nextest".to_owned(),
        name: "cargo nextest".to_owned(),
        pane: Some(pane_ref(
            "terminal_24",
            "cargo nextest run",
            "/srv/code/query-engine",
            false,
        )),
        worktree_path: Some("/srv/code/query-engine".to_owned()),
        worktree_branch: Some("main".to_owned()),
        last_activity: now,
        card: rimz::RowCard::Process(rimz::ProcessCard {
            state: rimz::ProcessState::Busy,
            command_detail: Some("integration::backend::zellij".to_owned()),
            cpu_pct: Some(37),
            rss_kb: Some(412_000),
            ..rimz::ProcessCard::default()
        }),
    };

    snapshot.worktree_groups = vec![
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine".to_owned(),
            label: "main".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: vec![
                status_count(rimz::feed::AgentStatus::Running, 1),
                status_count(rimz::feed::AgentStatus::Waiting, 1),
            ],
            rows: vec![claude, codex, process],
            hidden_count: 0,
            diff_added: Some(182),
            diff_removed: Some(47),
            commits_ahead: Some(3),
            commits_behind: Some(1),
            trunk: Some("main".to_owned()),
            clean: Some(false),
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/mux".to_owned(),
            label: "zellij-health".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: vec![status_count(rimz::feed::AgentStatus::Failed, 1)],
            rows: vec![pi],
            hidden_count: 0,
            diff_added: Some(14),
            diff_removed: Some(3),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            trunk: Some("main".to_owned()),
            clean: Some(false),
        },
    ];
    snapshot.value_tally = Some(spend_tally(9.42, 712_000, 8));
    snapshot.today_spend_live_usd = Some(10.08);
}

#[allow(clippy::too_many_arguments)]
fn agent_row(
    id: &str,
    name: &str,
    pane_raw: &str,
    cwd: &str,
    branch: &str,
    status: rimz::feed::AgentStatus,
    phase: rimz::agents::TurnPhase,
    task: &str,
    model: &str,
    context: Option<(u8, u64, u64)>,
    now: jiff::Timestamp,
) -> rimz::SidebarRow {
    let (context_pct, context_window, total_tokens) = context
        .map_or((None, None, None), |(pct, window, total)| {
            (Some(pct), Some(window), Some(total))
        });
    let mut card = rimz::AgentCard {
        status: Some(status),
        phase,
        surface: Some(rimz::Surface::NativeUi),
        task: Some(task.to_owned()),
        model: Some(model.to_owned()),
        context_pct,
        context_window,
        total_tokens,
        cache_read_input_tokens: Some(total_tokens.unwrap_or_default() / 3),
        fresh_input_tokens: Some(total_tokens.unwrap_or_default() / 5),
        output_tokens: Some(total_tokens.unwrap_or_default() / 8),
        todo_done: Some(4),
        todo_total: Some(7),
        context_severity: context_pct.map(|pct| {
            rimz::feed::ContextSeverity::classify(
                pct,
                total_tokens,
                &rimz::config::ContextSeverityConfig::default(),
            )
        }),
        registered_at: Some(now),
        ..rimz::AgentCard::default()
    };
    if status == rimz::feed::AgentStatus::Failed {
        card.turn_error_label = Some("API error".to_owned());
        card.todo_done = Some(2);
        card.todo_total = Some(5);
    }
    if status == rimz::feed::AgentStatus::Running {
        card.sub_agents = vec![
            rimz::SidebarSubAgent {
                id: "child:review".to_owned(),
                name: "review".to_owned(),
                status: rimz::feed::AgentStatus::Running,
                phase: rimz::agents::TurnPhase::Reasoning,
                task: Some("check unsafe edges".to_owned()),
                model: Some("Haiku".to_owned()),
                effort: None,
                description: Some("audit auth watcher changes".to_owned()),
                total_tokens: Some(22_400),
                elapsed_secs: Some(180),
                started_at: Some(now),
                last_activity: now,
            },
            rimz::SidebarSubAgent {
                id: "child:test".to_owned(),
                name: "test".to_owned(),
                status: rimz::feed::AgentStatus::Success,
                phase: rimz::agents::TurnPhase::Idle,
                task: Some("run focused nextest".to_owned()),
                model: Some("Haiku".to_owned()),
                effort: None,
                description: None,
                total_tokens: Some(18_900),
                elapsed_secs: Some(260),
                started_at: Some(now),
                last_activity: now,
            },
        ];
    }

    rimz::SidebarRow {
        id: id.to_owned(),
        name: name.to_owned(),
        pane: Some(pane_ref(pane_raw, name, cwd, status.is_attention())),
        worktree_path: Some(cwd.to_owned()),
        worktree_branch: Some(branch.to_owned()),
        last_activity: now,
        card: rimz::RowCard::Agent(Box::new(card)),
    }
}

fn pane_ref(raw: &str, command: &str, cwd: &str, focused: bool) -> rimz::feed::PaneRef {
    rimz::feed::PaneRef {
        pane_id: rimz::PaneId::from_parts(rimz::MuxName::Zellij, raw),
        session_name: "rimz-fixture".to_owned(),
        view_id: Some("tab_0".to_owned()),
        view_kind: Some(rimz::ViewKind::Tab),
        view_name: Some("main".to_owned()),
        is_focused: focused,
        command: Some(command.to_owned()),
        spawn_command: None,
        cwd: Some(cwd.to_owned()),
        pane_pid: None,
        pane_process_start: None,
        resumed_session_id: None,
        elevated_agent: None,
    }
}

fn status_count(status: rimz::feed::AgentStatus, count: usize) -> rimz::SidebarStatusCount {
    rimz::SidebarStatusCount { status, count }
}

fn add_provider_fixture(snapshot: &mut rimz::SidebarSnapshot, now: jiff::Timestamp) {
    snapshot.sidebar.provider_tabs = rimz::config::ProviderTabsMode::Always;
    snapshot.providers = vec![
        provider_panel(
            "claude",
            "Claude",
            173,
            Some("2.1.158"),
            Some("Claude Max"),
            true,
            true,
            Some((25, 40)),
            spend_tally(6.84, 498_000, 4),
            now,
        ),
        provider_panel(
            "codex",
            "Codex",
            33,
            Some("0.135.0"),
            Some("ChatGPT Pro"),
            false,
            false,
            None,
            spend_tally(3.24, 214_000, 5),
            now,
        ),
    ];
}

#[allow(clippy::too_many_arguments)]
fn provider_panel(
    kind: &str,
    product_name: &str,
    color: u8,
    version: Option<&str>,
    plan: Option<&str>,
    metered: bool,
    remote_control: bool,
    windows: Option<(u8, u8)>,
    spending: rimz::SpendTally,
    now: jiff::Timestamp,
) -> rimz::SidebarProviderPanel {
    let window = |used: u8, mins: u32, resets_in_secs: u64| rimz::agents::RateLimitWindow {
        used_percentage: Some(used),
        resets_at: Some(now + std::time::Duration::from_secs(resets_in_secs)),
        duration_mins: Some(mins),
    };
    let windows = windows
        .map(|(short, long)| {
            vec![
                window(short, 300, 2 * 60 * 60),
                window(long, 7 * 24 * 60, 2 * 24 * 60 * 60),
            ]
        })
        .unwrap_or_default();
    rimz::SidebarProviderPanel {
        kind: kind.to_owned(),
        product_name: product_name.to_owned(),
        art: vec![
            " ▐▛███▜▌".to_owned(),
            "▝▜█████▛▘".to_owned(),
            "  ▘▘ ▝▝".to_owned(),
        ],
        color,
        version: version.map(ToOwned::to_owned),
        plan: plan.map(ToOwned::to_owned),
        metered,
        remote_control,
        spending: Some(spending),
        windows,
    }
}

fn spend_tally(usd: f64, tokens: u64, sessions: u32) -> rimz::SpendTally {
    let window = |scale: f64| {
        let tokens = (tokens as f64 * scale) as u64;
        rimz::SpendWindow {
            usd: usd * scale,
            tokens,
            input: tokens * 7 / 10,
            output: tokens * 2 / 10,
            cache_write: tokens / 20,
            cache_read: tokens / 10,
            sessions,
        }
    };
    rimz::SpendTally {
        today: window(1.0),
        week: window(2.8),
        month: window(7.4),
        year: window(19.0),
    }
}

fn write_topology_cache(runtime: &RuntimePaths, topology: Option<&str>) {
    let Some(topology) = topology else {
        return;
    };
    match serde_json::from_str::<rimz::schema::pane_topology::PaneTopologyCache>(topology) {
        Ok(mut cache) => {
            sanitize_topology_cache(&mut cache);
            if let Err(err) = rimz::sidebar::cache::write_pane_topology_cache(runtime, &cache) {
                tracing::debug!(error = %err, "presence poke: topology cache write failed");
            }
        }
        Err(err) => {
            tracing::debug!(error = %err, "presence poke: topology payload parse failed");
        }
    }
}

fn sanitize_topology_cache(cache: &mut rimz::schema::pane_topology::PaneTopologyCache) {
    for pane in &mut cache.panes {
        if pane
            .pane_command
            .as_deref()
            .is_some_and(command_is_launch_chrome)
        {
            pane.pane_command = None;
        }
    }
}

/// Map a poke reason onto its typed event. `None` means the poke carries no
/// event of its own (`alive` is stamp-only). Producer-verifying pane reasons
/// missing their pane data degrade to the identity-free `PanesChanged` nudge,
/// so a sparse poke still triggers the producer's verifying pull.
fn wake_event(
    reason: WakeReason,
    pane_id: Option<&str>,
    command_args: &[String],
    focused_pane_ids: &[String],
    unfocused_pane_ids: &[String],
) -> Option<SidebarEvent> {
    let zellij_pane = |raw: &str| rimz::ids::PaneId::from_parts(rimz::ids::MuxName::Zellij, raw);
    match reason {
        WakeReason::Alive => None,
        WakeReason::PanesChanged => Some(SidebarEvent::PanesChanged),
        WakeReason::PaneOpened => Some(match pane_id {
            Some(pane_id) => SidebarEvent::PaneOpened {
                pane_id: zellij_pane(pane_id),
                command: command_from_args(command_args),
            },
            None => SidebarEvent::PanesChanged,
        }),
        WakeReason::PaneClosed => Some(match pane_id {
            Some(pane_id) => SidebarEvent::PaneClosed {
                pane_id: zellij_pane(pane_id),
            },
            None => SidebarEvent::PanesChanged,
        }),
        WakeReason::FocusStranded => pane_id.map(|pane_id| SidebarEvent::FocusStranded {
            pane_id: zellij_pane(pane_id),
        }),
        WakeReason::CommandChanged => Some(match pane_id.zip(command_from_args(command_args)) {
            Some((_pane_id, command)) if command_is_launch_chrome(&command) => {
                SidebarEvent::PanesChanged
            }
            Some((pane_id, command)) => SidebarEvent::CommandChanged {
                pane_id: zellij_pane(pane_id),
                command,
            },
            None => SidebarEvent::PanesChanged,
        }),
        WakeReason::FocusChanged => Some(SidebarEvent::FocusChanged {
            focused: zellij_pane_ids(focused_pane_ids),
            unfocused: zellij_pane_ids(unfocused_pane_ids),
        }),
    }
}

fn zellij_pane_ids(raws: &[String]) -> Vec<rimz::ids::PaneId> {
    raws.iter()
        .filter(|raw| !raw.is_empty())
        .map(|raw| rimz::ids::PaneId::from_parts(rimz::ids::MuxName::Zellij, raw))
        .collect()
}

fn command_from_args(args: &[String]) -> Option<String> {
    let command = args
        .iter()
        .filter(|arg| !arg.is_empty())
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    (!command.is_empty()).then_some(command)
}

fn command_is_launch_chrome(command: &str) -> bool {
    let mut tokens = command.split_whitespace().filter(|token| !token.is_empty());
    let Some(program) = tokens.next() else {
        return false;
    };
    program_basename(program) == "rimz" && tokens.next() == Some("tab")
}

fn program_basename(program: &str) -> &str {
    std::path::Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(program)
}

fn session_name_from_record(state: &StatePaths) -> Option<String> {
    workspace_record::read(&state.workspace_record)
        .ok()
        .map(|record| record.session_name)
}

fn bin_name(stem: &str) -> String {
    format!("{stem}{}", std::env::consts::EXE_SUFFIX)
}

pub(crate) fn rimz_cli_program() -> PathBuf {
    env_path("RIMZ_BIN")
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| PathBuf::from(bin_name("rimz")))
}

#[cfg(test)]
mod tests;
