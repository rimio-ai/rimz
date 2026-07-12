use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Args;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use super::GlobalFlags;
use crate::cli::render;
use rimz::agents::{AgentState, AgentStatus};
use rimz::harness::target;
use rimz::ids::AgentSessionId;
use rimz::pane::PaneRef;
use rimz::proc::TreeTotals;
use rimz::tui::{MouseCapture, TerminalModeGuard};
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
        conflicts_with = "all",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::scope_names)
    )]
    worktree: Option<String>,
}

pub(super) fn run_top(args: TopArgs, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let mux = rimz::mux::auto_detect_backend(globals.mux).map_err(|_| {
        anyhow::anyhow!(crate::cli::agents_launch::live_session_guidance(
            &workspace.session_name
        ))
    })?;
    let backend = rimz::mux::backend_for(mux);
    crate::cli::agents_launch::ensure_live_session(&*backend, &workspace.session_name)?;
    let runtime = rimz::RuntimePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing runtime paths")?;
    let state = rimz::StatePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing state paths")?;
    state.ensure_dirs().context("preparing state directories")?;
    let interval = args.interval.unwrap_or(DEFAULT_INTERVAL);
    let filter = channel_filter(args.all, args.worktree.as_deref(), &workspace);
    let first = sample(&state, &runtime, &workspace, filter.as_deref())?;
    std::thread::sleep(FIRST_SAMPLE_DELAY);
    let mut current = sample(&state, &runtime, &workspace, filter.as_deref())?;
    if args.once {
        let rows = top_rows(&first, &current, FIRST_SAMPLE_DELAY);
        let mut out = render::out();
        render_top(&mut out, &current, &rows)?;
        return Ok(());
    }

    let _mode = TerminalModeGuard::enable(MouseCapture::Off)?;
    let mut previous = first;
    let mut elapsed_hint = FIRST_SAMPLE_DELAY;
    loop {
        repaint(&current, &top_rows(&previous, &current, elapsed_hint))?;
        if wait_for_quit(interval)? {
            return Ok(());
        }
        previous = current;
        current = sample(&state, &runtime, &workspace, filter.as_deref())?;
        elapsed_hint = interval;
    }
}

fn sample(
    state: &rimz::StatePaths,
    runtime: &rimz::RuntimePaths,
    workspace: &rimz::ResolvedWorkspace,
    channel: Option<&str>,
) -> Result<TopSample> {
    let snapshot = rimz::sidebar::consumer::read_published_snapshot(
        &mut rimz::sidebar::consumer::RollupCursor::new(),
        state,
        runtime,
        &workspace.session_name,
        None,
    )
    .context("reading the room snapshot")?;
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
    let agents: Vec<AgentState> = snapshot
        .agents
        .into_iter()
        .filter(|agent| agent.parent_agent_id.is_none())
        .filter(|agent| in_room.contains_key(&agent.agent_id))
        .filter(|agent| channel.is_none_or(|filter| target::agent_in_worktree(agent, filter)))
        .collect();
    let peers: Vec<&AgentState> = agents.iter().collect();
    let now = jiff::Timestamp::now();
    let mut rows = BTreeMap::new();
    for agent in &agents {
        let handle = target::agent_handle(agent, &peers, false);
        let live_pane_pid = in_room.get(&agent.agent_id).copied().flatten();
        let metrics =
            metrics_root_pid(live_pane_pid, agent.pane.as_ref()).and_then(rimz::proc::tree_totals);
        rows.insert(
            agent.agent_id.to_string(),
            AgentSample {
                handle,
                status: agent.status,
                context_pct: agent.context_fill_pct(),
                tokens: agent.total_tokens.unwrap_or(0),
                age: render::age_short(agent.last_seen, now),
                metrics,
            },
        );
    }
    Ok(TopSample {
        sampled_at: Instant::now(),
        rows,
    })
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
    handle: String,
    status: AgentStatus,
    context_pct: Option<f64>,
    tokens: u64,
    age: String,
    metrics: Option<TreeTotals>,
}

#[derive(Clone, Debug, PartialEq)]
struct TopRow {
    handle: String,
    status: String,
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
                handle: current.handle.clone(),
                status: status_label(current.status),
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
    rows.sort_by(|left, right| {
        metric_rank(right)
            .cmp(&metric_rank(left))
            .then_with(|| right.tokens.cmp(&left.tokens))
            .then_with(|| left.handle.cmp(&right.handle))
    });
    rows
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
    let running = rows.iter().filter(|row| row.status == "running").count();
    let total_cpu: f64 = rows.iter().filter_map(|row| row.cpu_pct).sum();
    let total_mem: u64 = rows.iter().filter_map(|row| row.mem_bytes).sum();
    let total_tokens: u64 = rows.iter().map(|row| row.tokens).sum();
    writeln!(
        w,
        "{} agents · {running} running · {} CPU · {} MEM · {} tokens",
        sample.rows.len(),
        fmt_cpu(Some(total_cpu)),
        render::fmt_bytes(total_mem),
        render::compact_count(total_tokens),
    )?;
    let mut table = render::Table::new([
        "AGENT", "STATUS", "CPU", "MEM", "IO/S", "PROCS", "CTX", "TOKENS", "AGE",
    ])
    .right(&[2, 3, 4, 5, 6, 7]);
    for row in rows {
        table.row([
            render::cell(row.handle.as_str()).fg(render::palette::ACCENT),
            render::cell(row.status.as_str()),
            render::cell(fmt_cpu(row.cpu_pct)).dash(),
            render::cell(
                row.mem_bytes
                    .map(render::fmt_bytes)
                    .unwrap_or_else(|| "-".to_owned()),
            )
            .dash(),
            render::cell(
                row.io_bps
                    .map(|bytes| format!("{}/s", render::fmt_bytes(bytes)))
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
        ]);
    }
    table.render(w)
}

fn repaint(sample: &TopSample, rows: &[TopRow]) -> Result<()> {
    use ratatui::crossterm::{
        cursor::MoveTo,
        execute,
        terminal::{Clear, ClearType},
    };

    let mut stdout = std::io::stdout();
    execute!(stdout, MoveTo(0, 0), Clear(ClearType::All))?;
    let mut frame = Vec::new();
    render_top(&mut frame, sample, rows)?;
    rimz::tui::write_crlf(&mut stdout, &frame)?;
    execute!(stdout, Clear(ClearType::FromCursorDown))?;
    stdout.flush()?;
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
    match (worktree, all) {
        (Some(worktree), _) => Some(worktree.to_owned()),
        (None, true) => None,
        (None, false) => crate::cli::current_channel(workspace),
    }
}

fn status_label(status: AgentStatus) -> String {
    status.as_str().to_owned()
}

fn fmt_cpu(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.1}%"))
        .unwrap_or_else(|| "-".to_owned())
}

fn parse_interval(raw: &str) -> std::result::Result<Duration, String> {
    if let Some(ms) = raw.strip_suffix("ms") {
        let ms: u64 = ms
            .parse()
            .map_err(|_| format!("invalid interval `{raw}`"))?;
        return Ok(Duration::from_millis(ms));
    }
    crate::cli::parse::parse_duration_units(raw, &[("s", 1), ("m", 60), ("", 1)])
}

#[cfg(test)]
mod tests {
    use super::*;

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
            handle: handle.to_owned(),
            status: AgentStatus::Running,
            context_pct: Some(25.0),
            tokens,
            age: "1s".to_owned(),
            metrics,
        }
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
