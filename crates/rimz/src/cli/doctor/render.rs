//! The human `rimz doctor` report: each [`DoctorReport`](super::model::DoctorReport)
//! section as a titled block in the room's palette, with a status glyph carrying
//! the verdict. Built on the shared [`crate::cli::render`] table and key/value
//! primitives, so the report reads like every other `rimz` command and strips to
//! clean text when color is off.

use std::io::{self, Write};

use jiff::Timestamp;

use crate::cli::render::{
    Cell, KeyVals, Table, age_label, cell, fmt_bytes, home_relative, home_relative_path,
    lsp_state_label, paint, palette, status,
};
use rimz::agents::AgentStatus;
use rimz::trust::TrustState;

use super::model::{
    Accounts, AgentCounts, AgentRollup, Capabilities, Diagnostics, DoctorImpact, DoctorReport,
    DoctorState, DuplicateSessions, FolderTrust, Home, HookStatus, Host, LogScope, LoopTasks,
    MachineConfigHealth, MachineConfigProblemKind, MessageProblemRow, Messages, Mux, MuxBinaryRow,
    MuxLog, PluginRow, Presence, PresencePluginRow, PresencePluginStatus, PresencePluginTelemetry,
    PresencePlugins, Probe, Protocols, RemoteAgent, RemoteControl, Room, RoomState, SessionHealth,
    Storage, Terminal, TopologyWriterHealth, Trust, Version, Workspace, ZellijKittyGraphics,
};

/// A section verdict: the glyph and palette tone it renders with.
#[derive(Clone, Copy)]
enum Health {
    Ok,
    Info,
    Neutral,
}

#[derive(Clone, Copy)]
enum Severity {
    Warning,
    Problem,
}

impl Severity {
    fn parts(self) -> (&'static str, anstyle::Style) {
        crate::cli::render::verdict(match self {
            Self::Warning => status::StateRole::Waiting,
            Self::Problem => status::StateRole::Failed,
        })
    }
}

fn parts(health: Health) -> (&'static str, anstyle::Style) {
    let role = match health {
        Health::Ok => status::StateRole::Success,
        Health::Info => status::StateRole::Working,
        Health::Neutral => status::StateRole::Neutral,
    };
    crate::cli::render::verdict(role)
}

/// Running count of what the report found, kept per section so the closing
/// line can point at where to look rather than just how much there is.
#[derive(Default)]
struct Tally {
    section: &'static str,
    audit: bool,
    warns: Vec<Finding>,
    alarms: Vec<Finding>,
}

struct Finding {
    section: &'static str,
    text: String,
    fix: String,
    subject: String,
}

impl Tally {
    fn enter(&mut self, section: &'static str) {
        self.section = section;
    }

    fn record(&mut self, severity: Severity, text: impl Into<String>, fix: impl Into<String>) {
        let text = text.into();
        self.record_subject(severity, text.clone(), fix, text);
    }

    fn record_subject(
        &mut self,
        severity: Severity,
        text: String,
        fix: impl Into<String>,
        subject: String,
    ) {
        let bucket = match severity {
            Severity::Warning => &mut self.warns,
            Severity::Problem => &mut self.alarms,
        };
        bucket.push(Finding {
            section: self.section,
            text,
            fix: fix.into(),
            subject,
        });
    }
}

fn counted(
    tally: &mut Tally,
    severity: Severity,
    text: impl Into<String>,
    fix: impl Into<String>,
) -> Cell {
    tally.record(severity, text, fix);
    let (glyph, style) = severity.parts();
    cell(glyph).fg(style)
}

fn counted_verdict(
    tally: &mut Tally,
    severity: Severity,
    text: impl Into<String>,
    fix: impl Into<String>,
) -> Cell {
    let text = text.into();
    tally.record(severity, text.clone(), fix);
    let (glyph, style) = severity.parts();
    cell(format!("{glyph} {text}")).fg(style)
}

fn counted_note(
    tally: &mut Tally,
    w: &mut impl Write,
    severity: Severity,
    text: &str,
    fix: impl Into<String>,
) -> io::Result<()> {
    tally.record(severity, text, fix);
    let (glyph, style) = severity.parts();
    writeln!(w, "    {}", paint(style, &format!("{glyph} {text}")))
}

/// The sections a bucket touched, in report order and named once each.
fn sections(hits: &[Finding]) -> String {
    let mut seen = Vec::new();
    for hit in hits {
        if !seen.contains(&hit.section) {
            seen.push(hit.section);
        }
    }
    seen.join(", ")
}

/// A glyph-only cell for a table's status column.
fn badge(health: Health) -> Cell {
    let (glyph, style) = parts(health);
    cell(glyph).fg(style)
}

/// A `glyph text` value cell, both painted in the verdict's tone.
fn verdict(health: Health, text: impl Into<String>) -> Cell {
    let (glyph, style) = parts(health);
    cell(format!("{glyph} {}", text.into())).fg(style)
}

fn unavailable_text(error: &str) -> String {
    format!("unavailable ({error})")
}

fn unavailable(tally: &mut Tally, severity: Severity, error: &str, fix: impl Into<String>) -> Cell {
    counted_verdict(tally, severity, unavailable_text(error), fix)
}

fn fit_cell(
    tally: &mut Tally,
    fits: bool,
    used: usize,
    limit: usize,
    path: &str,
    fix: &str,
) -> Cell {
    let text = format!(
        "{} ({used}/{limit} bytes for {path})",
        if fits { "OK" } else { "TOO LONG" }
    );
    if fits {
        verdict(Health::Ok, text)
    } else {
        counted_verdict(tally, Severity::Problem, text, fix)
    }
}

fn style_of(health: Health) -> anstyle::Style {
    parts(health).1
}

/// Open a titled section: a blank line then the heading in the accent tone.
/// Entering also aims the tally, so every finding below is attributed here.
fn section(w: &mut impl Write, tally: &mut Tally, title: &'static str) -> io::Result<()> {
    tally.enter(title);
    writeln!(w)?;
    writeln!(w, "{}", paint(palette::header(), title))
}

/// A hanging note under a section: an indented `glyph text` line.
fn note(w: &mut impl Write, health: Health, text: &str) -> io::Result<()> {
    let (glyph, style) = parts(health);
    writeln!(w, "    {}", paint(style, &format!("{glyph} {text}")))
}

fn detail(w: &mut impl Write, style: anstyle::Style, text: &str) -> io::Result<()> {
    writeln!(w, "      {}", paint(style, text))
}

fn render_sandbox(
    w: &mut impl Write,
    sandbox: &super::model::Sandbox,
    tally: &mut Tally,
) -> io::Result<()> {
    section(w, tally, "SANDBOX")?;
    let mut kv = KeyVals::new().indent(2);
    kv.push("mode", cell(sandbox.mode.to_string()));
    kv.push(
        "bwrap",
        cell(
            sandbox
                .path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "not found".to_owned()),
        ),
    );
    if let Some(version) = &sandbox.version {
        kv.push("version", cell(version));
    }
    let error = sandbox.error.as_deref().unwrap_or("passed");
    kv.push(
        "probe",
        match (sandbox.mode, &sandbox.error) {
            (rimz::config::Isolation::Sandbox, Some(_)) => counted_verdict(
                tally,
                Severity::Problem,
                error,
                "install bubblewrap, or set agents.isolation to host",
            ),
            (rimz::config::Isolation::Host, _) => verdict(Health::Info, error),
            (_, None) => verdict(Health::Ok, "passed"),
        },
    );
    kv.render(w)
}

pub(super) fn render_human(report: &DoctorReport, w: &mut impl Write) -> io::Result<()> {
    let mut tally = Tally {
        audit: report.audit,
        ..Tally::default()
    };
    render_identity(w, report.version, &report.host)?;
    render_workspace(w, &report.workspace, &mut tally)?;
    render_mux(w, &report.mux, &mut tally)?;
    render_terminal(w, &report.terminal, &mut tally)?;
    render_home(w, &report.home, &mut tally)?;
    render_machine_config(w, &report.machine_config, &mut tally)?;
    render_sandbox(w, &report.sandbox, &mut tally)?;
    render_hibernate(w, &report.hibernate, &mut tally)?;
    render_hooks(w, report, &mut tally)?;
    if let Some(trust) = &report.folder_trust {
        render_folder_trust(w, trust, &mut tally)?;
    }
    render_empty_dir(w, &report.empty_dir, &mut tally)?;
    render_accounts(w, &report.accounts, &mut tally)?;
    render_plugins(w, report, &mut tally)?;
    render_loop(w, &report.loop_tasks, &mut tally)?;
    render_lsp(w, &report.lsp, &mut tally)?;
    render_remote_control(w, &report.remote_control, &mut tally)?;
    render_storage(w, &report.disk_usage, &mut tally)?;
    render_temp_units(w, &report.temp_units, &mut tally)?;

    if let Some(protocols) = &report.protocols {
        render_protocols(w, protocols, &mut tally)?;
    }

    if let Some(trust) = &report.trust {
        render_trust(w, trust, &mut tally)?;
    }
    render_agents(w, report, &mut tally)?;
    render_messages(w, report, &mut tally)?;
    render_diagnostics(w, report, &mut tally)?;
    render_last_incident(w, report, &mut tally)?;
    render_tally(w, &tally)?;
    Ok(())
}

fn render_hibernate(
    w: &mut impl Write,
    report: &rimz::harness::hibernate::PreflightReport,
    tally: &mut Tally,
) -> io::Result<()> {
    use rimz::harness::hibernate::CheckStatus;
    section(w, tally, "HIBERNATE")?;
    let mut kv = KeyVals::new().indent(2);
    kv.push("enabled", cell(report.enabled.to_string()));
    kv.push(
        "active",
        cell(report.skipped.as_ref().map_or_else(
            || "yes".into(),
            |reason| {
                let summary = report
                    .checks
                    .iter()
                    .find(|check| !matches!(check.status, CheckStatus::Ok))
                    .and_then(|check| check.summary.as_deref())
                    .unwrap_or(reason);
                format!("skipped: {summary}")
            },
        )),
    );
    for check in &report.checks {
        let fix = check
            .fix
            .unwrap_or("repair the reported hibernation configuration or missing prerequisite");
        let text = check.summary.as_deref().unwrap_or(&check.detail);
        let value = match check.status {
            CheckStatus::Ok => verdict(Health::Ok, text),
            CheckStatus::Warning => counted_verdict(tally, Severity::Warning, text, fix),
            CheckStatus::Error => counted_verdict(tally, Severity::Problem, text, fix),
        };
        kv.push(check.name, value);
    }
    kv.render(w)
}

fn render_temp_units(
    w: &mut impl Write,
    report: &Probe<rimz::harness::tmp_floor::TempUnits>,
    tally: &mut Tally,
) -> io::Result<()> {
    section(w, tally, "TEMP UNITS")?;
    let mut kv = KeyVals::new().indent(2);
    let report = match report {
        Probe::Ready(report) => report,
        Probe::Unavailable { error, .. } => {
            kv.push(
                "free",
                counted_verdict(
                    tally,
                    Severity::Warning,
                    error,
                    "repair the reported configuration or filesystem access error",
                ),
            );
            return kv.render(w);
        }
    };
    let free = format!(
        "{} of {}, floor {}",
        fmt_bytes(report.free_bytes),
        fmt_bytes(report.total_bytes),
        fmt_bytes(report.floor_bytes)
    );
    let free = if report.free_bytes >= report.floor_bytes {
        verdict(Health::Ok, &free)
    } else if report.reclaimable_bytes > 0 {
        counted_verdict(tally, Severity::Warning, &free, "run `rimz gc --tmp-floor`")
    } else {
        counted_verdict(
            tally,
            Severity::Problem,
            &free,
            "stop or restart the agents holding the largest units; `rimz agents show @handle` prints the unit path",
        )
    };
    kv.push("free", free);
    kv.push(
        "reclaimable",
        cell(if report.reclaimable_units == 0 {
            "none".into()
        } else {
            let earliest = match report.earliest_reclaim {
                Some(at) => format!(", earliest reclaim {at}"),
                // Every reclaimable unit is past the grace: it waits for the next sweep.
                None if report.past_grace_units == report.reclaimable_units => String::new(),
                None => ", earliest reclaim unknown".into(),
            };
            format!(
                "{} units, {}; {} past the grace{earliest}",
                report.reclaimable_units,
                fmt_bytes(report.reclaimable_bytes),
                report.past_grace_units,
            )
        }),
    );
    kv.push(
        "in use",
        cell(format!(
            "{} units, {}",
            report.in_use_units,
            fmt_bytes(report.in_use_bytes)
        )),
    );
    kv.render(w)
}

