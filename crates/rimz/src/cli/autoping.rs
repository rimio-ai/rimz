//! `rimz autoping` — schedule window-priming pings on this machine's OS scheduler.
//!
//! Provider budget windows are sliding: the clock starts on the first billable
//! token. A scheduled ping consumes one token at a time you choose, so the window
//! starts (and resets) on your schedule. Rimz keeps no daemon — the OS scheduler
//! keeps time and fires `rimz autoping run <name>`, which drives a lowest-effort
//! `ping`→`pong` supervised turn through the existing agent harness, bringing the
//! room up if it is closed and clearing the transient card when it finishes.
//!
//! This handler parses, edits the per-machine config, and installs/uninstalls the
//! OS scheduler entry with a consent preview. The pure schedule parsing and
//! artifact rendering live in [`rimz::autoping`]; the supervised-run path is the
//! shared `agents -p` seam.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand, ValueEnum};
use jiff::Timestamp;
use toml_edit::{DocumentMut, Item, Table, value};

use rimz::agents::{find_adapter, hook_trust_fix};
use rimz::autoping::{self, Schedule, Scheduler};
use rimz::config::{MachineConfig, ScheduleEntry};
use rimz::ledger::atomic::write_bytes_atomically;
use rimz::ledger::paths::{RuntimePaths, config_home, runtime_home};
use rimz::sidebar::enrich::shortest_window_running;
use rimz::workspace::WorkspaceResolver;

use super::GlobalFlags;
use super::render as ui;

#[derive(Debug, Args)]
pub struct AutoPingArgs {
    #[command(subcommand)]
    command: AutoPingSubcmd,
}

#[derive(Debug, Subcommand)]
enum AutoPingSubcmd {
    /// Add or replace a schedule in the per-machine config.
    Add(AddArgs),
    /// Remove a schedule from the config (and uninstall its scheduler entry).
    Remove(NameArgs),
    /// List configured schedules and whether each is installed.
    List,
    /// Install configured schedules onto this machine's OS scheduler.
    Install(SelectArgs),
    /// Remove installed scheduler entries, keeping the config.
    Uninstall(SelectArgs),
    /// Run one schedule's ping now. The OS scheduler calls this; humans rarely do.
    #[command(hide = true)]
    Run(NameArgs),
}

#[derive(Debug, Args)]
struct AddArgs {
    /// Schedule name (letters, digits, `-`, `_`).
    name: String,
    /// Agent kind to prime; must support a ping turn (e.g. `claude`, `codex`).
    #[arg(long)]
    kind: String,
    /// Daily firing time, 24-hour `HH:MM` local wall-clock.
    #[arg(long)]
    at: Option<String>,
    /// Day mask: `daily`, `weekdays`, `weekends`, a range `mon-fri`, or a list `mon,wed,fri`.
    #[arg(long)]
    days: Option<String>,
    /// Raw 5-field cron expression (cron backend only; replaces `--at`/`--days`).
    #[arg(long, conflicts_with_all = ["at", "days"])]
    cron: Option<String>,
    /// Project root whose room hosts the ping; resolved to an absolute root.
    #[arg(long, default_value = ".")]
    root: PathBuf,
    /// Optional channel/worktree to host the transient ping pane.
    #[arg(long)]
    worktree: Option<String>,
}

#[derive(Debug, Args)]
struct NameArgs {
    name: String,
}

#[derive(Debug, Args)]
struct SelectArgs {
    /// One schedule name; omit to act on every configured schedule.
    name: Option<String>,
    /// Which OS scheduler to target.
    #[arg(long, value_enum, default_value_t = SchedulerArg::Auto)]
    scheduler: SchedulerArg,
    /// Skip the confirmation prompt.
    #[arg(long, short = 'y')]
    yes: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum SchedulerArg {
    Auto,
    Systemd,
    Cron,
}

pub fn run(args: AutoPingArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        AutoPingSubcmd::Add(args) => add(args),
        AutoPingSubcmd::Remove(args) => remove(&args.name),
        AutoPingSubcmd::List => list(),
        AutoPingSubcmd::Install(args) => install(args),
        AutoPingSubcmd::Uninstall(args) => uninstall(args),
        AutoPingSubcmd::Run(args) => run_one(&args.name, globals),
    }
}

// ---- add / remove -----------------------------------------------------------

