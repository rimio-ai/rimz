//! `rimz doctor` — workspace health report: trust state, protocol versions,
//! socket-path budget, hook wiring, agent problems, and message-delivery
//! failures.
//!
//! The report is collected concurrently into a [`model::DoctorReport`], then either
//! rendered as the human report ([`render`]) or serialized as JSON — to stdout
//! or atomically to a file. Collection lives in the sibling modules; presentation
//! lives in [`render`]; this file only assembles and emits.
//!
//! Each source a section reads fails on its own: a source added to an existing
//! section carries its own error, so its failure renders as one note inside the
//! section and never replaces data the section already showed. A broken config
//! is exactly when someone runs doctor.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread::{Scope, ScopedJoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Args;

use super::{GlobalFlags, spinner::Spinner};
use crate::cli::render as ui;
use rimz::workspace::WorkspaceResolver;

mod agents;
mod messages;
mod model;
mod mux_log;
mod protocol;
mod render;
mod runtime;
mod watermark;

#[cfg(test)]
mod tests;

use model::DoctorReport;

#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Widen every section to every row it has (every agent session, loop task, language server,
    /// and message failure), and measure disk use for STORAGE and TEMP UNITS by walking the RimZ
    /// home. STORAGE adds per-folder room entries and warns above 150,000 in unswept folders.
    /// The bug-report flag, as in rimz doctor --audit --no-log-text --output <file>.
    /// The audit can take minutes; an agent runs it under rimz wait --run.
    #[arg(long)]
    audit: bool,
    /// Emit machine-readable JSON instead of the human report.
    #[arg(long)]
    json: bool,
    /// Omit multiplexer log record text from the report (summaries, samples, and excerpts).
    #[arg(long)]
    no_log_text: bool,
    /// Write the report to a file (atomically) instead of stdout.
    #[arg(long, value_name = "PATH")]
    output: Option<PathBuf>,
    /// Dismiss recorded diagnostics, the last incident, and message failures so
    /// reports only show records from after this moment.
    #[arg(long)]
    clear: bool,
}

pub fn run(args: DoctorArgs, globals: &GlobalFlags) -> Result<()> {
    if args.clear {
        let workspace = WorkspaceResolver::resolve(".", globals.root.clone())
            .context("resolving workspace to clear doctor history")?;
        let paths = rimz::StatePaths::for_project_root(&workspace.project_root)
            .context("resolving workspace state to clear doctor history")?;
        watermark::stamp(&paths, jiff::Timestamp::now())?;
    }
    let log_text = if args.no_log_text {
        mux_log::LogText::Omit
    } else {
        mux_log::LogText::Include
    };
    let progress = Progress {
        spinner: Spinner::delayed(progress_label(&[]), Duration::from_millis(150)),
        in_flight: Mutex::new(Vec::new()),
    };
    let report = collect_report(globals, args.audit, log_text, &progress);
    drop(progress);
    emit(&report, args.json, args.output.as_deref())
}

