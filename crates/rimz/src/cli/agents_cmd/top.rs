use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Args;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use super::GlobalFlags;
use super::report::{child_for_agent, row_for_agent};
use crate::cli::render;
use rimz::address;
use rimz::agents::AgentStatus;
use rimz::ids::AgentSessionId;
use rimz::pane::PaneRef;
use rimz::proc::TreeTotals;
use rimz::store::snapshot::card_status;
use rimz::tui::{MouseCapture, Screen, TerminalModeGuard};
use rimz::utils::time::{DurationUnit, parse_duration_units};
use rimz::workspace::WorkspaceResolver;

const DEFAULT_INTERVAL: Duration = Duration::from_secs(2);
const FIRST_SAMPLE_DELAY: Duration = Duration::from_millis(500);
const KEY_POLL: Duration = Duration::from_millis(100);

#[derive(Debug, Args)]
pub(super) struct TopArgs {
    /// Refresh interval, e.g. `2s`, `500ms`, or a bare number of seconds.
    #[arg(long, value_parser = parse_interval)]
    interval: Option<Duration>,
    /// Print one table and exit.
    #[arg(long)]
    once: bool,
    /// Include every channel in this room.
    #[arg(long)]
    all: bool,
    /// Filter to a worktree/channel.
    #[arg(
        long,
        short = 'w',
        conflicts_with = "all",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::scope_names)
    )]
    worktree: Option<String>,
}

pub(super) fn run_top(args: TopArgs, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let store = crate::cli::require_existing_store(&workspace)?;
    let _mux = rimz::room::require_live_mux(globals.mux, &workspace)?;
    let runtime = rimz::RuntimePaths::for_project_root(&workspace.project_root)
        .context("preparing runtime paths")?;
    let state = rimz::StatePaths::for_project_root(&workspace.project_root)
        .context("preparing state paths")?;
    let mut reader = rimz::sidebar::consumer::PublishedSnapshotReader::new(
        runtime.clone(),
        workspace.session_name.clone(),
        None,
    );
    let interval = args.interval.unwrap_or(DEFAULT_INTERVAL);
    let filter = channel_filter(args.all, args.worktree.as_deref(), &workspace);
    let address_reader = crate::cli::current_channel(&workspace, Some(&store)).address_context();
    let first = sample(
        &state,
        &runtime,
        &store,
        &mut reader,
        filter.as_deref(),
        &address_reader,
    )?;
    refuse_empty_scope(&first, args.worktree.as_deref(), &store, &workspace)?;
    std::thread::sleep(FIRST_SAMPLE_DELAY);
    let mut current = sample(
        &state,
        &runtime,
        &store,
        &mut reader,
        filter.as_deref(),
        &address_reader,
    )?;
    if args.once {
        let rows = top_rows(&first, &current, FIRST_SAMPLE_DELAY);
        let mut out = render::out();
        render_top(&mut out, &current, &rows)?;
        return Ok(());
    }

    let _mode = TerminalModeGuard::enable(MouseCapture::Off, Screen::Main)?;
    let mut previous = first;
    let mut elapsed_hint = FIRST_SAMPLE_DELAY;
    loop {
        repaint(&current, &top_rows(&previous, &current, elapsed_hint))?;
        if wait_for_quit(interval)? {
            return Ok(());
        }
        previous = current;
        current = sample(
            &state,
            &runtime,
            &store,
            &mut reader,
            filter.as_deref(),
            &address_reader,
        )?;
        elapsed_hint = interval;
    }
}

fn sample(
    state: &rimz::StatePaths,
    runtime: &rimz::RuntimePaths,
    store: &rimz::Store,
    reader: &mut rimz::sidebar::consumer::PublishedSnapshotReader,
    channel: Option<&str>,
    address_reader: &address::AddressContext,
) -> Result<TopSample> {
    let snapshot = reader.read(state).context("reading the room snapshot")?;
    let demotion = rimz::harness::park_demotion(
        Some(store),
        state,
        runtime,
        &crate::cli::machine_config(),
        &snapshot.agents,
        snapshot.resume_outcomes.as_deref().unwrap_or_default(),
        jiff::Timestamp::now(),
    );
    Ok(sample_snapshot(
        &snapshot,
        channel,
        address_reader,
        &demotion,
    ))
}