fn render_lsp(
    w: &mut impl Write,
    lsp: &Probe<super::model::Lsp>,
    tally: &mut Tally,
) -> io::Result<()> {
    section(w, tally, "LSP")?;
    let Probe::Ready(lsp) = lsp else {
        if let Probe::Unavailable { error, .. } = lsp {
            counted_note(
                tally,
                w,
                Severity::Warning,
                error,
                "run `rimz lsp list --json` to inspect the registry error",
            )?;
        }
        return Ok(());
    };
    render_lsp_servers(w, &lsp.servers, tally)?;
    for check in &lsp.checks {
        use rimz::lsp::broker::probe::Outcome;
        let name = &check.server;
        match &check.outcome {
            Outcome::Running { .. } => note(w, Health::Ok, &format!("{name} running"))?,
            Outcome::Started { version } => note(
                w,
                Health::Ok,
                &format!(
                    "{name} starts{}",
                    version
                        .as_ref()
                        .map_or_else(String::new, |version| format!(" ({version})"))
                ),
            )?,
            Outcome::Failed { cause, fix } => {
                counted_note(
                    tally,
                    w,
                    Severity::Problem,
                    &format!(
                        "{name} cannot start in {}: {}",
                        home_relative_path(&check.root),
                        cause.exit_summary()
                    ),
                    fix,
                )?;
                render_stderr(w, cause)?;
            }
            Outcome::TimedOut { cause, fix } => {
                counted_note(
                    tally,
                    w,
                    Severity::Warning,
                    &format!("{name} did not answer initialize within 10s"),
                    fix,
                )?;
                render_stderr(w, cause)?;
            }
            Outcome::Invalid { error, fix } => counted_note(
                tally,
                w,
                Severity::Problem,
                &format!("{name}: {error}"),
                fix,
            )?,
            Outcome::Untrusted { .. } => counted_note(
                tally,
                w,
                Severity::Problem,
                &format!("{name}: project configuration is untrusted"),
                "review the project's command paths, then run `rimz trust grant`",
            )?,
        }
    }
    if let Some(error) = &lsp.checks_error {
        counted_note(
            tally,
            w,
            Severity::Problem,
            &format!("cannot check this checkout's language servers: {error}"),
            "repair this checkout's LSP configuration, then re-run `rimz doctor`",
        )?;
    }
    if let Some(record) = &lsp.last_refusal {
        render_lsp_refusal(w, record, tally)?;
    }
    Ok(())
}

fn render_lsp_servers(
    w: &mut impl Write,
    servers: &[super::model::LspServer],
    tally: &mut Tally,
) -> io::Result<()> {
    use rimz::lsp::registry::{State, StopReason};
    if servers.is_empty() {
        return note(w, Health::Neutral, "no shared language servers");
    }
    let mut visible: Vec<_> = servers
        .iter()
        .filter(|server| {
            tally.audit
                || server.entry.state.is_running()
                || matches!(
                    server.entry.state,
                    State::Dormant {
                        reason: Some(StopReason::Crashed),
                        ..
                    }
                )
        })
        .collect();
    visible.sort_by_key(|server| status::lsp_order(&server.entry));
    let hidden = servers.len() - visible.len();
    let mut table = Table::new(["STATE", "CHECKOUT", "SERVER", "RSS", "LEASES"]).right(&[3, 4]);
    for server in &visible {
        let label = lsp_state_label(&server.entry.state);
        let state = if matches!(
            server.entry.state,
            State::Dormant {
                reason: Some(StopReason::Crashed),
                ..
            }
        ) {
            tally.record(
                Severity::Problem,
                format!(
                    "{} {}: {label}",
                    home_relative_path(&server.entry.root),
                    server.entry.server
                ),
                "inspect the crash below and repair the language server, then retry the query",
            );
            let (glyph, style) = Severity::Problem.parts();
            cell(format!("{glyph} {label}")).fg(style)
        } else {
            verdict(
                match status::lsp(&server.entry.state) {
                    status::StateRole::Success => Health::Ok,
                    status::StateRole::Working => Health::Info,
                    _ => Health::Neutral,
                },
                label,
            )
        };
        table.row([
            state,
            cell(home_relative_path(&server.entry.root)),
            cell(&server.entry.server),
            if server.entry.state.is_running() {
                cell(rimz::utils::size::decimal_bytes(server.rss_bytes))
            } else {
                cell("-").dash()
            },
            cell(server.entry.leases.len().to_string()),
        ]);
    }
    if !visible.is_empty() {
        table.render(w)?;
    }
    for server in visible {
        if matches!(
            server.entry.state,
            State::Dormant {
                reason: Some(StopReason::Crashed),
                ..
            }
        ) && let Some(cause) = &server.entry.last_crash
        {
            detail(
                w,
                palette::muted(),
                &format!(
                    "{} {}: {}",
                    home_relative_path(&server.entry.root),
                    server.entry.server,
                    cause.exit_summary()
                ),
            )?;
            render_stderr(w, cause)?;
        }
    }
    if hidden > 0 {
        note(
            w,
            Health::Neutral,
            &format!("{hidden} more dormant or not started — rimz lsp list --all"),
        )?;
    }
    Ok(())
}