fn collect_report(
    globals: &GlobalFlags,
    audit: bool,
    log_text: mux_log::LogText,
    progress: &Progress,
) -> DoctorReport {
    let workspace = WorkspaceResolver::resolve(".", globals.root.clone());
    let ws = workspace.as_ref().ok();
    let history_cleared_at = ws
        .and_then(|ws| rimz::StatePaths::for_project_root(&ws.project_root).ok())
        .as_ref()
        .and_then(watermark::read);
    std::thread::scope(|scope| {
        let mux = progress.spawn(scope, "multiplexer", || {
            runtime::collect_mux(globals.mux, ws, history_cleared_at, log_text)
        });
        let terminal = progress.spawn(scope, "terminal", runtime::collect_terminal);
        let machine_config = progress.spawn(scope, "machine config", collect_machine_config);
        let packs = progress.spawn(scope, "packs", collect_packs);
        let sandbox = progress.spawn(scope, "sandbox", runtime::collect_sandbox);
        let hibernate = progress.spawn(scope, "hibernate", || {
            rimz::harness::hibernate::inspect(&crate::cli::machine_config().agents.hibernate)
        });
        let hooks = progress.spawn(scope, "hooks", agents::collect_hooks);
        let accounts = progress.spawn(scope, "accounts and agents", || {
            let mut accounts = agents::collect_accounts(ws);
            let agents = ws.map(|ws| agents::collect_agent_rollup(ws, audit, &mut accounts));
            (accounts, agents)
        });
        let plugins = progress.spawn(scope, "agent plugins", agents::collect_plugins);
        let loop_tasks = progress.spawn(scope, "loop tasks", collect_loop);
        let lsp = progress.spawn(scope, "lsp", || collect_lsp(ws, globals));
        let remote_control = progress.spawn(scope, "remote control", || {
            runtime::collect_remote_control(ws.map(|ws| ws.project_root.as_path()))
        });
        let disk_usage = progress.spawn(scope, "storage", || {
            audit.then(|| runtime::collect_storage(ws))
        });
        let temp_units = progress.spawn(scope, "temp units", || runtime::collect_temp_units(audit));
        let protocols = progress.spawn(scope, "protocols", || ws.map(protocol::collect_protocols));
        let trust = progress.spawn(scope, "trust", || ws.map(agents::collect_trust));
        let folder_trust = progress.spawn(scope, "folder trust", || {
            ws.map(|ws| match crate::cli::folder_trust::collect(ws) {
                Ok(rows) => model::Probe::Ready(model::FolderTrust {
                    rows: rows
                        .iter()
                        .map(crate::cli::folder_trust::Row::from)
                        .collect(),
                }),
                Err(error) => model::Probe::Unavailable {
                    diagnosis: None,
                    error: error.to_string(),
                },
            })
        });
        let empty_dir = progress.spawn(scope, "empty dir", || agents::collect_empty_dir(ws));
        let messages = progress.spawn(scope, "messages", || {
            ws.map(|ws| messages::collect_messages(ws, history_cleared_at, audit))
        });
        let diagnostics = progress.spawn(scope, "diagnostics", || {
            ws.map(|ws| runtime::collect_diagnostics(ws, history_cleared_at))
        });
        let last_incident = progress.spawn(scope, "last incident", || {
            ws.and_then(|ws| runtime::collect_last_incident(ws, history_cleared_at))
        });
        let (accounts, agents) = collected(accounts);
        let hooks = collected(hooks);
        let prompt_cache_fallback =
            agents::collect_prompt_cache_fallback(&hooks, &crate::cli::machine_config().harness);
        DoctorReport {
            schema: "rimz.doctor.v1",
            audit,
            version: rimz::build_id::VERSION,
            host: runtime::collect_host(),
            workspace: match &workspace {
                Ok(ws) => model::Probe::Ready(workspace_view(ws)),
                Err(err) => model::Probe::Unavailable {
                    diagnosis: None,
                    error: err.to_string(),
                },
            },
            mux: collected(mux),
            terminal: collected(terminal),
            home: collect_home(),
            machine_config: collected(machine_config),
            packs: collected(packs),
            sandbox: collected(sandbox),
            hibernate: collected(hibernate),
            hooks,
            prompt_cache_fallback,
            accounts,
            plugins: collected(plugins),
            loop_tasks: collected(loop_tasks),
            lsp: collected(lsp),
            remote_control: collected(remote_control),
            disk_usage: collected(disk_usage),
            temp_units: collected(temp_units),
            protocols: collected(protocols),
            trust: collected(trust),
            folder_trust: collected(folder_trust),
            empty_dir: collected(empty_dir),
            agents,
            history_cleared_at,
            messages: collected(messages),
            diagnostics: collected(diagnostics),
            last_incident: collected(last_incident),
        }
    })
}

fn collected<T>(check: ScopedJoinHandle<'_, T>) -> T {
    check
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

struct Progress {
    spinner: Spinner,
    in_flight: Mutex<Vec<&'static str>>,
}

impl Progress {
    fn spawn<'scope, 'env, T: Send + 'scope>(
        &'scope self,
        scope: &'scope Scope<'scope, 'env>,
        label: &'static str,
        collect: impl FnOnce() -> T + Send + 'scope,
    ) -> ScopedJoinHandle<'scope, T> {
        scope.spawn(move || {
            let started = Instant::now();
            self.update(|checks| checks.push(label));
            let report = collect();
            self.update(|checks| checks.retain(|check| *check != label));
            tracing::debug!(
                check = label,
                elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
                "doctor check completed"
            );
            report
        })
    }

    fn update(&self, update: impl FnOnce(&mut Vec<&'static str>)) {
        let mut checks = self
            .in_flight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        update(&mut checks);
        self.spinner.set(progress_label(&checks));
    }
}