fn sample_snapshot(
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    channel: Option<&str>,
    reader: &address::AddressContext,
    demotion: &rimz::agents::ParkDemotion,
) -> TopSample {
    let in_room: HashMap<AgentSessionId, Option<u32>> = snapshot
        .agent_panes
        .iter()
        .filter_map(|pane_agent| {
            pane_agent
                .agent_id
                .clone()
                .map(|id| (id, pane_agent.pane_pid))
        })
        .collect();
    let agents = snapshot
        .agents
        .iter()
        .filter(|agent| !agent.is_provider_subagent())
        .filter(|agent| in_room.contains_key(&agent.agent_id))
        .filter(|agent| channel.is_none_or(|filter| address::agent_in_worktree(agent, filter)));
    let now = jiff::Timestamp::now();
    let mut rows = BTreeMap::new();
    for agent in agents {
        let handle = address::paste_address(agent, snapshot, reader);
        let live_pane_pid = in_room.get(&agent.agent_id).copied().flatten();
        let metrics =
            metrics_root_pid(live_pane_pid, agent.pane.as_ref()).and_then(rimz::proc::tree_totals);
        rows.insert(
            agent.agent_id.to_string(),
            AgentSample {
                kind: agent.kind.to_string(),
                handle,
                status: card_status(
                    agent,
                    row_for_agent(snapshot, agent),
                    child_for_agent(snapshot, agent),
                    demotion,
                )
                .0,
                context_pct: agent.context_fill_pct(),
                tokens: agent.usage.total_tokens.unwrap_or(0),
                age: render::age_short(agent.last_seen, now),
                metrics,
                parent_id: agent.parent_agent_id.as_ref().map(ToString::to_string),
            },
        );
    }
    TopSample {
        sampled_at: Instant::now(),
        rows,
    }
}

fn metrics_root_pid(live_pane_pid: Option<u32>, stamped_pane: Option<&PaneRef>) -> Option<u32> {
    live_pane_pid.or_else(|| stamped_pane.and_then(|pane| pane.pane_pid))
}

#[derive(Clone, Debug)]
struct TopSample {
    sampled_at: Instant,
    rows: BTreeMap<String, AgentSample>,
}

#[derive(Clone, Debug)]
struct AgentSample {
    parent_id: Option<String>,
    kind: String,
    handle: String,
    status: AgentStatus,
    context_pct: Option<f64>,
    tokens: u64,
    age: String,
    metrics: Option<TreeTotals>,
}

#[derive(Clone, Debug, PartialEq)]
struct TopRow {
    id: String,
    parent_id: Option<String>,
    kind: String,
    handle: String,
    status: AgentStatus,
    cpu_pct: Option<f64>,
    mem_bytes: Option<u64>,
    io_bps: Option<u64>,
    process_count: Option<u32>,
    context_pct: Option<f64>,
    tokens: u64,
    age: String,
}

fn top_rows(previous: &TopSample, current: &TopSample, elapsed_hint: Duration) -> Vec<TopRow> {
    let elapsed = current
        .sampled_at
        .checked_duration_since(previous.sampled_at)
        .unwrap_or(elapsed_hint)
        .max(Duration::from_millis(1));
    let elapsed_secs = elapsed.as_secs_f64();
    let clk_tck = rimz::proc::clk_tck() as f64;
    let mut rows: Vec<TopRow> = current
        .rows
        .iter()
        .map(|(id, current)| {
            let previous_metrics = previous.rows.get(id).and_then(|row| row.metrics);
            let cpu_pct = match (previous_metrics, current.metrics) {
                (Some(previous), Some(current)) => {
                    let delta = current.cpu_ticks.saturating_sub(previous.cpu_ticks);
                    Some(delta as f64 / (clk_tck * elapsed_secs) * 100.0)
                }
                _ => None,
            };
            let io_bps = match (
                previous_metrics.and_then(|metrics| metrics.io_bytes),
                current.metrics.and_then(|metrics| metrics.io_bytes),
            ) {
                (Some(previous), Some(current)) => {
                    Some(((current.saturating_sub(previous)) as f64 / elapsed_secs) as u64)
                }
                _ => None,
            };
            TopRow {
                id: id.clone(),
                parent_id: current.parent_id.clone(),
                kind: current.kind.clone(),
                handle: current.handle.clone(),
                status: current.status,
                cpu_pct,
                mem_bytes: current
                    .metrics
                    .map(|metrics| metrics.rss_kb.saturating_mul(1024)),
                io_bps,
                process_count: current.metrics.map(|metrics| metrics.process_count),
                context_pct: current.context_pct,
                tokens: current.tokens,
                age: current.age.clone(),
            }
        })
        .collect();
    let ranks = rows
        .iter()
        .map(|row| (row.id.clone(), tree_rank(row, &rows)))
        .collect::<BTreeMap<_, _>>();
    rows.sort_by(|left, right| {
        ranks[&right.id]
            .cmp(&ranks[&left.id])
            .then_with(|| right.tokens.cmp(&left.tokens))
            .then_with(|| left.handle.cmp(&right.handle))
    });
    let mut ordered = Vec::with_capacity(rows.len());
    for row in rows.iter().filter(|row| {
        !rows
            .iter()
            .any(|parent| row.parent_id.as_deref() == Some(&parent.id))
    }) {
        push_tree(row, &rows, 0, &mut ordered);
    }
    ordered
}