fn add(args: AddArgs) -> Result<()> {
    autoping::validate_name(&args.name)?;
    ping_kind_supported(&args.kind)?;
    let workspace = WorkspaceResolver::resolve(&args.root, None)
        .with_context(|| format!("resolving project root at {}", args.root.display()))?;
    let entry = ScheduleEntry {
        kind: args.kind,
        root: workspace.project_root,
        worktree: args.worktree,
        at: args.at,
        days: args.days,
        cron: args.cron,
    };
    // Validate the firing time before writing, so a bad `--at`/`--days` fails here.
    let schedule = autoping::parse_schedule(&args.name, &entry)?;
    config_set_entry(&args.name, &entry)?;

    let mut out = ui::out();
    writeln!(
        out,
        "added autoping `{}`: {} {} in {}",
        args.name,
        entry.kind,
        schedule.describe(),
        entry.root.display()
    )?;
    writeln!(
        out,
        "install it on this machine's scheduler with `rimz autoping install {}`",
        args.name
    )?;
    Ok(())
}

fn remove(name: &str) -> Result<()> {
    // Best-effort scheduler cleanup first, so removing the config never strands an
    // installed timer; missing schedulers are simply skipped.
    if let Ok(scheduler) = detect_scheduler(SchedulerArg::Auto) {
        let _ = uninstall_one(scheduler, name);
    }
    let removed = config_remove(name)?;
    let mut out = ui::out();
    if removed {
        writeln!(out, "removed autoping `{name}`")?;
    } else {
        writeln!(out, "no autoping schedule named `{name}`")?;
    }
    Ok(())
}

// ---- list -------------------------------------------------------------------

fn list() -> Result<()> {
    let schedules = load_schedules()?;
    let mut out = ui::out();
    if schedules.is_empty() {
        writeln!(
            out,
            "no autoping schedules; add one with `rimz autoping add`"
        )?;
        return Ok(());
    }
    let scheduler = detect_scheduler(SchedulerArg::Auto).ok();
    match scheduler {
        Some(scheduler) => writeln!(out, "scheduler: {}", scheduler.label())?,
        None => writeln!(out, "scheduler: none available (systemd --user or crontab)")?,
    }
    for (name, entry) in &schedules {
        let when = match autoping::parse_schedule(name, entry) {
            Ok(schedule) => schedule.describe(),
            Err(err) => format!("invalid: {err}"),
        };
        let state = scheduler
            .map(|scheduler| status_label(scheduler, name))
            .unwrap_or_else(|| "—".to_owned());
        writeln!(
            out,
            "  {name:<16} {} {when:<24} [{state}] {}",
            entry.kind,
            entry.root.display()
        )?;
    }
    Ok(())
}

fn status_label(scheduler: Scheduler, name: &str) -> String {
    match scheduler {
        Scheduler::SystemdUser => match systemctl_is_enabled(name) {
            Some(state) => state,
            None => "not installed".to_owned(),
        },
        Scheduler::Cron => {
            let crontab = read_crontab().unwrap_or_default();
            if autoping::list_crontab(&crontab)
                .iter()
                .any(|entry| entry.name == name)
            {
                "installed".to_owned()
            } else {
                "not installed".to_owned()
            }
        }
    }
}

// ---- install / uninstall ----------------------------------------------------

fn install(args: SelectArgs) -> Result<()> {
    let scheduler = detect_scheduler(args.scheduler)?;
    let names = selected_names(args.name.as_deref())?;
    let rimz_bin = std::env::current_exe().context("locating the rimz executable")?;
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());

    let mut plans = Vec::new();
    for name in &names {
        let entry = load_entry(name)?;
        preflight_kind(&entry.kind)?;
        let schedule = autoping::parse_schedule(name, &entry)?;
        plans.push((
            name.clone(),
            entry,
            build_plan(scheduler, name, &schedule, &rimz_bin, &shell)?,
        ));
    }

    preview_plans(scheduler, &plans)?;
    if !confirmed(
        args.yes,
        &format!("install {} autoping schedule(s)", plans.len()),
    )? {
        bail!("aborted");
    }

    let mut out = ui::out();
    for (name, _entry, plan) in &plans {
        apply_install(name, plan)?;
        writeln!(out, "installed autoping `{name}` ({})", scheduler.label())?;
    }
    if scheduler == Scheduler::SystemdUser && linger_disabled() {
        writeln!(
            out,
            "note: enable lingering so timers fire while you are logged out — `loginctl enable-linger {}`",
            std::env::var("USER").unwrap_or_else(|_| "$USER".to_owned())
        )?;
    }
    Ok(())
}

fn uninstall(args: SelectArgs) -> Result<()> {
    let scheduler = detect_scheduler(args.scheduler)?;
    let names = selected_names(args.name.as_deref())?;
    let mut out = ui::out();
    for name in &names {
        uninstall_one(scheduler, name)?;
        writeln!(out, "uninstalled autoping `{name}` ({})", scheduler.label())?;
    }
    Ok(())
}