fn progress_label(in_flight: &[&str]) -> String {
    match in_flight.split_first() {
        None => "checking doctor".into(),
        Some((oldest, [])) => format!("checking {oldest}"),
        Some((oldest, others)) => format!("checking {oldest} (+{} more)", others.len()),
    }
}

/// Startup checks for the current checkout; failures stay apart from the machine-wide table.
fn collect_lsp_checks(
    ws: &rimz::ResolvedWorkspace,
    globals: &GlobalFlags,
    entries: &[rimz::lsp::registry::Entry],
) -> Result<Vec<rimz::lsp::broker::probe::Check>> {
    let cwd = match &globals.root {
        Some(root) => root.clone(),
        None => std::env::current_dir()?,
    };
    let cwd = std::fs::canonicalize(cwd)?;
    let root = rimz::lsp::registry::enclosing_checkout(&cwd, entries).unwrap_or(&ws.worktree_root);
    let machine = rimz::config::MachineConfig::load_lenient();
    let effective = rimz::config::effective::load(&machine, ws.launch_repo_root())?;
    Ok(rimz::lsp::broker::probe::check_startup(
        root,
        &effective.lsp_servers,
        &effective.untrusted_lsp_servers,
        entries,
    ))
}

fn collect_lsp(
    ws: Option<&rimz::ResolvedWorkspace>,
    globals: &GlobalFlags,
) -> model::Probe<model::Lsp> {
    let collect = || -> Result<model::Lsp> {
        let entries = rimz::lsp::registry::read_entries()?;
        let (checks, checks_error) = match ws.map(|ws| collect_lsp_checks(ws, globals, &entries)) {
            None => (Vec::new(), None),
            Some(Ok(checks)) => (checks, None),
            Some(Err(error)) => (Vec::new(), Some(error.to_string())),
        };
        let last_refusal = rimz::diag::lsp::recent()
            .into_iter()
            .rev()
            .find(|record| matches!(record.event.as_str(), "refused" | "queue_timeout"))
            .filter(|record| !refusal_recovered(record, &checks, &entries));
        Ok(model::Lsp {
            checks,
            checks_error,
            servers: entries
                .into_iter()
                .map(|entry| {
                    let reading = rimz::lsp::memory::for_entry(&entry);
                    model::LspServer {
                        entry,
                        rss_bytes: reading.rss_kb.saturating_mul(1024),
                        swap_bytes: reading.swap_kb.saturating_mul(1024),
                    }
                })
                .collect(),
            last_refusal,
        })
    };
    match collect() {
        Ok(lsp) => model::Probe::Ready(lsp),
        Err(error) => model::Probe::Unavailable {
            diagnosis: None,
            error: error.to_string(),
        },
    }
}

fn refusal_recovered(
    record: &rimz::diag::lsp::Record,
    checks: &[rimz::lsp::broker::probe::Check],
    entries: &[rimz::lsp::registry::Entry],
) -> bool {
    use rimz::lsp::broker::probe::Outcome;
    // The startup probe spawns without admission, so its `Started` proves a
    // configuration repair but says nothing about memory.
    let memory = refusal_shortfall(record).is_some();
    checks.iter().any(|check| {
        check.root == record.root
            && check.server == record.server
            && match check.outcome {
                Outcome::Running { .. } => true,
                Outcome::Started { .. } => !memory,
                _ => false,
            }
    }) || entries.iter().any(|entry| {
        entry.root == record.root
            && entry.server == record.server
            && entry.state.is_running()
            && i128::from(entry.ready_at_ms.unwrap_or(entry.started_at_ms))
                > i128::from(record.at.as_millisecond())
    })
}

/// The memory figures of a refusal admission recorded for lack of memory;
/// `None` for a configuration refusal, whose details carry only its reason.
fn refusal_shortfall(record: &rimz::diag::lsp::Record) -> Option<rimz::lsp::admission::Shortfall> {
    serde_json::from_value(record.details.clone()).ok()
}

fn collect_home() -> model::Home {
    use rimz::disk::paths;
    let home = paths::rimz_home();
    let legacy = paths::legacy_roots();
    model::Home {
        path: home.display().to_string(),
        from_env: paths::env_path("RIMZ_HOME").is_some(),
        agents_home_override: paths::env_path("RIMZ_AGENTS_HOME")
            .map(|path| path.display().to_string()),
        fix: (!legacy.is_empty()).then(|| paths::legacy_roots_fix(&legacy, &home)),
        legacy_roots: legacy
            .iter()
            .map(|root| root.display().to_string())
            .collect(),
    }
}