fn tree_rank(row: &TopRow, rows: &[TopRow]) -> (bool, u64, u64) {
    let mut rank = metric_rank(row);
    for child in rows
        .iter()
        .filter(|child| child.parent_id.as_deref() == Some(&row.id))
    {
        let child_rank = tree_rank(child, rows);
        rank.0 |= child_rank.0;
        rank.1 = rank.1.saturating_add(child_rank.1);
    }
    rank
}

fn push_tree(row: &TopRow, rows: &[TopRow], depth: usize, ordered: &mut Vec<TopRow>) {
    let mut displayed = row.clone();
    if depth > 0 {
        displayed.handle = format!("{}└ {}", "  ".repeat(depth), row.handle);
    }
    ordered.push(displayed);
    for child in rows
        .iter()
        .filter(|child| child.parent_id.as_deref() == Some(&row.id))
    {
        push_tree(child, rows, depth + 1, ordered);
    }
}

fn metric_rank(row: &TopRow) -> (bool, u64, u64) {
    (
        row.cpu_pct.is_some(),
        row.cpu_pct.map(cpu_milli).unwrap_or(0),
        row.mem_bytes.unwrap_or(0),
    )
}

fn cpu_milli(value: f64) -> u64 {
    (value.max(0.0) * 1_000.0).round() as u64
}

fn render_top(w: &mut impl Write, sample: &TopSample, rows: &[TopRow]) -> std::io::Result<()> {
    let total_cpu = rows
        .iter()
        .filter_map(|row| row.cpu_pct)
        .reduce(|total, cpu| total + cpu);
    let total_mem: u64 = rows.iter().filter_map(|row| row.mem_bytes).sum();
    let total_tokens: u64 = rows.iter().map(|row| row.tokens).sum();
    writeln!(
        w,
        "{} agent{} · {} CPU · {} MEM · {} spent",
        sample.rows.len(),
        if sample.rows.len() == 1 { "" } else { "s" },
        fmt_cpu(total_cpu),
        render::fmt_bytes(total_mem),
        render::compact_count(total_tokens),
    )?;
    let show_io = rows.iter().any(|row| row.io_bps.is_some());
    let mut headers = vec![
        "AGENT", "STATUS", "CPU", "MEM", "PROCS", "CTX", "SPENT", "SEEN",
    ];
    if show_io {
        headers.insert(4, "IO/S");
    }
    let right = (2..headers.len() - 1).collect::<Vec<_>>();
    let mut table = render::Table::new(headers).right(&right);
    for row in rows {
        let mut cells = vec![
            render::cell(row.handle.as_str()).fg(render::palette::identity(&row.kind)),
            render::cell(row.status.as_str()).fg(render::status::agent(
                row.status,
                rimz::agents::TurnPhase::Idle,
            )),
            render::cell(fmt_cpu(row.cpu_pct)).dash(),
            render::cell(
                row.mem_bytes
                    .map(render::fmt_bytes)
                    .unwrap_or_else(|| "-".to_owned()),
            )
            .dash(),
            render::cell(
                row.process_count
                    .map(|count| count.to_string())
                    .unwrap_or_else(|| "-".to_owned()),
            )
            .dash(),
            render::cell(
                row.context_pct
                    .map(|pct| format!("{}%", pct.round() as u8))
                    .unwrap_or_else(|| "-".to_owned()),
            )
            .dash(),
            render::cell(render::compact_count(row.tokens)),
            render::cell(row.age.as_str()),
        ];
        if show_io {
            cells.insert(
                4,
                render::cell(
                    row.io_bps
                        .map(|bytes| format!("{}/s", render::fmt_bytes(bytes)))
                        .unwrap_or_else(|| "-".to_owned()),
                )
                .dash(),
            );
        }
        table.row(cells);
    }
    table.render(w)
}