/// The rendered scheduler artifacts for one schedule, ready to preview and write.
enum Plan {
    Systemd {
        service_path: PathBuf,
        timer_path: PathBuf,
        service: String,
        timer: String,
    },
    Cron {
        line: String,
    },
}

fn build_plan(
    scheduler: Scheduler,
    name: &str,
    schedule: &Schedule,
    rimz_bin: &Path,
    shell: &str,
) -> Result<Plan> {
    let command = autoping::run_command(rimz_bin, shell, name);
    match scheduler {
        Scheduler::SystemdUser => {
            let oncalendar = autoping::systemd_oncalendar(name, schedule)?;
            let description = autoping::description(name);
            let dir = config_home().join("systemd").join("user");
            let stem = autoping::unit_stem(name);
            Ok(Plan::Systemd {
                service_path: dir.join(format!("{stem}.service")),
                timer_path: dir.join(format!("{stem}.timer")),
                service: autoping::render_systemd_service(&command, &description),
                timer: autoping::render_systemd_timer(&oncalendar, &description),
            })
        }
        Scheduler::Cron => Ok(Plan::Cron {
            line: format!("{} {}", autoping::cron_expr(schedule), command),
        }),
    }
}

fn preview_plans(scheduler: Scheduler, plans: &[(String, ScheduleEntry, Plan)]) -> Result<()> {
    let mut out = ui::out();
    writeln!(out, "rimz autoping install ({})", scheduler.label())?;
    for (name, entry, plan) in plans {
        writeln!(
            out,
            "\n  {name}: {} in {}",
            entry.kind,
            entry.root.display()
        )?;
        match plan {
            Plan::Systemd {
                service_path,
                timer_path,
                service,
                timer,
            } => {
                writeln!(out, "  writes {}", timer_path.display())?;
                indent(&mut out, timer)?;
                writeln!(out, "  writes {}", service_path.display())?;
                indent(&mut out, service)?;
                writeln!(
                    out,
                    "  then: systemctl --user enable --now {}.timer",
                    autoping::unit_stem(name)
                )?;
            }
            Plan::Cron { line } => {
                writeln!(out, "  adds to your crontab:")?;
                indent(&mut out, line)?;
            }
        }
    }
    writeln!(out, "\nundo with `rimz autoping uninstall`")?;
    Ok(())
}

fn indent(out: &mut impl Write, text: &str) -> Result<()> {
    for line in text.lines() {
        writeln!(out, "    {line}")?;
    }
    Ok(())
}

fn apply_install(name: &str, plan: &Plan) -> Result<()> {
    match plan {
        Plan::Systemd {
            service_path,
            timer_path,
            service,
            timer,
        } => {
            write_bytes_atomically(service_path, service.as_bytes())
                .with_context(|| format!("writing {}", service_path.display()))?;
            write_bytes_atomically(timer_path, timer.as_bytes())
                .with_context(|| format!("writing {}", timer_path.display()))?;
            run_systemctl(&["daemon-reload"])?;
            run_systemctl(&[
                "enable",
                "--now",
                &format!("{}.timer", autoping::unit_stem(name)),
            ])?;
            Ok(())
        }
        Plan::Cron { line } => {
            let existing = read_crontab().unwrap_or_default();
            let updated = autoping::splice_crontab(&existing, name, line);
            write_crontab(&updated)
        }
    }
}

fn uninstall_one(scheduler: Scheduler, name: &str) -> Result<()> {
    match scheduler {
        Scheduler::SystemdUser => {
            let stem = autoping::unit_stem(name);
            let dir = config_home().join("systemd").join("user");
            let timer = dir.join(format!("{stem}.timer"));
            // Only disable a timer that exists, so removing an absent schedule
            // stays quiet instead of erroring on a missing unit.
            if timer.exists() {
                run_systemctl_quiet(&["disable", "--now", &format!("{stem}.timer")]);
            }
            let mut removed = false;
            for ext in ["timer", "service"] {
                let path = dir.join(format!("{stem}.{ext}"));
                if path.exists() {
                    std::fs::remove_file(&path)
                        .with_context(|| format!("removing {}", path.display()))?;
                    removed = true;
                }
            }
            if removed {
                run_systemctl_quiet(&["daemon-reload"]);
            }
            Ok(())
        }
        Scheduler::Cron => {
            let existing = read_crontab().unwrap_or_default();
            let updated = autoping::reclaim_crontab(&existing, Some(name));
            if updated != existing {
                write_crontab(&updated)?;
            }
            Ok(())
        }
    }
}

// ---- run --------------------------------------------------------------------