fn collect_packs() -> model::Probe<rimz::packs::CheckoutReport> {
    match rimz::packs::inspect_checkouts(&rimz::disk::paths::agents_home()) {
        Ok(report) => model::Probe::Ready(report),
        Err(error) => model::Probe::Unavailable {
            error: error.to_string(),
            diagnosis: None,
        },
    }
}

fn collect_machine_config() -> model::MachineConfigHealth {
    let broken_files = rimz::config::broken_machine_files()
        .into_iter()
        .map(|err| {
            let kind = if matches!(err, rimz::config::ConfigErr::Definition(_)) {
                model::MachineConfigProblemKind::Definition
            } else if err.diagnosis().is_some() {
                model::MachineConfigProblemKind::Parse
            } else {
                model::MachineConfigProblemKind::Semantic
            };
            model::MachineConfigProblem {
                diagnosis: err.diagnosis().cloned(),
                path: err.path().display().to_string(),
                error: match &err {
                    rimz::config::ConfigErr::Definition(error) => error.message.clone(),
                    _ => config_file_error_detail(&err, err.diagnosis()),
                },
                kind,
            }
        })
        .collect();
    model::MachineConfigHealth { broken_files }
}

fn config_file_error_detail(
    err: &(dyn std::error::Error + 'static),
    diagnosis: Option<&rimz::config::ConfigFileDiagnosis>,
) -> String {
    diagnosis
        .map(|diagnosis| format!("{}; fix: {}", diagnosis.summary(), diagnosis.fix()))
        .unwrap_or_else(|| ui::one_line_error(err))
}

/// The loop tasks from config plus transient instance state. Read-only and
/// workspace-independent: it surfaces the scheduled-execution surface this box
/// carries, including live clock tasks without a scheduler.
fn collect_loop() -> model::LoopTasks {
    use super::loop_timer::{self, TimerStatus};
    use rimz::harness::schedule::arming;
    use rimz::harness::schedule::catalog::{TaskCatalog, TaskSource, workspace_instance_roots};
    let timer_status = loop_timer::status();
    let timer_active = timer_status.as_ref().is_ok_and(TimerStatus::active);
    let timer = match timer_status {
        Ok(TimerStatus::NotInstalled) => model::LoopTimer::NotInstalled,
        Ok(TimerStatus::Installed {
            backend, active, ..
        }) => model::LoopTimer::Installed {
            backend: backend.label(),
            active,
        },
        Err(error) => model::LoopTimer::Unavailable {
            error: error.to_string(),
        },
    };
    let arming = arming::load();
    let now = jiff::Timestamp::now();
    let mut rooms = std::collections::BTreeMap::new();
    let mut markers = std::collections::BTreeMap::new();
    let mut unscheduled = Vec::new();
    let config = rimz::config::MachineConfig::load_lenient();
    let machine = TaskCatalog::load_lenient(None);
    let instances = workspace_instance_roots()
        .into_iter()
        .flat_map(|root| {
            TaskCatalog::load_lenient(Some(&root))
                .visible()
                .iter()
                .filter(|(_, task)| task.source() == TaskSource::Instance)
                .map(|(name, task)| (name.clone(), task.clone()))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let rows = machine
        .visible()
        .iter()
        .chain(instances.iter().map(|(name, task)| (name, task)))
        .map(|(name, task)| {
            let entry = task.entry();
            let room_open = *rooms
                .entry(entry.resolved_root())
                .or_insert_with_key(|root| {
                    rimz::RuntimePaths::for_project_root(root)
                        .is_ok_and(|runtime| rimz::sidebar::fresh_sidebar_present(&runtime))
                });
            if unscheduled_clock(
                task.trigger(),
                arming.get(&task.key(name)),
                task.source(),
                room_open,
                timer_active,
                now,
            ) {
                unscheduled.push(name.clone());
            }
            let (when, valid) = match (task.trigger(), task.action()) {
                (_, Err(err)) => (format!("invalid: {err}"), false),
                (Err(err), _) => (format!("invalid: {err}"), false),
                (Ok(trigger), Ok(_)) => (trigger.describe(), true),
            };
            let mut held = std::collections::BTreeMap::new();
            for pin in rimz::harness::schedule::catalog::worktree_pins(
                name,
                task,
                arming.get(&task.key(name)),
                now,
            ) {
                let marker = markers.entry(pin.path).or_insert_with_key(|path| {
                    let existing = path.ancestors().find(|ancestor| ancestor.is_dir())?;
                    rimz::worktree::read_marker_for_worktree(existing)
                        .ok()
                        .flatten()
                });
                if let Some(marker) = marker {
                    held.insert(
                        rimz::utils::path::normalize_path_lexical(&marker.worktree_path),
                        marker.name.clone(),
                    );
                }
            }
            model::LoopTaskRow {
                name: name.clone(),
                spec: match task.action() {
                    Ok(
                        rimz::harness::schedule::TaskAction::Spawn(spec)
                        | rimz::harness::schedule::TaskAction::Workflow(spec),
                    ) => spec.clone(),
                    Ok(rimz::harness::schedule::TaskAction::Deliver(target)) => {
                        target.handle.clone()
                    }
                    Ok(rimz::harness::schedule::TaskAction::CheckOnly) => "check".to_owned(),
                    Err(_) => "<invalid>".to_owned(),
                },
                when,
                root: entry.root.display().to_string(),
                valid,
                room_open,
                holds_worktrees: held
                    .into_iter()
                    .map(|(path, name)| model::HeldWorktree { path, name })
                    .collect(),
            }
        })
        .chain(
            config
                .r#loop
                .skipped_tasks
                .0
                .iter()
                .filter_map(|(name, entry)| {
                    let problem = entry.root_problem()?;
                    Some(model::LoopTaskRow {
                        name: name.clone(),
                        spec: "<invalid>".to_owned(),
                        when: format!("invalid: {problem}"),
                        root: entry.root.display().to_string(),
                        valid: false,
                        room_open: false,
                        holds_worktrees: Vec::new(),
                    })
                }),
        )
        .collect();
    model::LoopTasks {
        throttle_errors: rimz::harness::schedule::throttle::check_errors(),
        tasks: rows,
        timer,
        unscheduled,
    }
}

fn unscheduled_clock(
    trigger: &Result<rimz::harness::schedule::ParsedTrigger, rimz::harness::schedule::ScheduleErr>,
    record: Option<&rimz::harness::schedule::arming::Arming>,
    source: rimz::harness::schedule::catalog::TaskSource,
    room_open: bool,
    timer_active: bool,
    now: jiff::Timestamp,
) -> bool {
    use rimz::harness::schedule::{Trigger, arming::ArmState};
    !room_open
        && !timer_active
        && trigger.as_ref().is_ok_and(|parsed| {
            matches!(
                parsed.trigger,
                Trigger::Schedule(_) | Trigger::Condition { .. }
            )
        })
        && ArmState::resolve(record, source, now) == ArmState::Live
}

fn workspace_view(ws: &rimz::ResolvedWorkspace) -> model::Workspace {
    model::Workspace {
        workspace_id: ws.workspace_id.as_str().to_owned(),
        project_root: ws.project_root.display().to_string(),
        root_class: ws.root_class,
        worktree_root: ws.worktree_root.display().to_string(),
        worktree_branch: ws.worktree_branch.clone(),
        session_name: ws.session_name.clone(),
        sock_headroom: runtime::collect_socket_headroom(ws),
    }
}

/// Render or serialize the report to its destination. A file destination writes
/// plain text (ANSI stripped) atomically; stdout flows through the shared styled
/// stream, which keeps color only on a terminal.
fn emit(report: &DoctorReport, json: bool, output: Option<&Path>) -> Result<()> {
    if let Some(path) = output {
        let bytes = if json {
            let mut json =
                serde_json::to_string_pretty(report).context("rendering doctor report JSON")?;
            json.push('\n');
            json.into_bytes()
        } else {
            let mut stream = anstream::StripStream::new(Vec::new());
            render::render_human(report, &mut stream)?;
            stream.into_inner()
        };
        return rimz::disk::atomic::write_bytes_atomically(path, &bytes)
            .with_context(|| format!("writing doctor report to {}", path.display()));
    }

    if json {
        return ui::json_pretty(report);
    }
    render::render_human(report, &mut ui::out())?;
    Ok(())
}

pub fn ping() -> Result<()> {
    #[expect(clippy::print_stdout, reason = "liveness check output")]
    {
        println!("ok");
    }
    Ok(())
}