fn repaint(sample: &TopSample, rows: &[TopRow]) -> Result<()> {
    use ratatui::crossterm::{queue, terminal::Clear, terminal::ClearType};

    let mut stdout = std::io::stdout();
    queue!(stdout, Clear(ClearType::All))?;
    let mut frame = Vec::new();
    render_top(&mut frame, sample, rows)?;
    rimz::tui::replace_frame(&mut stdout, &frame)?;
    Ok(())
}

fn wait_for_quit(duration: Duration) -> Result<bool> {
    let deadline = Instant::now() + duration;
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Ok(false);
        }
        let timeout = (deadline - now).min(KEY_POLL);
        if !event::poll(timeout)? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if key.code == KeyCode::Char('q')
            || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            return Ok(true);
        }
    }
}

fn channel_filter(
    all: bool,
    worktree: Option<&str>,
    workspace: &rimz::ResolvedWorkspace,
) -> Option<String> {
    super::list::list_channel_filter(all, worktree, workspace)
}

fn refuse_empty_scope(
    sample: &TopSample,
    scope: Option<&str>,
    store: &rimz::Store,
    workspace: &rimz::ResolvedWorkspace,
) -> Result<()> {
    if sample.rows.is_empty()
        && let Some(scope) = scope
    {
        super::list::refuse_unknown_lane(store, workspace, scope)?;
    }
    Ok(())
}

fn fmt_cpu(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.1}%"))
        .unwrap_or_else(|| "-".to_owned())
}