fn run_one(name: &str, globals: &GlobalFlags) -> Result<()> {
    let entry = load_entry(name)?;
    preflight_kind(&entry.kind)?;
    // The ping exists only to *start* a sliding budget window, so a token spent on
    // one already counting down buys nothing — skip it. Best-effort: an unknown or
    // cold reading falls through to the ping.
    if window_already_running(&entry)? {
        writeln!(
            ui::out(),
            "autoping `{name}`: {} budget window already active; skipping ping",
            entry.kind
        )?;
        return Ok(());
    }
    let mut run_globals = globals.clone();
    run_globals.root = Some(entry.root.clone());
    // Drives the shared `agents -p` path; on success it exits with the run's
    // status code and never returns here.
    super::agents_cmd::run_blocking_ping(&entry.kind, entry.worktree.as_deref(), &run_globals)
}

/// Whether `entry`'s provider already has a budget window counting down, read
/// from the shared account-scoped cache. The window state is account-scoped, so
/// the entry's workspace is resolved only to reach this user's runtime root.
fn window_already_running(entry: &ScheduleEntry) -> Result<bool> {
    let workspace = WorkspaceResolver::resolve(&entry.root, None)
        .with_context(|| format!("resolving project root at {}", entry.root.display()))?;
    let runtime = RuntimePaths::under(workspace.workspace_id, &runtime_home())
        .context("locating the runtime root")?;
    Ok(shortest_window_running(&runtime, &entry.kind, Timestamp::now()) == Some(true))
}

// ---- shared helpers ---------------------------------------------------------

/// Validate that a kind can be pinged at all — enforced at add time, before the
/// hooks/trust preconditions a fired ping needs.
fn ping_kind_supported(kind: &str) -> Result<()> {
    let adapter =
        find_adapter(kind).ok_or_else(|| anyhow::anyhow!("unknown agent kind `{kind}`"))?;
    if adapter.ping_args().is_none() {
        bail!("agent kind `{kind}` does not support a ping turn; use `claude` or `codex`");
    }
    Ok(())
}

/// The full precondition a fired ping needs: ping support, plus installed and
/// trusted hooks so the supervised turn can report completion. Enforced at
/// install time (fail fast, with the fix) and again at run time.
fn preflight_kind(kind: &str) -> Result<()> {
    ping_kind_supported(kind)?;
    let adapter =
        find_adapter(kind).ok_or_else(|| anyhow::anyhow!("unknown agent kind `{kind}`"))?;
    if !adapter.hooks_installed() {
        bail!(
            "{kind} hooks are not installed, so the ping cannot report completion; run `rimz hooks install {kind}`"
        );
    }
    let untrusted = adapter.untrusted_installed_hooks();
    if !untrusted.is_empty() {
        bail!(
            "{kind} hooks are installed but not trusted ({}); {}",
            untrusted.join(", "),
            hook_trust_fix(kind)
        );
    }
    Ok(())
}

fn load_schedules() -> Result<std::collections::BTreeMap<String, ScheduleEntry>> {
    Ok(MachineConfig::load()
        .context("loading per-machine config")?
        .autoping
        .schedules
        .0)
}

fn load_entry(name: &str) -> Result<ScheduleEntry> {
    load_schedules()?.remove(name).ok_or_else(|| {
        anyhow::anyhow!("no autoping schedule named `{name}`; see `rimz autoping list`")
    })
}

fn selected_names(name: Option<&str>) -> Result<Vec<String>> {
    match name {
        Some(name) => {
            // Surface a typo before touching the scheduler.
            load_entry(name)?;
            Ok(vec![name.to_owned()])
        }
        None => {
            let names: Vec<String> = load_schedules()?.into_keys().collect();
            if names.is_empty() {
                bail!("no autoping schedules; add one with `rimz autoping add`");
            }
            Ok(names)
        }
    }
}

fn confirmed(yes: bool, prompt: &str) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        bail!("{prompt}? re-run with --yes to proceed without a prompt");
    }
    super::confirm(&format!("{prompt}?"))
}

// ---- config editing (toml_edit, comment-preserving) -------------------------