fn render_lsp_refusal(
    w: &mut impl Write,
    record: &rimz::diag::lsp::Record,
    tally: &mut Tally,
) -> io::Result<()> {
    let (reason, fix) = if let Ok(shortfall) =
        serde_json::from_value::<rimz::lsp::admission::Shortfall>(record.details.clone())
    {
        let bytes = rimz::utils::size::decimal_bytes;
        let mut text = format!(
            "needs {}, {} available, {} reserved",
            bytes(shortfall.estimate_bytes),
            bytes(shortfall.available_bytes),
            bytes(shortfall.reserve_bytes)
        );
        if !shortfall.holders.is_empty() {
            let holders = shortfall
                .holders
                .iter()
                .map(|holder| {
                    format!(
                        "{} {} ({})",
                        home_relative_path(&holder.root),
                        holder.server,
                        bytes(holder.rss_bytes)
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            text.push_str(&format!("; held by {holders}"));
        }
        (
            text,
            "close unused programs or stop unused language servers to free memory, then retry the query",
        )
    } else {
        (
            record
                .details
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("startup refused")
                .to_owned(),
            "inspect the startup cause in `rimz doctor --json`, repair that cause, then retry the query",
        )
    };
    counted_note(
        tally,
        w,
        Severity::Warning,
        &format!(
            "{} {} {} {}: {reason}",
            home_relative_path(&record.root),
            record.server,
            if record.event == "queue_timeout" {
                "timed out in queue"
            } else {
                "refused"
            },
            age_short(Timestamp::now(), record.at),
        ),
        fix,
    )
}

fn render_stderr(w: &mut impl Write, cause: &rimz::lsp::registry::CrashCause) -> io::Result<()> {
    let lines: Vec<_> = cause.stderr_tail.lines().rev().take(5).collect();
    for line in lines.into_iter().rev() {
        detail(w, palette::muted(), line)?;
    }
    Ok(())
}

fn render_home(w: &mut impl Write, home: &Home, tally: &mut Tally) -> io::Result<()> {
    section(w, tally, "HOME")?;
    let source = if home.from_env {
        "RIMZ_HOME"
    } else {
        "default"
    };
    let mut kv = KeyVals::new().indent(2);
    kv.push(
        "home",
        cell(format!("{} ({source})", home_relative(&home.path))),
    );
    if let Some(agents_home) = &home.agents_home_override {
        kv.push(
            "RIMZ_AGENTS_HOME",
            verdict(
                Health::Info,
                format!(
                    "overrides RIMZ_HOME; profiles, teams, and skills read from {}",
                    home_relative(agents_home)
                ),
            ),
        );
    }
    kv.render(w)?;
    if home.legacy_roots.is_empty() {
        return Ok(());
    }
    let roots = home
        .legacy_roots
        .iter()
        .map(|root| home_relative(root))
        .collect::<Vec<_>>()
        .join(", ");
    counted_note(
        tally,
        w,
        Severity::Warning,
        &format!("legacy roots {roots} are still on disk and not read"),
        format!(
            "move what you still need from {roots} into {} by hand, then delete them (docs/guide/configuration.md#moving-from-the-xdg-roots)",
            home_relative(&home.path)
        ),
    )
}

fn render_machine_config(
    w: &mut impl Write,
    config: &MachineConfigHealth,
    tally: &mut Tally,
) -> io::Result<()> {
    section(w, tally, "MACHINE CONFIG")?;
    if config.broken_files.is_empty() {
        let mut kv = KeyVals::new().indent(2);
        kv.push(
            "config files",
            verdict(Health::Ok, "all present files parse"),
        );
        return kv.render(w);
    }
    for problem in &config.broken_files {
        let summary = problem
            .diagnosis
            .as_ref()
            .map(|diagnosis| diagnosis.summary());
        let error = summary.as_deref().unwrap_or(&problem.error);
        let detail = match problem.kind {
            MachineConfigProblemKind::Definition => {
                format!("{} cannot be used: {}", home_relative(&problem.path), error)
            }
            MachineConfigProblemKind::Parse => format!(
                "{} is unparseable: {}; settings in this file use built-in defaults",
                home_relative(&problem.path),
                error,
            ),
            MachineConfigProblemKind::Semantic => format!(
                "{} is invalid: {}; settings in this file use built-in defaults",
                home_relative(&problem.path),
                error,
            ),
        };
        let fix = if matches!(problem.kind, MachineConfigProblemKind::Definition) {
            "run `rimz agents validate`; launches selecting these definitions are refused until they pass"
        } else {
            "repair the reported configuration file, then re-run `rimz doctor`"
        };
        let fix = problem
            .diagnosis
            .as_ref()
            .map_or(fix, |diagnosis| diagnosis.fix());
        tally.record_subject(
            Severity::Warning,
            detail.clone(),
            fix,
            home_relative(&problem.path),
        );
        let (glyph, style) = Severity::Warning.parts();
        writeln!(w, "    {}", paint(style, &format!("{glyph} {detail}")))?;
    }
    Ok(())
}

fn render_identity(w: &mut impl Write, version: &str, host: &Host) -> io::Result<()> {
    writeln!(w, "{}", paint(palette::header(), "RimZ doctor"))?;
    let mut kv = KeyVals::new().indent(2);
    kv.push("version", cell(version));
    let user = match &host.user {
        Some(name) => format!("{name} (uid {})", host.uid),
        None => format!("uid {}", host.uid),
    };
    kv.push("user", cell(user));
    kv.push(
        "binary",
        cell(host.binary.as_deref().unwrap_or("unknown")).fg(palette::body()),
    );
    kv.render(w)
}

fn render_terminal(w: &mut impl Write, terminal: &Terminal, tally: &mut Tally) -> io::Result<()> {
    section(w, tally, "TERMINAL")?;
    let mut kv = KeyVals::new().indent(2);
    let depth_health = if terminal.resolved_depth == "truecolor" {
        Health::Ok
    } else {
        Health::Neutral
    };
    kv.push(
        "depth",
        verdict(
            depth_health,
            format!(
                "{} (mode {})",
                terminal.resolved_depth,
                terminal_mode_label(terminal.theme_mode)
            ),
        ),
    );
    kv.push(
        "signals",
        cell(format!(
            "truecolor-advertised={} COLORTERM={} TERM={} terminfo-truecolor={}",
            terminal.truecolor_advertised,
            terminal.colorterm.as_deref().unwrap_or("unset"),
            terminal.term.as_deref().unwrap_or("unset"),
            terminal.terminfo_truecolor,
        ))
        .fg(palette::faint()),
    );
    if let Some(fix) = &terminal.fix {
        kv.push(
            "color",
            counted_verdict(tally, Severity::Warning, "truecolor is not advertised", fix),
        );
    }
    kv.render(w)
}

fn terminal_mode_label(mode: rimz::config::ThemeMode) -> &'static str {
    match mode {
        rimz::config::ThemeMode::Auto => "auto",
        rimz::config::ThemeMode::Truecolor => "truecolor",
        rimz::config::ThemeMode::Indexed => "256",
    }
}

fn render_workspace(
    w: &mut impl Write,
    workspace: &Probe<Workspace>,
    tally: &mut Tally,
) -> io::Result<()> {
    section(w, tally, "WORKSPACE")?;
    let mut kv = KeyVals::new().indent(2);
    match workspace {
        Probe::Unavailable { error, .. } => {
            kv.push(
                "workspace",
                counted_verdict(
                    tally,
                    Severity::Problem,
                    format!("could not resolve ({error})"),
                    "run `rimz doctor --root <project-path>` from an existing project directory",
                ),
            );
            kv.render(w)
        }
        Probe::Ready(ws) => {
            kv.push("id", cell(ws.workspace_id.as_str()).fg(palette::accent()));
            kv.push(
                "project root",
                cell(ws.project_root.as_str()).fg(palette::body()),
            );
            kv.push("root class", cell(ws.root_class.label()));
            kv.push(
                "worktree root",
                cell(ws.worktree_root.as_str()).fg(palette::body()),
            );
            kv.push(
                "worktree branch",
                cell(ws.worktree_branch.as_deref().unwrap_or("<detached>")),
            );
            kv.push("session", cell(ws.session_name.as_str()));
            match &ws.sock_headroom {
                Probe::Unavailable { error, .. } => kv.push(
                    "sock headroom",
                    unavailable(
                        tally,
                        Severity::Problem,
                        error,
                        "repair the runtime directory error, then re-run `rimz doctor`",
                    ),
                ),
                Probe::Ready(budget) => {
                    kv.push(
                        "sock headroom",
                        fit_cell(
                            tally,
                            budget.fits,
                            budget.used,
                            budget.limit,
                            &budget.dir,
                            budget
                                .remedy
                                .as_deref()
                                .unwrap_or("shorten the runtime root path, then restart the room"),
                        ),
                    );
                }
            }
            kv.render(w)
        }
    }
}

fn render_mux(w: &mut impl Write, mux: &Probe<Mux>, tally: &mut Tally) -> io::Result<()> {
    section(w, tally, "MULTIPLEXER")?;
    let mux = match mux {
        Probe::Unavailable { error, .. } => {
            let mut kv = KeyVals::new().indent(2);
            kv.push(
                "multiplexer",
                unavailable(
                    tally,
                    Severity::Problem,
                    error,
                    "repair the multiplexer error, then re-run `rimz doctor --json`",
                ),
            );
            return kv.render(w);
        }
        Probe::Ready(mux) => mux,
    };

    let mut kv = KeyVals::new().indent(2);
    kv.push("backend", cell(mux.name.to_string()).fg(palette::accent()));
    kv.push("version", mux_version_cell(tally, &mux.version));
    push_capabilities(&mut kv, tally, &mux.capabilities);
    match &mux.binaries.active {
        Some(active) => kv.push("binary", cell(binary_label(active)).fg(palette::body())),
        None => kv.push(
            "binary",
            counted_verdict(
                tally,
                Severity::Warning,
                "not found on PATH",
                format!("install {} and make it available on PATH", mux.name),
            ),
        ),
    }
    push_mux_log(&mut kv, tally, &mux.log);
    if let Some(socket) = &mux.socket {
        kv.push("socket", cell(socket.as_str()).fg(palette::body()));
    }
    if let Some(socket) = &mux.zellij_socket {
        kv.push(
            "zellij socket",
            fit_cell(
                tally,
                socket.fits,
                socket.len,
                socket.limit,
                &socket.path,
                socket
                    .fix
                    .as_deref()
                    .unwrap_or("shorten the runtime root path, then restart the room"),
            ),
        );
    }
    if let Some(legacy) = &mux.legacy_session {
        kv.push(
            "legacy session",
            counted_verdict(
                tally,
                Severity::Warning,
                format!("{} on {}", legacy.session, legacy.socket),
                &legacy.fix,
            ),
        );
    }
    if let Some(room) = &mux.room {
        kv.push("room", room_cell(tally, mux.name, room));
    }
    if let Some(health) = &mux.session_health {
        kv.push("session health", session_health_cell(tally, health));
    }
    if let Some(presence) = &mux.presence {
        kv.push("presence", presence_cell(tally, presence));
    }
    if let Some(writer) = &mux.topology_writer {
        push_topology_writer(&mut kv, tally, writer);
    }
    if let Some(plugins) = &mux.presence_plugins {
        match plugins {
            Probe::Ready(plugins) => {
                push_presence_plugins(&mut kv, tally, plugins, server_version(&mux.version))
            }
            Probe::Unavailable { error, .. } => kv.push(
                "presence plugin",
                unavailable(
                    tally,
                    Severity::Warning,
                    error,
                    "run `rimz reload`, then re-run `rimz doctor`",
                ),
            ),
        }
    }
    if let Some(ttyd) = &mux.ttyd {
        match ttyd {
            Probe::Ready(ttyd) => kv.push(
                "ttyd web",
                verdict(Health::Ok, format!("{} ({})", ttyd.version, ttyd.path)),
            ),
            Probe::Unavailable { error, .. } => kv.push(
                "ttyd web",
                counted_verdict(
                    tally,
                    Severity::Warning,
                    format!("missing — rimz web needs it; {error}"),
                    "install ttyd and make it available on PATH",
                ),
            ),
        }
    }
    kv.render(w)?;

    render_mux_binary_notes(w, mux, tally)?;
    render_mux_log_notes(w, &mux.log, tally)?;
    render_duplicate_session_notes(w, &mux.duplicate_sessions, tally)
}

fn room_cell(tally: &mut Tally, selected: rimz::MuxName, room: &Room) -> Cell {
    let here = room_state_label(&room.selected_state);
    let rival = selected.other();
    let rival_state = match rival {
        rimz::MuxName::Zellij => &room.zellij,
        rimz::MuxName::Tmux => &room.tmux,
    };
    let label = if matches!(rival_state, RoomState::Live) {
        format!("{here} here; live on {rival}")
    } else {
        here
    };
    if room.conflict {
        counted_verdict(
            tally,
            Severity::Problem,
            format!("{label} (room ownership conflict)"),
            "close the duplicate room on the other multiplexer, then run `rimz start` on the selected backend",
        )
    } else if matches!(room.selected_state, RoomState::Unavailable { .. }) {
        counted_verdict(
            tally,
            Severity::Warning,
            label,
            "repair the multiplexer probe error, then re-run `rimz doctor --json`",
        )
    } else if matches!(room.selected_state, RoomState::Live) {
        verdict(Health::Ok, label)
    } else {
        cell(label).fg(palette::faint())
    }
}

fn room_state_label(state: &RoomState) -> String {
    match state {
        RoomState::Live => "live".to_owned(),
        RoomState::Exited => "exited".to_owned(),
        RoomState::Absent => "absent".to_owned(),
        RoomState::Unavailable { error } => format!("unavailable ({error})"),
    }
}

fn mux_version_cell(tally: &mut Tally, version: &Version) -> Cell {
    match version {
        Version::Reported { version } => cell(version.as_str()),
        Version::Unknown => cell("unknown").fg(palette::faint()),
        Version::Unavailable { error } => unavailable(
            tally,
            Severity::Warning,
            error,
            "check the multiplexer binary on PATH and re-run its version command",
        ),
    }
}

fn push_capabilities(kv: &mut KeyVals, tally: &mut Tally, capabilities: &Capabilities) {
    match capabilities {
        Capabilities::Zellij(Probe::Ready(caps)) => {
            kv.push(
                "zellij floor",
                floor_cell(tally, caps.meets_min_version, caps.min_version),
            );
            let graphics = match caps.kitty_graphics {
                ZellijKittyGraphics::Supported => verdict(
                    Health::Info,
                    "supported by the host terminal; Zellij rooms still draw cell art (Zellij rejects the placement the pixel tier uses)",
                ),
                ZellijKittyGraphics::Unsupported => {
                    verdict(Health::Info, "unsupported by host terminal")
                }
                ZellijKittyGraphics::NoReply => verdict(
                    Health::Info,
                    "no reply (graphics disabled or session predates Zellij 0.45)",
                ),
                ZellijKittyGraphics::BelowMinimum => {
                    verdict(Health::Info, "unavailable (requires Zellij >= 0.45.0)")
                }
                ZellijKittyGraphics::NotProbed => {
                    cell("not probed (outside Zellij or no controlling tty)").fg(palette::faint())
                }
            };
            kv.push("zellij kitty graphics", graphics);
        }
        Capabilities::Zellij(Probe::Unavailable { error, .. }) => {
            kv.push(
                "zellij floor",
                unavailable(
                    tally,
                    Severity::Warning,
                    error,
                    "check `zellij --version` and repair the reported error",
                ),
            );
        }
        Capabilities::Tmux(Probe::Ready(caps)) => {
            kv.push(
                "tmux floor",
                floor_cell(tally, caps.meets_min_version, caps.min_version),
            );
            let (maj, min, patch) = caps.min_version;
            let popup = if caps.popup_supported {
                verdict(Health::Ok, "supported")
            } else {
                counted_verdict(
                    tally,
                    Severity::Warning,
                    format!("unavailable (requires tmux >= {maj}.{min}.{patch})"),
                    format!(
                        "upgrade tmux to at least {maj}.{min}.{patch}, then restart its sessions"
                    ),
                )
            };
            kv.push("tmux popup", popup);
        }
        Capabilities::Tmux(Probe::Unavailable { error, .. }) => {
            kv.push(
                "tmux floor",
                unavailable(
                    tally,
                    Severity::Warning,
                    error,
                    "check `tmux -V` and repair the reported error",
                ),
            );
        }
    }
}

/// Where the log verdict below comes from: the file, how far back the scan
/// reached, and which sessions can write into it. Provenance rather than a
/// verdict, so the issue lines under the section carry the health alone.
fn push_mux_log(kv: &mut KeyVals, tally: &mut Tally, log: &MuxLog) {
    let MuxLog::Ready {
        path,
        scope,
        size_bytes,
        scanned_bytes,
        records_before_cutoff,
        since,
        log_text_omitted,
        ..
    } = log
    else {
        let value = match log {
            MuxLog::Missing { path } => cell(format!("none yet ({path})")).fg(palette::faint()),
            MuxLog::Disabled { hint } => cell(hint.as_str()).fg(palette::faint()),
            MuxLog::Unavailable { error } => unavailable(
                tally,
                Severity::Warning,
                error,
                "repair access to the multiplexer log, then re-run `rimz doctor`",
            ),
            MuxLog::Ready { .. } => unreachable!("matched above"),
        };
        kv.push("log", value);
        return;
    };
    let mut reach = format!(
        "last {} of {}",
        fmt_bytes(*scanned_bytes),
        fmt_bytes(*size_bytes)
    );
    if let Some(since) = since {
        reach.push_str(&format!(
            ", since you cleared {}",
            age_short(Timestamp::now(), *since)
        ));
        if *records_before_cutoff > 0 {
            reach.push_str(&format!(
                " ({records_before_cutoff} older records dismissed)"
            ));
        }
    }
    let scope = match scope {
        LogScope::HostUser { uid } => {
            format!("written by every zellij server running as uid {uid}")
        }
        LogScope::Server => "written by this room's tmux server".to_owned(),
    };
    let mut lines = vec![
        vec![cell(home_relative(path)).fg(palette::body())],
        vec![cell(format!("read {reach} · {scope}")).fg(palette::faint())],
    ];
    if *log_text_omitted {
        lines.push(vec![
            cell("--no-log-text: no log record text in this report").fg(palette::faint()),
        ]);
    }
    kv.push_lines("log", lines);
}

fn session_health_cell(tally: &mut Tally, health: &Probe<SessionHealth>) -> Cell {
    match health {
        Probe::Unavailable { error, .. } => unavailable(
            tally,
            Severity::Warning,
            error,
            "inspect the room with `rimz doctor --json`, then repair the reported multiplexer error",
        ),
        Probe::Ready(SessionHealth::Ok) => verdict(Health::Ok, "ok"),
        Probe::Ready(SessionHealth::Stuck { fix }) => counted_verdict(
            tally,
            Severity::Problem,
            "stuck (resurrected/suspended panes)",
            fix,
        ),
        Probe::Ready(SessionHealth::Unresponsive { fix }) => {
            counted_verdict(tally, Severity::Problem, "live but not responding", fix)
        }
    }
}

fn presence_cell(tally: &mut Tally, presence: &Presence) -> Cell {
    match presence {
        Presence::Event { poked_secs } => {
            verdict(Health::Ok, format!("event mode (poked {poked_secs}s ago)"))
        }
        Presence::Poll {
            reason,
            expected,
            summary,
            fix,
        } => {
            let reason = summary.unwrap_or(reason);
            if *expected {
                verdict(Health::Ok, format!("polling — {reason}"))
            } else {
                counted_verdict(
                    tally,
                    Severity::Warning,
                    format!("polling — {reason}"),
                    fix.unwrap_or("run `rimz reload` to restore event-driven pane discovery"),
                )
            }
        }
        Presence::NotApplicable { reason } => {
            cell(format!("not applicable — {reason}")).fg(palette::faint())
        }
        Presence::Unavailable { error } => {
            tally.record(
                Severity::Problem,
                format!("pane discovery unavailable ({error})"),
                "inspect the pane-discovery error in `rimz doctor --json` and repair its cause",
            );
            let (glyph, style) = Severity::Problem.parts();
            cell(format!("{glyph} unavailable")).fg(style)
        }
    }
}

fn push_topology_writer(kv: &mut KeyVals, tally: &mut Tally, writer: &TopologyWriterHealth) {
    if let Some(bin) = &writer.recorded_bin {
        let label = if bin.exists {
            bin.path.clone()
        } else {
            format!("missing ({})", bin.path)
        };
        kv.push(
            "room binary",
            if bin.exists {
                verdict(Health::Ok, label)
            } else {
                counted_verdict(
                    tally,
                    Severity::Warning,
                    label,
                    bin.fix
                        .as_deref()
                        .unwrap_or("run `rimz reload` to replace the missing room binary"),
                )
            },
        );
    }
    if let Some(conflict) = &writer.conflict {
        let stale = conflict
            .stale
            .as_ref()
            .map(topology_writer_label)
            .unwrap_or_else(|| "a legacy plugin".to_owned());
        let accepted = conflict
            .accepted
            .as_ref()
            .map(topology_writer_label)
            .unwrap_or_else(|| "a legacy plugin".to_owned());
        kv.push(
            "topology writers",
            verdict(Health::Info,
                format!(
                    "contained a stale writer: {stale} lost to {accepted}; {} writes rejected, last {} ago; {}",
                    conflict.rejected_count,
                    age_label(conflict.age_secs),
                    conflict.fix,
                ),
            ),
        );
    }
}

/// The presence-plugin block: a verdict line per loaded plugin, then the
/// identity and traffic that explain it.
///
/// The plugin is the sidebar's eyes on Zellij, so the reader's question is
/// "does the sidebar still see my panes, and if not what do I run". Each row
/// leads with that answer; remedies live in the closing block. Build hash, load time, and the
/// telemetry window follow as subordinate evidence. Counters the report can
/// derive stay off the human surface — `--json` keeps every raw field.
fn push_presence_plugins(
    kv: &mut KeyVals,
    tally: &mut Tally,
    plugins: &PresencePlugins,
    server_version: Option<&str>,
) {
    let desired = plugins.desired_build.as_deref().map(short_build);
    if plugins.rows.is_empty() {
        let want = desired
            .as_deref()
            .map(|build| format!(" (want build {build})"))
            .unwrap_or_default();
        kv.push(
            "presence plugin",
            counted_verdict(
                tally,
                Severity::Warning,
                format!("none loaded{want} — the sidebar cannot see panes"),
                "run `rimz reload`",
            ),
        );
        push_presence_telemetry_log(kv, plugins);
        return;
    }

    let many = plugins.rows.len() > 1;
    if many {
        kv.push(
            "presence plugins",
            counted_verdict(
                tally,
                Severity::Warning,
                format!(
                    "{} loaded — only one may write pane topology",
                    plugins.rows.len()
                ),
                "run `rimz reload`",
            ),
        );
    }
    for row in &plugins.rows {
        let key = if many {
            format!("  plugin #{}", row.plugin_id)
        } else {
            "presence plugin".to_owned()
        };
        let mut lines = vec![
            vec![presence_plugin_verdict(tally, row)],
            vec![
                cell(presence_plugin_identity(
                    row,
                    desired.as_deref(),
                    server_version,
                    many,
                ))
                .fg(palette::body()),
            ],
        ];
        if let Some(traffic) = presence_plugin_traffic(row) {
            lines.push(vec![cell(traffic).fg(palette::faint())]);
        }
        kv.push_lines(key, lines);
    }
    push_presence_telemetry_log(kv, plugins);
}

/// What this plugin is doing for the sidebar right now, and the fix when that
/// answer is unwelcome.
fn presence_plugin_verdict(tally: &mut Tally, row: &PresencePluginRow) -> Cell {
    if row.status == PresencePluginStatus::Rejected {
        return counted_verdict(
            tally,
            Severity::Warning,
            format!(
                "a newer plugin took over — {} of its topology writes were ignored",
                row.rejected_count.unwrap_or_default()
            ),
            "run `rimz reload`",
        );
    }
    if row.status == PresencePluginStatus::Inactive {
        return verdict(Health::Info, "loaded; not writing pane topology");
    }
    let telemetry = row.telemetry.as_ref();
    if telemetry.is_some_and(failing_recently) {
        // The traffic line below owns every count; this line owns the cause.
        let cause = telemetry
            .and_then(|telemetry| telemetry.last_failure.as_ref())
            .map_or_else(|| "pane discovery lags".to_owned(), failure_cause);
        return counted_verdict(
            tally,
            Severity::Warning,
            format!("writing pane topology; wakes are failing — {cause}"),
            if telemetry
                .and_then(|telemetry| telemetry.last_failure.as_ref())
                .is_some()
            {
                "inspect the reported wake failure and repair its cause; capture `rimz doctor --json` if it persists"
            } else {
                "run `rimz reload`"
            },
        );
    }
    if row.outdated {
        return counted_verdict(
            tally,
            Severity::Warning,
            "writing pane topology on an outdated build",
            "run `rimz reload`",
        );
    }
    verdict(Health::Ok, "writing pane topology")
}

/// Whether the plugin reported command failures while its telemetry was still
/// fresh. Stale telemetry describes a plugin that is no longer reporting, and
/// the identity line already says so.
fn failing_recently(telemetry: &PresencePluginTelemetry) -> bool {
    let topology = telemetry.topology_failures_delta.unwrap_or_default();
    let other = telemetry.other_failures_delta.unwrap_or_default();
    let fresh = telemetry.last_seen_age_secs <= rimz::mux::PRESENCE_STAMP_FRESH.as_secs();
    fresh && (topology > 0 || other > 0)
}

/// The host's own account of the failure, falling back to the exit status when
/// it died without saying anything.
fn failure_cause(failure: &super::model::PresenceCommandFailure) -> String {
    if !failure.detail.is_empty() {
        return failure.detail.clone();
    }
    match failure.exit_code {
        Some(code) => format!("the wake exited {code} without reporting a cause"),
        None => "the wake was killed before it could report".to_owned(),
    }
}

/// Which build is loaded, when it loaded, and how recently it reported in.
fn presence_plugin_identity(
    row: &PresencePluginRow,
    desired: Option<&str>,
    server_version: Option<&str>,
    keyed_by_id: bool,
) -> String {
    let mut parts = Vec::new();
    parts.push(match (row.build.as_deref().map(short_build), desired) {
        (Some(build), Some(desired)) if row.outdated => {
            format!("build {build}, outdated (want {desired})")
        }
        (Some(build), Some(_)) => format!("build {build}, current"),
        (Some(build), None) => format!("build {build}"),
        (None, _) => "build unknown".to_owned(),
    });
    if let Some(loaded_at_ms) = row.loaded_at_ms {
        parts.push(format!("loaded {}", plugin_loaded_time(loaded_at_ms)));
    }
    match &row.telemetry {
        Some(telemetry) => parts.push(format!(
            "last report {} ago",
            age_label(telemetry.last_seen_age_secs)
        )),
        None => parts.push("no telemetry yet".to_owned()),
    }
    // The server's own version already heads this section; repeat it only when
    // the plugin loaded under a different one, which a restart resolves.
    if let Some(version) = row
        .telemetry
        .as_ref()
        .and_then(|telemetry| telemetry.zellij_version.as_deref())
        && server_version.is_none_or(|server| !server.contains(version))
    {
        parts.push(format!("loaded under zellij {version}"));
    }
    if !keyed_by_id {
        parts.push(format!("plugin #{}", row.plugin_id));
    }
    parts.join(" · ")
}

/// The work and memory the plugin logged across its telemetry window. Deltas
/// only mean something against the span that produced them, so the window
/// leads.
fn presence_plugin_traffic(row: &PresencePluginRow) -> Option<String> {
    let telemetry = row.telemetry.as_ref()?;
    if telemetry.sample_count < 2 {
        return Some("one telemetry sample so far; no trend yet".to_owned());
    }
    let mut parts = Vec::new();
    let window_secs = telemetry.last_at_ms.saturating_sub(telemetry.first_at_ms) / 1_000;
    if window_secs > 0 {
        parts.push(format!("last {}", age_label(window_secs)));
    }
    parts.push(format!("{} commands", telemetry.commands_completed_delta));
    let topology = telemetry.topology_failures_delta.unwrap_or_default();
    let other = telemetry.other_failures_delta.unwrap_or_default();
    if topology > 0 {
        parts.push(format!("{topology} failed to apply topology"));
    }
    if other > 0 {
        parts.push(format!("{other} other failures"));
    }
    if topology == 0 && other == 0 {
        parts.push("all applied".to_owned());
    }
    if let Some(rejected) = telemetry
        .stale_writer_rejections_delta
        .filter(|delta| *delta > 0)
    {
        parts.push(format!("{rejected} writes rejected as stale"));
    }
    // `bytes` is `pages * 64 KiB`; one of the two is enough, and bytes are the
    // ones a reader can judge.
    parts.push(match telemetry.byte_growth {
        0 => "memory steady".to_owned(),
        growth if growth > 0 => format!("memory +{}", fmt_bytes(growth.unsigned_abs())),
        growth => format!("memory -{}", fmt_bytes(growth.unsigned_abs())),
    });
    Some(parts.join(" · "))
}

fn push_presence_telemetry_log(kv: &mut KeyVals, plugins: &PresencePlugins) {
    if let Some(path) = plugins.history.first() {
        let rotated = if plugins.history.len() > 1 {
            " (+ rotated .1)"
        } else {
            ""
        };
        kv.push(
            "telemetry log",
            cell(format!("{path}{rotated}")).fg(palette::faint()),
        );
    }
}

fn server_version(version: &Version) -> Option<&str> {
    match version {
        Version::Reported { version } => Some(version.as_str()),
        Version::Unknown | Version::Unavailable { .. } => None,
    }
}

fn plugin_loaded_time(loaded_at_ms: u64) -> String {
    i64::try_from(loaded_at_ms)
        .ok()
        .and_then(|millis| Timestamp::from_millisecond(millis).ok())
        .map(|timestamp| timestamp.strftime("%H:%M:%S").to_string())
        .unwrap_or_else(|| loaded_at_ms.to_string())
}

fn short_build(build: &str) -> String {
    build.chars().take(8).collect()
}

fn render_duplicate_session_notes(
    w: &mut impl Write,
    duplicate_sessions: &Option<Probe<DuplicateSessions>>,
    tally: &mut Tally,
) -> io::Result<()> {
    if let Some(Probe::Ready(dup)) = duplicate_sessions {
        if dup.groups.is_empty() {
            note(w, Health::Ok, "duplicate sessions: none")?;
        } else {
            counted_note(
                tally,
                w,
                Severity::Warning,
                &format!(
                    "duplicate sessions: {} live sessions share this workspace; pane updates can be held",
                    dup.groups.len()
                ),
                dup.advice
                    .as_deref()
                    .unwrap_or("close the duplicate room sessions, then run `rimz reload`"),
            )?;
            for group in &dup.groups {
                let here = if group.is_current { "* " } else { "  " };
                let panes = if group.pane_ids.is_empty() {
                    "unlocated".to_owned()
                } else {
                    group.pane_ids.join(", ")
                };
                detail(
                    w,
                    palette::body(),
                    &format!(
                        "{here}{}/{}: {} sidebars ({panes})",
                        group.mux, group.session_name, group.sidebar_count
                    ),
                )?;
            }
        }
    } else if let Some(Probe::Unavailable { error, .. }) = duplicate_sessions {
        counted_note(
            tally,
            w,
            Severity::Warning,
            &format!("duplicate sessions: {}", unavailable_text(error)),
            "repair the multiplexer probe error, then re-run `rimz doctor --json`",
        )?;
    }
    Ok(())
}

/// Name a writer generation the way the presence-plugin rows do, so the two
/// rows describe the same plugin in the same words.
fn topology_writer_label(writer: &super::model::TopologyWriterId) -> String {
    format!(
        "plugin #{} loaded {}",
        writer.plugin_id,
        plugin_loaded_time(writer.loaded_at_ms)
    )
}

fn binary_label(row: &MuxBinaryRow) -> String {
    match &row.version {
        Some(version) => format!("{} — {version}", row.path),
        None => row.path.clone(),
    }
}

fn render_mux_binary_notes(w: &mut impl Write, mux: &Mux, tally: &mut Tally) -> io::Result<()> {
    if !mux.binaries.duplicates.is_empty() {
        counted_note(
            tally,
            w,
            Severity::Warning,
            &format!(
                "multiple {} binaries on PATH — clients and servers can mismatch",
                mux.name
            ),
            "keep one multiplexer binary on PATH; remove or shadow the rest",
        )?;
        if let Some(active) = &mux.binaries.active {
            detail(w, palette::body(), &format!("* {}", binary_label(active)))?;
        }
        for install in &mux.binaries.duplicates {
            detail(w, palette::body(), &format!("  {}", binary_label(install)))?;
        }
    }
    if mux.binaries.active.is_none() {
        return Ok(());
    }
    for server in &mux.binaries.server_mismatches {
        let deleted = if server.deleted { " (deleted)" } else { "" };
        counted_note(
            tally,
            w,
            Severity::Warning,
            &format!(
                "running {} server (pid {}) uses {}{}",
                mux.name, server.pid, server.exe, deleted
            ),
            "restart this server's sessions on the PATH binary",
        )?;
    }
    Ok(())
}

/// The log's verdict: one line per issue worth a human's time, then a single
/// line accounting for the lifecycle records the room provokes by running.
///
/// Expected records outnumber real ones by orders of magnitude in a busy room,
/// so they earn a count and a naming, never a line each. An issue under
/// investigation leads with what it means, and only an alarm carries its raw
/// record, because that is the only case where the reader needs the forensics.
fn render_mux_log_notes(w: &mut impl Write, log: &MuxLog, tally: &mut Tally) -> io::Result<()> {
    let MuxLog::Ready {
        problem_records,
        omitted_issue_groups,
        issues,
        ..
    } = log
    else {
        return Ok(());
    };
    if *problem_records == 0 {
        return note(w, Health::Ok, "log: nothing to report");
    }
    let (expected, investigate): (Vec<_>, Vec<_>) = issues
        .iter()
        .partition(|issue| issue.state != DoctorState::Investigate);

    if investigate.is_empty() {
        note(w, Health::Ok, "log: nothing needing attention")?;
    }
    for issue in investigate {
        let text = format!("{} ({})", issue.summary, issue_span(issue));
        if let Some(severity) = doctor_severity(issue.state, issue.impact) {
            counted_note(
                tally,
                w,
                severity,
                &text,
                "inspect the multiplexer log evidence; repair the reported cause, then run `rimz reload`",
            )?;
        } else {
            note(w, Health::Info, &text)?;
        }
        if issue.impact == DoctorImpact::Alarm {
            for sample in &issue.samples {
                detail(w, palette::muted(), sample.trim_end())?;
            }
            if issue.evidence_truncated {
                detail(w, palette::muted(), "evidence truncated at 8 KiB")?;
            }
        }
    }
    if tally.audit {
        for issue in &expected {
            note(
                w,
                Health::Info,
                &format!("{} ({})", issue.summary, issue_span(issue)),
            )?;
        }
    } else if !expected.is_empty() {
        let occurrences: usize = expected.iter().map(|issue| issue.occurrences).sum();
        note(
            w,
            Health::Info,
            &format!(
                "{occurrences} records are routine room lifecycle: {}",
                naming(expected.iter().map(|issue| issue.summary.as_str()))
            ),
        )?;
    }
    if *omitted_issue_groups > 0 {
        detail(
            w,
            palette::muted(),
            &format!("{omitted_issue_groups} older issue groups omitted"),
        )?;
    }
    Ok(())
}

/// How often an issue fired and how recently, in the reader's own terms.
fn issue_span(issue: &super::model::MuxLogIssue) -> String {
    let count = match issue.occurrences {
        1 => "once".to_owned(),
        count => format!("{count}×"),
    };
    let now = Timestamp::now();
    // A burst inside one second has no span worth naming; reporting it as
    // "over 0s" reads as a broken clock rather than a tight cluster.
    let span_secs = match (issue.first_occurrence, issue.last_occurrence) {
        (Some(first), Some(last)) => u64::try_from(last.duration_since(first).as_secs())
            .ok()
            .filter(|secs| *secs > 0),
        _ => None,
    };
    match (span_secs, issue.first_occurrence, issue.last_occurrence) {
        (Some(secs), _, Some(last)) => format!(
            "{count} over {}, last {}",
            age_label(secs),
            age_short(now, last)
        ),
        (_, Some(at), None) | (_, _, Some(at)) => format!("{count}, {}", age_short(now, at)),
        (_, None, None) => count,
    }
}

/// Name the first few of a set and count the rest, so a fold still says what it
/// swallowed.
fn naming<'a>(summaries: impl Iterator<Item = &'a str>) -> String {
    const NAMED: usize = 3;
    let all: Vec<_> = summaries.collect();
    let named = all
        .iter()
        .take(NAMED)
        .copied()
        .collect::<Vec<_>>()
        .join(" · ");
    match all.len().saturating_sub(NAMED) {
        0 => named,
        rest => format!("{named} · and {rest} more"),
    }
}

fn floor_cell(tally: &mut Tally, meets: bool, min: (u32, u32, u32)) -> Cell {
    let (maj, min_v, patch) = min;
    let text = format!(
        "{} (>= {maj}.{min_v}.{patch} required)",
        if meets { "OK" } else { "TOO OLD" }
    );
    if meets {
        verdict(Health::Ok, text)
    } else {
        counted_verdict(
            tally,
            Severity::Problem,
            text,
            format!(
                "upgrade the multiplexer to at least {maj}.{min_v}.{patch}, then restart its sessions"
            ),
        )
    }
}

fn render_empty_dir(
    w: &mut impl Write,
    empty_dir: &super::model::EmptyDir,
    tally: &mut Tally,
) -> io::Result<()> {
    use crate::cli::folder_trust::State;
    if empty_dir.rows.is_empty()
        && matches!(empty_dir.check, Probe::Ready(()))
        && empty_dir.profiles_error.is_none()
    {
        return Ok(());
    }
    section(w, tally, "EMPTY-DIR")?;
    if let Probe::Unavailable { error, .. } = &empty_dir.check {
        counted_note(tally, w, Severity::Warning, error, empty_dir.check_fix)?;
    }
    if let Some(error) = &empty_dir.profiles_error {
        counted_note(
            tally,
            w,
            Severity::Warning,
            error,
            "repair the reported profile configuration, then run `rimz agents validate`",
        )?;
    }
    if empty_dir.rows.is_empty() {
        return Ok(());
    }
    let mut table = Table::new(["", "AGENT", "LOGIN", "PROFILES", "STATUS"]);
    for row in &empty_dir.rows {
        let fix = match &row.trust.state {
            State::Decided => continue,
            State::Undecided { fix, .. } | State::Restricted { fix, .. } => fix,
        };
        table.row([
            counted(
                tally,
                Severity::Warning,
                format!(
                    "{}@{} falls back to the parent directory for profiles {}",
                    row.trust.kind,
                    row.trust.login,
                    row.profiles.join(", ")
                ),
                fix,
            ),
            cell(row.trust.kind),
            cell(row.trust.login.as_str()),
            cell(row.profiles.join(", ")),
            cell("they start in the parent's directory instead").fg(palette::warn()),
        ]);
    }
    table.render(w)
}

fn render_folder_trust(
    w: &mut impl Write,
    trust: &Probe<FolderTrust>,
    tally: &mut Tally,
) -> io::Result<()> {
    use crate::cli::folder_trust::State;
    section(w, tally, "FOLDER TRUST")?;
    let trust = match trust {
        Probe::Ready(trust) => trust,
        Probe::Unavailable { error, .. } => {
            return counted_note(
                tally,
                w,
                Severity::Warning,
                error,
                "repair the reported account configuration, then re-run `rimz doctor`",
            );
        }
    };
    if trust.rows.is_empty() {
        return detail(w, palette::muted(), "no detected agent models folder trust");
    }
    let decided: Vec<_> = trust
        .rows
        .iter()
        .filter(|row| matches!(row.state, State::Decided))
        .map(|row| format!("{}@{}", row.kind, row.login))
        .collect();
    if !decided.is_empty() {
        detail(
            w,
            palette::muted(),
            &format!("{} decided: {}", decided.len(), decided.join(", ")),
        )?;
    }
    if decided.len() == trust.rows.len() {
        return Ok(());
    }
    let mut table = Table::new(["", "AGENT", "LOGIN", "STATUS"]);
    for row in &trust.rows {
        let (status, fix) = match &row.state {
            State::Decided => continue,
            State::Undecided { fix, .. } => ("undecided", fix),
            State::Restricted { fix, .. } => ("restricted (saved untrusted)", fix),
        };
        table.row([
            counted(
                tally,
                Severity::Warning,
                format!("{}@{} folder trust is {status}", row.kind, row.login),
                fix,
            ),
            cell(row.kind),
            cell(row.login.as_str()),
            cell(status).fg(palette::warn()),
        ]);
    }
    table.render(w)
}

/// Hook wiring, sorted by what the reader can act on.
///
/// A working agent is worth one word, so the installed set collapses to a
/// single roll-up and the table holds only the agents asking for a command.
/// Agents absent from the machine stay in a closing aside: they are the
/// section's largest group and its least interesting one. Names render plain
/// here — provider color marks agents at work, and a wiring audit is not that.
fn render_hooks(w: &mut impl Write, report: &DoctorReport, tally: &mut Tally) -> io::Result<()> {
    section(w, tally, "HOOKS")?;
    let mut installed = Vec::new();
    let mut needs_action = Vec::new();
    let mut unsupported = Vec::new();
    let mut absent = Vec::new();
    for row in &report.hooks {
        match &row.status {
            HookStatus::Installed => installed.push(row.kind.as_str()),
            HookStatus::InstalledUntrusted { .. }
            | HookStatus::InstalledDisabled { .. }
            | HookStatus::NotInstalled { .. }
            | HookStatus::Blocked { .. } => {
                needs_action.push(row);
            }
            HookStatus::Unsupported { reason } if row.detected => {
                unsupported.push((row.kind.as_str(), reason.as_str()));
            }
            HookStatus::NotDetected | HookStatus::Unsupported { .. } => {
                absent.push(row.kind.as_str());
            }
        }
    }

    if installed.is_empty() {
        writeln!(
            w,
            "  {}",
            paint(palette::faint(), "no agent reports to RimZ yet")
        )?;
    } else {
        installed.sort_unstable();
        writeln!(
            w,
            "  {}",
            paint(
                palette::muted(),
                &format!(
                    "{} reporting to RimZ: {}",
                    installed.len(),
                    installed.join(", ")
                )
            )
        )?;
    }

    if !needs_action.is_empty() {
        let mut table = Table::new(["", "AGENT", "STATUS"]);
        for row in needs_action {
            let (severity, fix) = match &row.status {
                HookStatus::InstalledDisabled { fix } => (Severity::Warning, fix.clone()),
                HookStatus::InstalledUntrusted { events, fix } => (
                    Severity::Warning,
                    format!(
                        "silently skips untrusted hooks ({}) — {fix}",
                        events.join(", ")
                    ),
                ),
                HookStatus::NotInstalled { fix } | HookStatus::Blocked { fix } => {
                    (Severity::Problem, fix.clone())
                }
                _ => unreachable!("only actionable statuses reach the table"),
            };
            table.row([
                counted(
                    tally,
                    severity,
                    format!("{} {}", row.kind, row.status.label()),
                    fix,
                ),
                cell(row.kind.as_str()),
                cell(row.status.label()).fg(severity.parts().1),
            ]);
        }
        table.render(w)?;
    }

    for (kind, reason) in unsupported {
        note(
            w,
            Health::Neutral,
            &format!("{kind} is installed but cannot carry RimZ hooks"),
        )?;
        detail(w, palette::faint(), reason)?;
    }

    if !absent.is_empty() {
        absent.sort_unstable();
        note(
            w,
            Health::Neutral,
            &format!("not found on this machine: {}", absent.join(", ")),
        )?;
        detail(
            w,
            palette::faint(),
            "RimZ offers their hooks as soon as one appears",
        )?;
    }
    Ok(())
}

/// Named accounts and the room's selection. A broken account the room launches
/// under is an alarm; one nothing uses yet is a warning.
fn render_accounts(
    w: &mut impl Write,
    accounts: &Probe<Accounts>,
    tally: &mut Tally,
) -> io::Result<()> {
    let accounts = match accounts {
        Probe::Ready(accounts) if accounts.rows.is_empty() => return Ok(()),
        Probe::Ready(accounts) => accounts,
        Probe::Unavailable { error, .. } => {
            section(w, tally, "ACCOUNTS")?;
            return counted_note(
                tally,
                w,
                Severity::Problem,
                error,
                "repair the reported account configuration, then run `rimz accounts list`",
            );
        }
    };
    section(w, tally, "ACCOUNTS")?;
    let mut table = Table::new(["", "KIND", "NAME", "HOME", "STATUS", "DEFAULT FOR"]);
    for row in &accounts.rows {
        use super::model::AccountStanding;
        let name = format!("{}@{}", row.kind, row.name);
        let agents = row
            .usage
            .as_ref()
            .filter(|usage| !usage.agents.is_empty())
            .map(|usage| {
                format!(
                    "; {} paused or failed on it: {}",
                    plural(usage.agents.len(), "agent"),
                    usage.agents.join(", ")
                )
            })
            .unwrap_or_default();
        let (badge, status, style) = if let Some(problem) = &row.problem {
            let text = &problem.summary;
            let fix = &problem.fix;
            let severity = if row.room {
                Severity::Problem
            } else {
                Severity::Warning
            };
            (
                counted(tally, severity, format!("{name}: {text}{agents}"), fix),
                format!("{text}{agents}"),
                severity.parts().1,
            )
        } else if let Some(usage) = &row.usage {
            match usage.standing {
                AccountStanding::Spent => (
                    badge(Health::Info),
                    format!(
                        "spent, resets in {}, read {}{agents}",
                        usage
                            .resets_at
                            .map(|at| rimz::theme::fmt::reset_countdown(at, Timestamp::now()))
                            .unwrap_or_default(),
                        age_short(Timestamp::now(), usage.read_at),
                    ),
                    style_of(Health::Info),
                ),
                AccountStanding::Exhausted => (
                    counted(
                        tally,
                        Severity::Warning,
                        format!(
                            "{name} is out of credits with no reset coming, read {}{agents}",
                            age_short(Timestamp::now(), usage.read_at)
                        ),
                        "add credits to the account",
                    ),
                    format!(
                        "out of credits, no reset, read {}{agents}",
                        age_short(Timestamp::now(), usage.read_at)
                    ),
                    palette::warn(),
                ),
                AccountStanding::Ready => (
                    badge(Health::Ok),
                    format!("ready, read {}", age_short(Timestamp::now(), usage.read_at)),
                    palette::good(),
                ),
            }
        } else {
            (badge(Health::Ok), "ready".to_owned(), palette::good())
        };
        table.row([
            badge,
            cell(row.kind.as_str()).fg(palette::identity(&row.kind)),
            cell(row.name.as_str()),
            cell(
                row.home
                    .as_deref()
                    .map_or_else(|| "-".to_owned(), home_relative),
            )
            .dash(),
            cell(status).fg(style),
            cell(row.default_for.label()).dash(),
        ]);
    }
    table.render(w)
}

fn render_plugins(w: &mut impl Write, report: &DoctorReport, tally: &mut Tally) -> io::Result<()> {
    if report.plugins.is_empty() {
        return Ok(());
    }
    section(w, tally, "AGENT PLUGINS")?;
    let mut table = Table::new(["", "AGENT", "STATUS", "MANIFEST", "DETAIL"]);
    for plugin in &report.plugins {
        let (severity, status, detail) = plugin_verdict(plugin);
        table.row([
            match severity {
                Some(severity) => counted(
                    tally,
                    severity,
                    format!("{}: {status}", plugin.kind),
                    plugin
                        .setup_doc
                        .as_deref()
                        .map(|doc| format!("follow {doc}"))
                        .unwrap_or_else(|| {
                            format!(
                                "repair the plugin manifest at {} and its probe commands",
                                home_relative(&plugin.manifest)
                            )
                        }),
                ),
                None => badge(Health::Ok),
            },
            cell(plugin.kind.as_str()).fg(palette::identity(&plugin.kind)),
            cell(status).fg(severity.map_or(palette::good(), |severity| severity.parts().1)),
            cell(home_relative(&plugin.manifest)).fg(palette::body()),
            cell(detail).dash(),
        ]);
        push_probe_rows(&mut table, tally, plugin);
    }
    table.render(w)
}

fn plugin_verdict(plugin: &PluginRow) -> (Option<Severity>, &'static str, String) {
    if !plugin.valid {
        return (
            Some(Severity::Problem),
            "invalid",
            plugin
                .error
                .clone()
                .unwrap_or_else(|| "manifest validation failed".to_owned()),
        );
    }
    let bad_probes = plugin
        .probes
        .iter()
        .filter(|probe| !probe.present || !probe.executable)
        .map(|probe| probe.name)
        .collect::<Vec<_>>();
    if bad_probes.is_empty() {
        (
            None,
            "valid",
            plugin
                .setup_doc
                .clone()
                .unwrap_or_else(|| "self-managed hooks".to_owned()),
        )
    } else {
        (
            Some(Severity::Warning),
            "valid; probe unavailable",
            bad_probes.join(", "),
        )
    }
}

fn push_probe_rows(table: &mut Table, tally: &mut Tally, plugin: &PluginRow) {
    for probe in &plugin.probes {
        let status = match (probe.present, probe.executable) {
            (true, true) => "executable",
            (true, false) => "not executable",
            (false, _) => "missing",
        };
        table.row([
            if probe.present && probe.executable {
                badge(Health::Ok)
            } else {
                counted(
                    tally,
                    Severity::Warning,
                    format!("{} {} probe {status}", plugin.kind, probe.name),
                    format!(
                        "install or make executable the configured probe command: {}",
                        probe.command
                    ),
                )
            },
            cell(format!("  {} probe", probe.name)).fg(palette::meta()),
            cell(status).fg(if probe.present && probe.executable {
                palette::good()
            } else {
                palette::warn()
            }),
            cell("-"),
            cell(probe.command.as_str()).fg(palette::body()),
        ]);
    }
}

fn render_loop(w: &mut impl Write, loop_tasks: &LoopTasks, tally: &mut Tally) -> io::Result<()> {
    use super::model::LoopTimer;
    section(w, tally, "LOOP TASKS")?;
    if loop_tasks.tasks.is_empty() {
        return writeln!(w, "  {}", paint(palette::faint(), "none configured"));
    }
    let needs_you = loop_tasks
        .tasks
        .iter()
        .filter(|row| !row.valid || loop_tasks.unscheduled.contains(&row.name))
        .count();
    let summary = if needs_you == 0 {
        "none need you".to_owned()
    } else {
        format!(
            "{} {} you",
            needs_you,
            if needs_you == 1 { "needs" } else { "need" }
        )
    };
    note(
        w,
        Health::Neutral,
        &format!(
            "{}, {summary} — rimz loop list",
            plural(loop_tasks.tasks.len(), "task")
        ),
    )?;
    let mut table = Table::new(["", "NAME", "TARGET", "WHEN", "ROOT", "ROOM"]);
    let mut visible = 0;
    for row in &loop_tasks.tasks {
        let unscheduled = loop_tasks.unscheduled.contains(&row.name);
        if !tally.audit && row.valid && !unscheduled && row.holds_worktrees.is_empty() {
            continue;
        }
        visible += 1;
        let badge = if !row.valid {
            counted(
                tally,
                Severity::Problem,
                format!("{}: {}", row.name, row.when),
                "repair the task's trigger or action in loop.toml, then inspect `rimz loop list`",
            )
        } else if unscheduled {
            tally.record_subject(
                Severity::Warning,
                format!(
                    "{} has no scheduler: no room is open for its root and the loop timer is not active",
                    row.name
                ),
                "run `rimz loop timer install`, then `rimz loop timer status`, or `rimz start` in the task's root",
                format!("{} (no scheduler)", row.name),
            );
            let (glyph, style) = Severity::Warning.parts();
            cell(glyph).fg(style)
        } else {
            badge(Health::Info)
        };
        let mut when = row.when.clone();
        for held in &row.holds_worktrees {
            when.push_str(&format!(" · holds worktree {}", held.name));
        }
        table.row([
            badge,
            cell(&row.name).fg(palette::accent()),
            cell(&row.spec),
            cell(when),
            cell(home_relative(&row.root)).fg(palette::body()),
            if row.room_open {
                cell("room open").fg(palette::good())
            } else {
                cell("no room").fg(palette::muted())
            },
        ]);
    }
    if visible > 0 {
        table.render(w)?;
    }
    let value = match &loop_tasks.timer {
        LoopTimer::NotInstalled => verdict(Health::Neutral, "not installed"),
        LoopTimer::Installed { backend, active } => verdict(
            Health::Neutral,
            format!(
                "{} ({backend})",
                if *active { "active" } else { "inactive" }
            ),
        ),
        LoopTimer::Unavailable { error } => unavailable(
            tally,
            Severity::Warning,
            error,
            "run `rimz loop timer status` and repair the reported timer error",
        ),
    };
    let mut kv = KeyVals::new().indent(2);
    kv.push("loop timer", value);
    kv.render(w)
}

fn render_remote_control(
    w: &mut impl Write,
    remote: &RemoteControl,
    tally: &mut Tally,
) -> io::Result<()> {
    section(w, tally, "REMOTE CONTROL")?;
    match remote {
        RemoteControl::Unavailable { error, diagnosis } => {
            let summary = diagnosis.as_ref().map(|diagnosis| diagnosis.summary());
            let error = summary.as_deref().unwrap_or(error);
            let fix = diagnosis.as_ref().map_or(
                "repair the remote-control configuration, then re-run `rimz doctor`",
                |diagnosis| diagnosis.fix(),
            );
            let mut kv = KeyVals::new().indent(2);
            kv.push(
                "remote control",
                counted_verdict(
                    tally,
                    Severity::Problem,
                    format!("config unavailable ({error})"),
                    fix,
                ),
            );
            kv.render(w)
        }
        RemoteControl::Off => {
            let mut kv = KeyVals::new().indent(2);
            kv.push("remote control", cell("off").fg(palette::faint()));
            kv.render(w)
        }
        RemoteControl::On {
            agents,
            refusals,
            skipped,
            advisories,
        } => render_remote_on(w, agents, refusals, skipped, advisories, tally),
    }
}

fn render_remote_on(
    w: &mut impl Write,
    agents: &[RemoteAgent],
    refusals: &[String],
    skipped: &[String],
    advisories: &[String],
    tally: &mut Tally,
) -> io::Result<()> {
    let mut kv = KeyVals::new().indent(2);
    for agent in agents {
        let value = if agent.ready {
            verdict(Health::Ok, &agent.detail)
        } else {
            counted_verdict(
                tally,
                Severity::Warning,
                &agent.detail,
                "repair the provider's remote-control configuration, or disable it in [remote-control]",
            )
        };
        kv.push(agent.kind, value);
    }
    kv.render(w)?;
    for agent in agents {
        let Some(blocker) = &agent.blocker else {
            continue;
        };
        detail(
            w,
            palette::muted(),
            &format!("{} would still refuse:", agent.kind),
        )?;
        for line in blocker.lines() {
            detail(w, palette::muted(), line)?;
        }
    }
    for refusal in refusals {
        counted_note(
            tally,
            w,
            Severity::Problem,
            "`rimz start` refuses remote control",
            refusal.lines().collect::<Vec<_>>().join(" "),
        )?;
    }
    for skip in skipped {
        counted_note(
            tally,
            w,
            Severity::Warning,
            "enabled but not installed — skipped (the room still starts)",
            skip.lines().collect::<Vec<_>>().join(" "),
        )?;
    }
    for advisory in advisories {
        counted_note(
            tally,
            w,
            Severity::Warning,
            "provider daemon advisory (no start impact)",
            advisory.lines().collect::<Vec<_>>().join(" "),
        )?;
    }
    Ok(())
}

fn render_storage(w: &mut impl Write, disk_usage: &Storage, tally: &mut Tally) -> io::Result<()> {
    section(w, tally, "STORAGE")?;
    writeln!(
        w,
        "  {}",
        paint(
            palette::muted(),
            &format!("rimz on disk: {}", fmt_bytes(disk_usage.total_bytes))
        )
    )?;
    let mut table = Table::new(["", "AREA", "SIZE", "PATH"]).right(&[2]);
    for root in &disk_usage.roots {
        let size = if root.present {
            fmt_bytes(root.bytes)
        } else {
            "-".to_owned()
        };
        table.row([
            badge(Health::Neutral),
            cell(root.label),
            cell(size).dash(),
            cell(home_relative(&root.path)).fg(if root.present {
                palette::body()
            } else {
                palette::faint()
            }),
        ]);
    }
    table.render(w)
}

fn render_protocols(
    w: &mut impl Write,
    protocols: &Protocols,
    tally: &mut Tally,
) -> io::Result<()> {
    section(w, tally, "PROTOCOLS")?;
    let mut kv = KeyVals::new().indent(2);
    kv.push("event", cell(protocols.event));
    kv.push("sidebar", cell(protocols.sidebar));
    kv.render(w)?;
    for warning in &protocols.warnings {
        counted_note(
            tally,
            w,
            Severity::Warning,
            warning.detail.as_deref().unwrap_or(&warning.text),
            warning.fix,
        )?;
    }
    if let Some(drift) = &protocols.build_drift {
        counted_note(
            tally,
            w,
            Severity::Warning,
            &format!(
                "mixed rimz builds writing this workspace: {} distinct builds",
                drift.writers.len(),
            ),
            "run `rimz reload` to converge",
        )?;
        for writer in &drift.writers {
            let tag = if writer.is_running {
                " (this binary)"
            } else {
                ""
            };
            let location = if writer.sidebar_count == 0 {
                "no live sidebar".to_owned()
            } else if writer.pane_ids.is_empty() {
                format!("{} sidebars: unlocated", writer.sidebar_count)
            } else {
                format!(
                    "{} sidebars: {}",
                    writer.sidebar_count,
                    writer.pane_ids.join(", ")
                )
            };
            detail(
                w,
                palette::body(),
                &format!("{}{tag}: {location}", writer.build),
            )?;
        }
    }
    Ok(())
}

fn render_trust(w: &mut impl Write, trust: &Probe<Trust>, tally: &mut Tally) -> io::Result<()> {
    section(w, tally, "TRUST")?;
    let mut kv = KeyVals::new().indent(2);
    let value = match trust {
        Probe::Unavailable { error, diagnosis } => {
            let summary = diagnosis.as_ref().map(|diagnosis| diagnosis.summary());
            let error = summary.as_deref().unwrap_or(error);
            let fix = diagnosis.as_ref().map_or(
                "repair the trust-state error, then run `rimz trust status`",
                |diagnosis| diagnosis.fix(),
            );
            unavailable(tally, Severity::Problem, error, fix)
        }
        Probe::Ready(Trust { state, granted_at }) => match state {
            TrustState::Trusted => verdict(
                Health::Ok,
                format!(
                    "trusted (granted {})",
                    granted_at.as_deref().unwrap_or("<unknown>")
                ),
            ),
            TrustState::Stale => counted_verdict(
                tally,
                Severity::Problem,
                "stale (executable surface drifted)",
                "review the changed command paths, then run `rimz trust grant`",
            ),
            TrustState::Untrusted => counted_verdict(
                tally,
                Severity::Warning,
                "untrusted (command paths disabled)",
                "review the project's command paths, then run `rimz trust grant`",
            ),
            TrustState::NoConfig => verdict(Health::Neutral, "no project config"),
        },
    };
    kv.push("trust", value);
    kv.render(w)
}

fn render_agents(w: &mut impl Write, report: &DoctorReport, tally: &mut Tally) -> io::Result<()> {
    let Some(rollup) = &report.agents else {
        return Ok(());
    };
    section(w, tally, "AGENTS")?;
    match rollup {
        AgentRollup::Unavailable { error } => counted_note(
            tally,
            w,
            Severity::Problem,
            &unavailable_text(error),
            "inspect `rimz agents list --json` and repair the reported store error",
        ),
        AgentRollup::None => writeln!(w, "  {}", paint(palette::faint(), "none observed")),
        AgentRollup::Observed { counts, rows } => {
            writeln!(
                w,
                "  {}",
                paint(palette::muted(), &agent_counts_line(counts, report.audit))
            )?;
            if rows.is_empty() {
                return Ok(());
            }
            let now = Timestamp::now();
            let mut table = Table::new([
                "", "HANDLE", "ACCOUNT", "BRANCH", "STATUS", "REASON", "SEEN",
            ]);
            for agent in rows {
                let badge = if agent.attributed_to_account
                    || agent.historical
                    || agent.status == AgentStatus::Waiting
                {
                    badge(Health::Info)
                } else if matches!(agent.status, AgentStatus::Failed | AgentStatus::Paused) {
                    let fix = if agent.restartable {
                        format!("rimz agents restart {}", agent.handle)
                    } else {
                        format!(
                            "start a new {} agent; this session cannot be restarted in place",
                            agent.kind
                        )
                    };
                    counted(
                        tally,
                        if agent.status == AgentStatus::Failed {
                            Severity::Problem
                        } else {
                            Severity::Warning
                        },
                        format!(
                            "{} on {} is {}{}",
                            agent.handle,
                            agent.account,
                            agent.status.as_str(),
                            agent
                                .turn_error
                                .as_ref()
                                .map(|reason| format!(": {reason}"))
                                .unwrap_or_default()
                        ),
                        fix,
                    )
                } else {
                    badge(status_health(agent.status))
                };
                let style = status::agent(agent.status, agent.phase);
                table.row([
                    badge,
                    cell(agent.handle.as_str()).fg(palette::accent()),
                    cell(agent.account.as_str()).fg(palette::identity(&agent.kind)),
                    cell(agent.branch.as_deref().unwrap_or("-")).dash(),
                    cell(agent.status.as_str()).fg(style),
                    cell(agent.turn_error.as_deref().unwrap_or("-")).dash(),
                    cell(age_short(now, agent.last_seen)),
                ]);
            }
            table.render(w)
        }
    }
}

fn agent_counts_line(counts: &AgentCounts, audit: bool) -> String {
    let mut parts = Vec::new();
    push_count(&mut parts, counts.running, "running");
    push_count(&mut parts, counts.waiting, "waiting");
    push_count(&mut parts, counts.idle, "idle");
    push_count(&mut parts, counts.success, "success");
    push_count(&mut parts, counts.failed, "failed");
    push_count(&mut parts, counts.paused, "paused");
    push_count(&mut parts, counts.sleeping, "sleeping");
    let scope = if audit { "observed" } else { "live" };
    if parts.is_empty() {
        format!("0 {scope}")
    } else {
        format!("{} {scope}: {}", counts.total(), parts.join(", "))
    }
}

fn push_count(parts: &mut Vec<String>, count: usize, label: &str) {
    if count > 0 {
        parts.push(format!("{count} {label}"));
    }
}

fn render_messages(w: &mut impl Write, report: &DoctorReport, tally: &mut Tally) -> io::Result<()> {
    let Some(messages) = &report.messages else {
        return Ok(());
    };
    section(w, tally, "MESSAGES")?;
    let messages = match messages {
        Probe::Unavailable { error, .. } => {
            return counted_note(
                tally,
                w,
                Severity::Problem,
                &unavailable_text(error),
                "inspect `rimz message list --all --system` and repair the reported store error",
            );
        }
        Probe::Ready(messages) => messages,
    };
    if messages.open.total() == 0 && messages.stuck.is_empty() && messages.failure_counts.is_empty()
    {
        return writeln!(w, "  {}", paint(palette::faint(), "no open messages"));
    }
    if messages.open.total() == 0 {
        writeln!(
            w,
            "  {}",
            paint(palette::muted(), "0 open — `rimz message list`")
        )?;
    } else {
        writeln!(
            w,
            "  {}",
            paint(
                palette::muted(),
                &format!(
                    "{} open: {} — `rimz message list`",
                    messages.open.total(),
                    open_counts_line(messages)
                )
            )
        )?;
    }
    if !messages.failure_counts.is_empty() {
        let total = messages.failure_counts.values().sum();
        let counts = messages
            .failure_counts
            .iter()
            .map(|(status, count)| {
                format!(
                    "{count} {}",
                    match status.as_str() {
                        "timed_out" => "timed out",
                        "errored" => "errored",
                        "abandoned" => "abandoned",
                        "expired" => "expired",
                        other => other,
                    }
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        note(
            w,
            Health::Info,
            &format!(
                "{}: {counts} — rimz message list --all --system",
                plural(total, "recent failure")
            ),
        )?;
    }
    render_message_rows(w, messages, tally)
}

fn open_counts_line(messages: &Messages) -> String {
    let mut parts = Vec::new();
    push_count(&mut parts, messages.open.queued, "queued");
    push_count(&mut parts, messages.open.claimed, "claimed");
    push_count(&mut parts, messages.open.sent, "sent");
    parts.join(", ")
}

fn render_message_rows(
    w: &mut impl Write,
    messages: &Messages,
    tally: &mut Tally,
) -> io::Result<()> {
    if messages.stuck.is_empty() && (!tally.audit || messages.recent_failures.is_empty()) {
        return Ok(());
    }
    let now = Timestamp::now();
    let mut table = Table::new(["", "ID", "STATUS", "TARGET", "AGE", "PROBLEM"]);
    for row in &messages.stuck {
        render_message_row(&mut table, tally, row, true, now);
    }
    if tally.audit {
        for row in &messages.recent_failures {
            render_message_row(&mut table, tally, row, false, now);
        }
    }
    table.render(w)
}

fn render_message_row(
    table: &mut Table,
    tally: &mut Tally,
    row: &MessageProblemRow,
    stuck: bool,
    now: Timestamp,
) {
    table.row([
        if stuck {
            counted(
                tally,
                Severity::Warning,
                format!(
                    "{} to {} is stuck: {}",
                    row.message_id, row.target, row.problem
                ),
                format!(
                    "inspect `rimz message show {}` and resolve the reported delivery problem",
                    row.message_id
                ),
            )
        } else {
            badge(Health::Info)
        },
        cell(row.message_id.as_str()).fg(palette::accent()),
        cell(row.status.as_str()).fg(if stuck {
            palette::warn()
        } else {
            style_of(Health::Info)
        }),
        cell(row.target.as_str()).fg(palette::meta()),
        cell(age_short(now, row.at)),
        cell(row.problem.as_str()).fg(palette::body()),
    ]);
}

fn render_diagnostics(
    w: &mut impl Write,
    report: &DoctorReport,
    tally: &mut Tally,
) -> io::Result<()> {
    let Some(diagnostics) = &report.diagnostics else {
        return Ok(());
    };
    section(w, tally, "DIAGNOSTICS")?;
    if let Some(cleared_at) = report.history_cleared_at {
        writeln!(
            w,
            "  {}",
            paint(
                palette::muted(),
                &format!(
                    "history cleared {}",
                    age_short(Timestamp::now(), cleared_at)
                )
            )
        )?;
    }
    match diagnostics {
        Diagnostics::Unavailable => writeln!(w, "  {}", paint(palette::faint(), "unavailable")),
        Diagnostics::Ready { path, incidents } if incidents.is_empty() => writeln!(
            w,
            "  {}",
            paint(palette::faint(), &format!("no recent records ({path})"))
        ),
        // An incident RimZ already understood — expected, contained, or
        // recovered — is evidence that the machinery worked. Those earn a
        // summary by kind; audit includes them in the table as context.
        Diagnostics::Ready { path, incidents } => {
            let (settled, open): (Vec<_>, Vec<_>) = incidents
                .iter()
                .partition(|incident| incident.state != DoctorState::Investigate);
            writeln!(
                w,
                "  {}",
                paint(
                    palette::muted(),
                    &format!("{} recent incidents ({path})", incidents.len())
                )
            )?;
            let now_ms = rimz::utils::time::unix_now_ms();
            let mut table = Table::new(["", "KIND", "SEEN", "SUMMARY"]).right(&[2]);
            let audit = tally.audit;
            for incident in incidents
                .iter()
                .filter(|incident| audit || incident.state == DoctorState::Investigate)
            {
                let severity = doctor_severity(incident.state, incident.impact);
                let badge = if let Some(severity) = severity {
                    counted(
                        tally,
                        severity,
                        format!("{}: {}", incident.kind, incident_summary(incident)),
                        "inspect the diagnostic evidence in `rimz doctor --json`; repair the reported cause before clearing history",
                    )
                } else {
                    badge(Health::Info)
                };
                table.row([
                    badge,
                    cell(incident.kind.as_str())
                        .fg(severity.map_or(style_of(Health::Info), |severity| severity.parts().1)),
                    cell(incident_seen(now_ms, incident)),
                    cell(incident_summary(incident)).fg(palette::body()),
                ]);
            }
            if tally.audit || !open.is_empty() {
                table.render(w)?;
            }
            if !tally.audit && !settled.is_empty() {
                let mut kinds: Vec<_> = settled
                    .iter()
                    .map(|incident| incident.kind.as_str())
                    .collect();
                kinds.sort_unstable();
                kinds.dedup();
                note(
                    w,
                    Health::Info,
                    &format!(
                        "{} handled and closed themselves: {}",
                        settled.len(),
                        naming(kinds.into_iter())
                    ),
                )?;
            }
            Ok(())
        }
    }
}

fn incident_seen(now_ms: u64, incident: &super::model::DiagIncident) -> String {
    if incident.first_at_ms == incident.last_at_ms {
        return age_ms_short(now_ms, incident.last_at_ms);
    }
    format!(
        "{} to {}",
        age_ms_short(now_ms, incident.first_at_ms),
        age_ms_short(now_ms, incident.last_at_ms)
    )
}

fn incident_summary(incident: &super::model::DiagIncident) -> String {
    let mut summary = incident.summary.clone();
    if incident.record_count > 1 {
        summary.push_str(&format!(" · {} records", incident.record_count));
    }
    if incident.sink_suppressed > 0 {
        summary.push_str(&format!(" · {} suppressed", incident.sink_suppressed));
    }
    if incident.stale_build
        && let Some(build) = &incident.build
    {
        // ponytail: SUMMARY is the unpadded final column; add styled Cell spans
        // before giving this table a max width.
        summary.push_str(&paint(palette::muted(), &format!(" · old build {build}")));
    }
    summary
}

fn doctor_severity(state: DoctorState, impact: DoctorImpact) -> Option<Severity> {
    if state != DoctorState::Investigate {
        return None;
    }
    match impact {
        DoctorImpact::Alarm => Some(Severity::Problem),
        DoctorImpact::Warn => Some(Severity::Warning),
        DoctorImpact::Info => None,
    }
}

fn render_last_incident(
    w: &mut impl Write,
    report: &DoctorReport,
    tally: &mut Tally,
) -> io::Result<()> {
    let Some(incident) = &report.last_incident else {
        return Ok(());
    };
    section(w, tally, "LAST INCIDENT")?;
    let now = Timestamp::now();
    note(
        w,
        Health::Info,
        &format!(
            "{} · {} ({})",
            incident.cause,
            incident.at.strftime("%Y-%m-%d %H:%M"),
            age_short(now, incident.at),
        ),
    )?;
    Ok(())
}

/// Counted findings in severity order, each paired with its next action.
fn render_tally(w: &mut impl Write, tally: &Tally) -> io::Result<()> {
    writeln!(w)?;
    if tally.alarms.is_empty() && tally.warns.is_empty() {
        return writeln!(
            w,
            "{}",
            paint(palette::good(), "✓ everything checked is healthy")
        );
    }
    let mut parts = Vec::new();
    if !tally.alarms.is_empty() {
        parts.push(format!(
            "✗ {} in {}",
            plural(tally.alarms.len(), "problem"),
            sections(&tally.alarms)
        ));
    }
    if !tally.warns.is_empty() {
        parts.push(format!(
            "! {} in {}",
            plural(tally.warns.len(), "warning"),
            sections(&tally.warns)
        ));
    }
    let headline = if tally.alarms.is_empty() {
        palette::warn()
    } else {
        palette::alarm()
    };
    writeln!(w, "{}", paint(headline, &parts.join("  ·  ")))?;
    for (severity, findings) in [
        (Severity::Problem, &tally.alarms),
        (Severity::Warning, &tally.warns),
    ] {
        let (glyph, style) = severity.parts();
        let mut groups: Vec<Vec<&Finding>> = Vec::new();
        for finding in findings {
            if let Some(group) = groups
                .iter_mut()
                .find(|group| group[0].section == finding.section && group[0].fix == finding.fix)
            {
                group.push(finding);
            } else {
                groups.push(vec![finding]);
            }
        }
        for group in groups {
            let finding = group[0];
            let text = if group.len() == 1 {
                finding.text.clone()
            } else {
                format!(
                    "{} findings: {}",
                    group.len(),
                    group
                        .iter()
                        .map(|finding| finding.subject.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            writeln!(
                w,
                "  {}",
                paint(style, &format!("{glyph} {}: {text}", finding.section))
            )?;
            writeln!(
                w,
                "      {}",
                paint(palette::muted(), &format!("fix: {}", finding.fix))
            )?;
        }
    }
    Ok(())
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

fn status_health(status: AgentStatus) -> Health {
    match status {
        AgentStatus::Running | AgentStatus::Success => Health::Ok,
        AgentStatus::Waiting | AgentStatus::Paused | AgentStatus::Failed => Health::Info,
        AgentStatus::Idle | AgentStatus::Sleeping => Health::Neutral,
    }
}

/// Relative age of `then` from `now`, rendered compactly (`5s ago`, `2m ago`).
fn age_short(now: Timestamp, then: Timestamp) -> String {
    if now.duration_since(then).is_negative() {
        return "now".to_owned();
    }
    format!("{} ago", crate::cli::render::age_short(then, now))
}

fn age_ms_short(now_ms: u64, then_ms: u64) -> String {
    format!(
        "{} ago",
        crate::cli::render::age_label(now_ms.saturating_sub(then_ms) / 1_000)
    )
}

#[cfg(test)]
mod tests;