fn parse_interval(raw: &str) -> std::result::Result<Duration, String> {
    parse_duration_units(
        raw,
        &[
            DurationUnit::Millisecond,
            DurationUnit::Second,
            DurationUnit::Minute,
            DurationUnit::UnitlessSecond,
        ],
    )
    .map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stalled_child_snapshot() -> rimz::store::snapshot::SidebarSnapshot {
        let now = jiff::Timestamp::from_second(3600).unwrap();
        let mut parent = rimz::testkit::agent_state("claude", "parent", now);
        parent.pane = Some(PaneRef::from_id(rimz::PaneId::from_parts(
            rimz::MuxName::Tmux,
            "%1",
        )));
        let mut child = rimz::testkit::agent_state(
            "claude",
            "child",
            now - jiff::SignedDuration::from_secs(1920),
        );
        child.parent_agent_id = Some(parent.agent_id.clone());
        child.parent_agent_kind = Some(parent.kind.clone());
        child.launch_depth = Some(1);
        child.status = AgentStatus::Running;
        child.phase = rimz::agents::TurnPhase::Reasoning;
        child.pane = Some(PaneRef::from_id(rimz::PaneId::from_parts(
            rimz::MuxName::Tmux,
            "%2",
        )));
        let panes = vec![parent.pane.clone().unwrap(), child.pane.clone().unwrap()];
        rimz::store::snapshot::SidebarSnapshot::build_with_agents(
            rimz::WorkspaceId::from_project_root(std::path::Path::new("/repo")),
            vec![parent, child],
            now,
        )
        .with_live_panes(panes, None)
    }

    #[test]
    fn card_projection_stalled_child_top_uses_parent_projection() {
        let snapshot = stalled_child_snapshot();
        let sample = sample_snapshot(
            &snapshot,
            None,
            &crate::cli::CurrentChannel::Unscoped.address_context(),
            &Default::default(),
        );
        assert_eq!(sample.rows["child"].status, AgentStatus::Failed);
    }

    #[test]
    fn card_projection_stalled_child_show_uses_parent_projection() {
        let snapshot = stalled_child_snapshot();
        let child = snapshot
            .agents
            .iter()
            .find(|agent| agent.agent_id.as_str() == "child")
            .unwrap();
        assert!(row_for_agent(&snapshot, child).is_none());
        let report = super::super::report::build_list_report(
            &Default::default(),
            &snapshot,
            &[child],
            snapshot.now,
            None,
            &[],
            &Default::default(),
            &crate::cli::CurrentChannel::Unscoped.address_context(),
        );
        let entry = &report.agents[0];
        let mut out = anstream::StripStream::new(Vec::new());
        super::super::show::render_activity_section(
            &mut out,
            entry,
            None,
            None,
            false,
            snapshot.now,
        )
        .unwrap();
        let text = String::from_utf8(out.into_inner()).unwrap();
        assert!(text.starts_with("failed ·"), "{text}");
        assert!(text.contains("  failed: silent 32m"), "{text}");
        assert_eq!(entry.phase, rimz::agents::TurnPhase::Idle);
    }

    #[test]
    fn top_addresses_use_unfiltered_snapshot() {
        let now = jiff::Timestamp::UNIX_EPOCH;
        let mut snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
            rimz::WorkspaceId::from_project_root(std::path::Path::new("/repo")),
            ["a", "b"]
                .into_iter()
                .map(|lane| {
                    let mut agent = rimz::testkit::agent_state("claude", lane, now);
                    agent.role = Some("coder".to_owned());
                    agent.channel = Some(lane.to_owned());
                    agent.status = AgentStatus::Running;
                    let mut context = rimz::agents::AgentContext::new("claude", now);
                    context.turn_error = Some(rimz::agents::AgentTurnError {
                        class: rimz::agents::TurnErrorClass::PausedOverloaded,
                        at: now + jiff::SignedDuration::from_secs(1),
                        label: Some("overloaded".into()),
                    });
                    agent.context = Some(context);
                    agent
                })
                .collect(),
            now,
        );
        snapshot.agent_panes = snapshot
            .agents
            .iter()
            .map(|agent| {
                serde_json::from_value(serde_json::json!({
                    "kind": "claude",
                    "agent_id": agent.agent_id,
                    "pane_id": format!("tmux:%{}", agent.agent_id),
                    "channel": agent.channel,
                }))
                .unwrap()
            })
            .collect::<Vec<_>>()
            .into();
        let reader = crate::cli::CurrentChannel::Unscoped.address_context();
        let sample = sample_snapshot(&snapshot, Some("a"), &reader, &Default::default());
        assert_eq!(
            sample.rows.get("a").map(|row| row.handle.as_str()),
            Some("@coder#a")
        );
        let rows = top_rows(&sample, &sample, Duration::from_secs(1));
        assert_eq!(rows[0].handle, "@coder#a");
        assert_eq!(sample.rows["a"].status, AgentStatus::Paused);
    }

    #[test]
    fn interval_accepts_documented_units() {
        assert_eq!(parse_interval("5").unwrap(), Duration::from_secs(5));
        assert_eq!(parse_interval("500ms").unwrap(), Duration::from_millis(500));
    }

    fn totals(cpu_ticks: u64, rss_kb: u64, io_bytes: Option<u64>) -> TreeTotals {
        TreeTotals {
            cpu_ticks,
            rss_kb,
            io_bytes,
            process_count: 2,
        }
    }

    fn row(handle: &str, tokens: u64, metrics: Option<TreeTotals>) -> AgentSample {
        AgentSample {
            parent_id: None,
            kind: "claude".to_owned(),
            handle: handle.to_owned(),
            status: AgentStatus::Running,
            context_pct: Some(25.0),
            tokens,
            age: "1s".to_owned(),
            metrics,
        }
    }

    #[test]
    fn card_projection_top_without_metrics_omits_io_and_unknown_cpu() {
        let sample = TopSample {
            sampled_at: Instant::now(),
            rows: BTreeMap::from([("a".into(), row("@a", 3_000_000, None))]),
        };
        let rows = top_rows(&sample, &sample, Duration::from_secs(1));
        let mut out = anstream::StripStream::new(Vec::new());
        render_top(&mut out, &sample, &rows).unwrap();
        let text = String::from_utf8(out.into_inner()).unwrap();
        assert_eq!(
            text.lines().next(),
            Some("1 agent · - CPU · 0 B MEM · 3M spent")
        );
        assert!(!text.contains("IO/S"), "{text}");
        assert!(text.lines().nth(1).unwrap().contains("SPENT"), "{text}");
        assert!(!text.contains("TOKENS"), "{text}");
        assert!(text.lines().nth(1).unwrap().ends_with("SEEN"), "{text}");
    }

    #[test]
    fn card_projection_top_nests_children_and_ranks_tree_cpu() {
        let now = Instant::now();
        let mut child = row("@child", 1, Some(totals(0, 1, None)));
        child.parent_id = Some("parent".into());
        let previous = TopSample {
            sampled_at: now,
            rows: BTreeMap::from([
                ("parent".into(), row("@parent", 1, Some(totals(0, 1, None)))),
                ("child".into(), child),
                ("rival".into(), row("@rival", 1, Some(totals(0, 1, None)))),
                ("orphan".into(), row("@orphan", 0, None)),
            ]),
        };
        let mut current = previous.clone();
        current.sampled_at += Duration::from_secs(1);
        current
            .rows
            .get_mut("parent")
            .unwrap()
            .metrics
            .as_mut()
            .unwrap()
            .cpu_ticks = 10;
        current
            .rows
            .get_mut("child")
            .unwrap()
            .metrics
            .as_mut()
            .unwrap()
            .cpu_ticks = 90;
        current
            .rows
            .get_mut("rival")
            .unwrap()
            .metrics
            .as_mut()
            .unwrap()
            .cpu_ticks = 95;
        current.rows.get_mut("orphan").unwrap().parent_id = Some("missing".into());
        let rows = top_rows(&previous, &current, Duration::from_secs(1));
        assert_eq!(
            rows.iter()
                .map(|row| row.handle.as_str())
                .collect::<Vec<_>>(),
            ["@parent", "  └ @child", "@rival", "@orphan"]
        );
    }

    #[test]
    fn card_projection_top_strips_hash_and_refuses_unknown_scope() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let workspace = rimz::ResolvedWorkspace {
            workspace_id: rimz::WorkspaceId::from_project_root(&root),
            project_root: root.clone(),
            cwd_project_root: None,
            root_class: rimz::workspace::RootClass::Repo,
            worktree_root: root.clone(),
            worktree_branch: None,
            session_name: "test".into(),
            mux_hint: None,
        };
        let paths = rimz::StatePaths::under(workspace.workspace_id.clone(), &root).unwrap();
        let runtime = rimz::RuntimePaths::under(workspace.workspace_id.clone(), &root).unwrap();
        let store = rimz::Store::open(paths, runtime).unwrap();
        assert_eq!(
            channel_filter(false, Some("#auth"), &workspace).as_deref(),
            Some("auth")
        );
        let sample = TopSample {
            sampled_at: Instant::now(),
            rows: BTreeMap::new(),
        };
        let expected = super::super::list::refuse_unknown_lane(&store, &workspace, "#nonsense")
            .unwrap_err()
            .to_string();
        assert_eq!(
            refuse_empty_scope(&sample, Some("#nonsense"), &store, &workspace)
                .unwrap_err()
                .to_string(),
            expected
        );
    }

    #[test]
    fn top_rows_compute_rates_and_sort_metricless_last() {
        let now = Instant::now();
        let previous = TopSample {
            sampled_at: now,
            rows: BTreeMap::from([
                ("a".to_owned(), row("a", 5, Some(totals(100, 1, Some(100))))),
                ("b".to_owned(), row("b", 9, Some(totals(100, 5, Some(100))))),
                ("c".to_owned(), row("c", 999, None)),
            ]),
        };
        let current = TopSample {
            sampled_at: now + Duration::from_secs(1),
            rows: BTreeMap::from([
                (
                    "a".to_owned(),
                    row("a", 5, Some(totals(150, 1, Some(1_100)))),
                ),
                ("b".to_owned(), row("b", 9, Some(totals(110, 5, Some(200))))),
                ("c".to_owned(), row("c", 999, None)),
            ]),
        };

        let rows = top_rows(&previous, &current, Duration::from_secs(1));

        assert_eq!(
            rows.iter()
                .map(|row| row.handle.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
        assert!(rows[0].cpu_pct.unwrap() > rows[1].cpu_pct.unwrap());
        assert_eq!(rows[0].io_bps, Some(1_000));
        assert_eq!(rows[2].cpu_pct, None);
    }

    #[test]
    fn metrics_root_pid_prefers_live_frame_over_stamped_pane() {
        let mut stamped = PaneRef::from_id(rimz::PaneId::from_parts(rimz::MuxName::Tmux, "%1"));
        stamped.pane_pid = Some(11);

        assert_eq!(metrics_root_pid(Some(22), Some(&stamped)), Some(22));
        assert_eq!(metrics_root_pid(None, Some(&stamped)), Some(11));
    }
}