fn config_set_entry(name: &str, entry: &ScheduleEntry) -> Result<()> {
    let path = MachineConfig::path();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            MachineConfig::template().to_owned()
        }
        Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
    };
    let mut doc = text
        .parse::<DocumentMut>()
        .with_context(|| format!("parsing {}", path.display()))?;

    let mut table = Table::new();
    table["kind"] = value(&entry.kind);
    table["root"] = value(entry.root.to_string_lossy().into_owned());
    if let Some(worktree) = &entry.worktree {
        table["worktree"] = value(worktree);
    }
    if let Some(at) = &entry.at {
        table["at"] = value(at);
    }
    if let Some(days) = &entry.days {
        table["days"] = value(days);
    }
    if let Some(cron) = &entry.cron {
        table["cron"] = value(cron);
    }
    schedules_table(&mut doc)?.insert(name, Item::Table(table));

    let rendered = doc.to_string();
    MachineConfig::parse_text(&path, &rendered)
        .with_context(|| format!("validating `autoping.schedules.{name}`"))?;
    write_bytes_atomically(&path, rendered.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn schedules_table(doc: &mut DocumentMut) -> Result<&mut Table> {
    let autoping = doc
        .as_table_mut()
        .entry("autoping")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .context("`autoping` is not a table")?;
    let schedules = autoping
        .entry("schedules")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .context("`autoping.schedules` is not a table")?;
    schedules.set_implicit(true);
    Ok(schedules)
}

fn config_remove(name: &str) -> Result<bool> {
    let path = MachineConfig::path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(false);
    };
    let mut doc = text
        .parse::<DocumentMut>()
        .with_context(|| format!("parsing {}", path.display()))?;
    let removed = doc
        .get_mut("autoping")
        .and_then(Item::as_table_mut)
        .and_then(|autoping| autoping.get_mut("schedules"))
        .and_then(Item::as_table_mut)
        .map(|schedules| schedules.remove(name).is_some())
        .unwrap_or(false);
    if removed {
        let rendered = doc.to_string();
        write_bytes_atomically(&path, rendered.as_bytes())
            .with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(removed)
}

// ---- OS scheduler glue ------------------------------------------------------

fn detect_scheduler(choice: SchedulerArg) -> Result<Scheduler> {
    match choice {
        SchedulerArg::Systemd => systemd_user_available()
            .then_some(Scheduler::SystemdUser)
            .context("systemd user manager is not available; try `--scheduler cron`"),
        SchedulerArg::Cron => binary_on_path("crontab")
            .then_some(Scheduler::Cron)
            .context("`crontab` was not found on PATH"),
        SchedulerArg::Auto => {
            if systemd_user_available() {
                Ok(Scheduler::SystemdUser)
            } else if binary_on_path("crontab") {
                Ok(Scheduler::Cron)
            } else {
                bail!(
                    "no supported scheduler found; install systemd (--user) or cron, or pass --scheduler"
                )
            }
        }
    }
}

fn systemd_user_available() -> bool {
    binary_on_path("systemctl")
        && Command::new("systemctl")
            .args(["--user", "show-environment"])
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
}

fn binary_on_path(name: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| dir.join(name).exists())
}

fn run_systemctl(args: &[&str]) -> Result<()> {
    let status = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .with_context(|| format!("running `systemctl --user {}`", args.join(" ")))?;
    if !status.success() {
        bail!("`systemctl --user {}` failed", args.join(" "));
    }
    Ok(())
}

/// Run a best-effort `systemctl --user` call, capturing output so a missing
/// unit never leaks an error to the terminal.
fn run_systemctl_quiet(args: &[&str]) {
    let _ = Command::new("systemctl").arg("--user").args(args).output();
}

fn systemctl_is_enabled(name: &str) -> Option<String> {
    let unit = format!("{}.timer", autoping::unit_stem(name));
    let out = Command::new("systemctl")
        .args(["--user", "is-enabled", &unit])
        .output()
        .ok()?;
    let label = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!label.is_empty()).then_some(label)
}

fn linger_disabled() -> bool {
    let Ok(user) = std::env::var("USER") else {
        return false;
    };
    Command::new("loginctl")
        .args(["show-user", &user, "--property=Linger"])
        .output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim() == "Linger=no")
        .unwrap_or(false)
}

fn read_crontab() -> Result<String> {
    let out = Command::new("crontab")
        .arg("-l")
        .output()
        .context("running `crontab -l`")?;
    // A missing crontab exits non-zero with an empty list; treat it as empty.
    Ok(if out.status.success() {
        String::from_utf8_lossy(&out.stdout).into_owned()
    } else {
        String::new()
    })
}

fn write_crontab(content: &str) -> Result<()> {
    use std::process::Stdio;
    let mut child = Command::new("crontab")
        .arg("-")
        .stdin(Stdio::piped())
        .spawn()
        .context("running `crontab -`")?;
    child
        .stdin
        .take()
        .context("opening crontab stdin")?
        .write_all(content.as_bytes())
        .context("writing crontab")?;
    let status = child.wait().context("waiting for `crontab -`")?;
    if !status.success() {
        bail!("`crontab -` failed to install the schedule");
    }
    Ok(())
}
