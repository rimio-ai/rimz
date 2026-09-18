//! `rimz uninstall` — remove RimZ's machine-wide footprint.

use std::collections::{BTreeSet, HashSet};
use std::env;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use clap::Args;

use super::GlobalFlags;
use super::loop_timer::{self, TimerStatus};
use super::render::fmt_bytes;
use rimz::agents::skill_links::{self, Desired, SkillLinkPlan};
use rimz::disk::paths;
use rimz::disk::usage::{RuntimeStorage, StorageKind, StorageRoot};
use rimz::ids::{MuxName, WorkspaceId};
use rimz::mux::{self, MuxErr};
use rimz::uninstall::{RemovalOutcome, Removed};
use rimz::workspace::{KnownWorkspace, known_workspaces};

#[derive(Debug, Args)]
pub struct UninstallArgs {
    /// Also delete durable stores, spend history, and shared state.
    #[arg(long)]
    pub state: bool,
    /// Also delete per-machine config, themes, trust grants, and notification handlers.
    #[arg(long)]
    pub config: bool,
    /// Delete state and config in addition to the default runtime/cache/data wipe.
    #[arg(long)]
    pub all: bool,
    /// Leave rimz binaries in place.
    #[arg(long)]
    pub keep_binary: bool,
    /// Skip the confirmation prompt (required off a TTY).
    #[arg(long)]
    pub yes: bool,
}

#[derive(Clone, Debug)]
struct LiveRoom {
    mux: MuxName,
    workspace_id: WorkspaceId,
    session_name: String,
}

struct Preview<'a> {
    disk_usage: &'a RuntimeStorage,
    remove_state: bool,
    remove_config: bool,
    live_rooms: &'a [LiveRoom],
    hook_agents: &'a [String],
    skill_links: &'a [SkillLinkPlan],
    loop_timer: &'a TimerStatus,
    keep_binary: bool,
    binaries: &'a [PathBuf],
    project_dirs: &'a [PathBuf],
}

pub fn run(args: UninstallArgs, _globals: &GlobalFlags) -> Result<()> {
    let remove_state = args.state || args.all;
    let remove_config = args.config || args.all;
    let mut failures = Vec::new();
    let mut sudo_hints = Vec::new();

    let workspaces = match known_workspaces() {
        Ok(workspaces) => workspaces,
        Err(err) => {
            failures.push(format!("read known workspaces: {err}"));
            Vec::new()
        }
    };
    ensure_not_in_rimz_room(&workspaces)?;

    let disk_usage = rimz::disk::usage::measure();
    let (live_rooms, session_failures) = live_rooms(&workspaces);
    failures.extend(session_failures);
    let (logins, accounts_err) = super::hooks::provider_home_logins();
    if let Some(err) = accounts_err {
        failures.push(format!(
            "read provider accounts: {err}; only the providers' own homes were cleaned"
        ));
    }
    let hook_agents = logins
        .iter()
        .filter(|(_, adapter, env)| adapter.managed_hook_artifacts_present(env))
        .map(|(key, _, _)| hook_label(key))
        .collect::<Vec<_>>();
    let library = paths::skills_library();
    let skill_roots = logins
        .iter()
        .filter_map(|(_, adapter, env)| adapter.skills_home(env))
        .collect::<BTreeSet<_>>();
    let mut skill_links = Vec::new();
    for root in skill_roots {
        match skill_links::plan(&root, &library, Desired::None) {
            Ok(plan) if plan.has_owned_changes() => skill_links.push(plan),
            Ok(_) => {}
            Err(err) => failures.push(format!("plan skill link removal: {err}")),
        }
    }
    let timer_status = loop_timer::status().unwrap_or(TimerStatus::NotInstalled);
    let binaries = if args.keep_binary {
        Vec::new()
    } else {
        rimz::uninstall::binary_candidates(
            env::current_exe().ok(),
            cargo_bin_dir(),
            &system_bin_dir(),
        )
    };
    let project_dirs = project_local_dirs(&workspaces);

    render_preview(Preview {
        disk_usage: &disk_usage,
        remove_state,
        remove_config,
        live_rooms: &live_rooms,
        hook_agents: &hook_agents,
        skill_links: &skill_links,
        loop_timer: &timer_status,
        keep_binary: args.keep_binary,
        binaries: &binaries,
        project_dirs: &project_dirs,
    })?;
    if !confirm_uninstall(&args)? {
        return Ok(());
    }

    let mut stderr = std::io::stderr().lock();
    writeln!(stderr, "\nUninstalling RimZ...")?;

    teardown_rooms(&live_rooms, &mut stderr, &mut failures)?;

    match super::hooks::uninstall_managed_hooks() {
        // The accounts config error, if any, was recorded with the preview.
        Ok((reports, _)) if reports.is_empty() => writeln!(stderr, "Hooks: none installed")?,
        Ok((reports, _)) => {
            let agents = reports
                .iter()
                .map(|(key, _)| hook_label(key))
                .collect::<Vec<_>>()
                .join(", ");
            writeln!(stderr, "Hooks: removed {agents}")?;
        }
        Err(err) => failures.push(format!("remove managed hooks: {err}")),
    }

    let mut removed_skill_links = false;
    for plan in &skill_links {
        match skill_links::apply(plan) {
            Ok(outcome) => {
                if outcome.unlinked > 0 {
                    removed_skill_links = true;
                    writeln!(
                        stderr,
                        "Skill links: removed {} from {}",
                        outcome.unlinked,
                        plan.root().display()
                    )?;
                }
                if let Some(report) = plan.shadowed_report(&outcome.shadowed) {
                    writeln!(stderr, "{report}")?;
                }
            }
            Err(err) => failures.push(format!("remove skill links: {err}")),
        }
    }
    if !removed_skill_links {
        writeln!(stderr, "Skill links: none")?;
    }

    match loop_timer::remove() {
        Ok(report) if report.changed => {
            writeln!(stderr, "Timer: removed ({})", report.backend.label())?;
        }
        Ok(_) => writeln!(stderr, "Timer: none installed")?,
        Err(err) => failures.push(format!("remove loop timer: {err}")),
    }

    remove_roots(
        &disk_usage,
        remove_state,
        remove_config,
        &mut stderr,
        &mut failures,
    )?;

    if args.keep_binary {
        writeln!(stderr, "Binaries: kept (--keep-binary)")?;
    } else {
        let outcomes = rimz::uninstall::remove_binaries(&binaries);
        render_removal_outcomes(
            "Binaries",
            &outcomes,
            &mut stderr,
            &mut failures,
            Some(&mut sudo_hints),
        )?;
    }

    writeln!(stderr, "Project .rimz dirs: left in place")?;
    for path in &project_dirs {
        writeln!(stderr, "  {}", path.display())?;
    }

    if failures.is_empty() {
        writeln!(stderr, "Uninstall complete.")?;
        return Ok(());
    }

    let mut message = String::from("uninstall incomplete:");
    for failure in &failures {
        message.push_str("\n  - ");
        message.push_str(failure);
    }
    if !sudo_hints.is_empty() {
        message.push_str("\nProtected binary paths may need sudo:");
        for path in &sudo_hints {
            message.push_str("\n  sudo rm ");
            message.push_str(&shell_quote_path(path));
        }
    }
    anyhow::bail!("{message}");
}

fn render_preview(preview: Preview<'_>) -> Result<()> {
    let mut stderr = std::io::stderr().lock();
    writeln!(stderr, "RimZ uninstall preview")?;
    writeln!(stderr, "Storage:")?;
    for root in &preview.disk_usage.roots {
        let action = match root.kind {
            StorageKind::Runtime => "remove".to_owned(),
            StorageKind::Home => format!(
                "clean; state {}, config {}; keep definitions and provider accounts",
                if preview.remove_state {
                    "removed"
                } else {
                    "kept (--state)"
                },
                if preview.remove_config {
                    "removed"
                } else {
                    "kept (--config)"
                },
            ),
        };
        let present = if root.present { "" } else { " (absent)" };
        writeln!(
            stderr,
            "  {:<7} {:>8}  {}  {}{}",
            root.kind.label(),
            fmt_bytes(root.bytes),
            action,
            root.path.display(),
            present
        )?;
    }
    if preview.live_rooms.is_empty() {
        writeln!(stderr, "Rooms: none running")?;
    } else {
        writeln!(stderr, "Rooms:")?;
        for room in preview.live_rooms {
            writeln!(stderr, "  {} {}", room.mux, room.session_name)?;
        }
    }
    if preview.hook_agents.is_empty() {
        writeln!(stderr, "Hooks: none installed")?;
    } else {
        writeln!(stderr, "Hooks: {}", preview.hook_agents.join(", "))?;
    }
    if preview.skill_links.is_empty() {
        writeln!(stderr, "Skill links: none")?;
    } else {
        writeln!(stderr, "Skill links:")?;
        for plan in preview.skill_links {
            writeln!(stderr, "  {}", plan.root().display())?;
        }
    }
    match preview.loop_timer {
        TimerStatus::Installed {
            backend, active, ..
        } => {
            let state = if *active { "active" } else { "inactive" };
            writeln!(stderr, "Timer: installed ({}; {state})", backend.label())?;
        }
        TimerStatus::NotInstalled => writeln!(stderr, "Timer: none installed")?,
    }
    if preview.keep_binary {
        writeln!(stderr, "Binaries: kept (--keep-binary)")?;
    } else if preview.binaries.is_empty() {
        writeln!(stderr, "Binaries: none")?;
    } else {
        writeln!(stderr, "Binaries:")?;
        for path in preview.binaries {
            writeln!(stderr, "  {}", path.display())?;
        }
    }
    if preview.project_dirs.is_empty() {
        writeln!(stderr, "Project .rimz dirs left in place: none found")?;
    } else {
        writeln!(stderr, "Project .rimz dirs left in place:")?;
        for path in preview.project_dirs {
            writeln!(stderr, "  {}", path.display())?;
        }
    }
    Ok(())
}

fn confirm_uninstall(args: &UninstallArgs) -> Result<bool> {
    if args.yes {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        anyhow::bail!(
            "`rimz uninstall` removes hooks, rooms, runtime/cache/data, and the binary; pass --yes to confirm without a terminal"
        );
    }
    if !super::confirm("Remove RimZ from this machine?")? {
        writeln!(
            std::io::stderr().lock(),
            "Uninstall aborted; nothing changed."
        )?;
        return Ok(false);
    }
    Ok(true)
}

fn ensure_not_in_rimz_room(workspaces: &[KnownWorkspace]) -> Result<()> {
    let sessions = workspaces
        .iter()
        .map(|workspace| workspace.session_name.as_str())
        .collect::<HashSet<_>>();
    if sessions.is_empty() {
        return Ok(());
    }
    if env::var("ZELLIJ_SESSION_NAME")
        .ok()
        .filter(|session| sessions.contains(session.as_str()))
        .is_some()
    {
        anyhow::bail!("detach and rerun from outside the RimZ room");
    }
    if env::var_os("TMUX").is_some()
        && current_tmux_session()
            .as_deref()
            .is_some_and(|session| sessions.contains(session))
    {
        anyhow::bail!("detach and rerun from outside the RimZ room");
    }
    Ok(())
}

/// Which tmux session this process is running inside, if any.
///
/// Deliberately ambient: the question is "where am I", so it resolves through
/// the inherited `$TMUX` rather than the managed endpoint. A caller inside a
/// managed pane inherits the managed socket and answers with its RimZ session;
/// one inside an unrelated tmux answers with that session, which matches no
/// RimZ session name and correctly does not block the uninstall.
fn current_tmux_session() -> Option<String> {
    let output = Command::new("tmux")
        .args(["display-message", "-p", "#S"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let session = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!session.is_empty()).then_some(session)
}

fn live_rooms(workspaces: &[KnownWorkspace]) -> (Vec<LiveRoom>, Vec<String>) {
    let mut rooms = Vec::new();
    let mut failures = Vec::new();
    for mux in [MuxName::Zellij, MuxName::Tmux] {
        let backend = mux::backend_for(mux);
        let sessions = match backend.list_sessions() {
            Ok(sessions) => sessions.into_iter().collect::<HashSet<_>>(),
            Err(MuxErr::NotInstalled { .. }) => continue,
            Err(err) => {
                failures.push(format!("list {mux} sessions: {err}"));
                continue;
            }
        };
        for workspace in workspaces {
            if sessions.contains(&workspace.session_name) {
                rooms.push(LiveRoom {
                    mux,
                    workspace_id: workspace.workspace_id.clone(),
                    session_name: workspace.session_name.clone(),
                });
            }
        }
    }
    (rooms, failures)
}

fn teardown_rooms(
    rooms: &[LiveRoom],
    stderr: &mut impl Write,
    failures: &mut Vec<String>,
) -> Result<()> {
    if rooms.is_empty() {
        writeln!(stderr, "Rooms: none running")?;
        return Ok(());
    }
    for room in rooms {
        let runtime = match rimz::RuntimePaths::for_workspace(room.workspace_id.clone())
            .with_context(|| format!("preparing runtime paths for {}", room.session_name))
        {
            Ok(runtime) => runtime,
            Err(err) => {
                failures.push(err.to_string());
                continue;
            }
        };
        let state = match rimz::StatePaths::for_workspace(room.workspace_id.clone())
            .with_context(|| format!("preparing state paths for {}", room.session_name))
        {
            Ok(state) => state,
            Err(err) => {
                failures.push(err.to_string());
                continue;
            }
        };
        let backend = mux::backend_for(room.mux);
        let report = rimz::room::teardown::teardown_room(
            backend.as_ref(),
            &room.workspace_id,
            &room.session_name,
            &runtime,
            &state,
        );
        if !report.tmp_removed {
            failures.push(format!(
                "remove tmp and skill copies for {}",
                room.session_name
            ));
        }
        if report.session_killed {
            writeln!(
                stderr,
                "Rooms: removed {} {} ({} cache entries, {} processes)",
                room.mux,
                room.session_name,
                report.cache_removed.len(),
                report.processes_swept.len()
            )?;
        } else {
            let failure = format!("remove {} session {}", room.mux, room.session_name);
            writeln!(stderr, "Rooms: failed to {failure}")?;
            failures.push(failure);
        }
    }
    Ok(())
}

fn remove_roots(
    disk_usage: &RuntimeStorage,
    remove_state: bool,
    remove_config: bool,
    stderr: &mut impl Write,
    failures: &mut Vec<String>,
) -> Result<()> {
    for kind in [StorageKind::Runtime, StorageKind::Home] {
        if kind == StorageKind::Runtime {
            let outcomes = rimz::uninstall::remove_runtime_root();
            render_removal_outcomes(kind.label(), &outcomes, stderr, failures, None)?;
            continue;
        }
        let Some(root) = storage_root(disk_usage, kind) else {
            failures.push(format!("missing {} disk_usage root", kind.label()));
            continue;
        };
        let keep = home_kept_children(remove_state, remove_config);
        let outcomes = rimz::uninstall::remove_root_keeping(&root.path, &keep);
        render_removal_outcomes(kind.label(), &outcomes, stderr, failures, None)?;
        for name in keep {
            let path = root.path.join(name);
            if path.symlink_metadata().is_ok() {
                writeln!(stderr, "{}: kept {}", kind.label(), path.display())?;
            }
        }
    }
    Ok(())
}

fn render_removal_outcomes(
    label: &str,
    outcomes: &[RemovalOutcome],
    stderr: &mut impl Write,
    failures: &mut Vec<String>,
    mut sudo_hints: Option<&mut Vec<PathBuf>>,
) -> Result<()> {
    if outcomes.is_empty() {
        writeln!(stderr, "{label}: none")?;
        return Ok(());
    }
    for outcome in outcomes {
        match &outcome.result {
            Ok(Removed::Removed) => {
                writeln!(stderr, "{label}: removed {}", outcome.path.display())?
            }
            Ok(Removed::AlreadyAbsent) => {
                writeln!(stderr, "{label}: already absent {}", outcome.path.display())?
            }
            Err(err) => {
                writeln!(stderr, "{label}: failed {}", outcome.path.display())?;
                failures.push(format!("{label} {}: {err}", outcome.path.display()));
                if err.kind() == std::io::ErrorKind::PermissionDenied
                    && let Some(paths) = sudo_hints.as_deref_mut()
                {
                    paths.push(outcome.path.clone());
                }
            }
        }
    }
    Ok(())
}

fn home_kept_children(remove_state: bool, remove_config: bool) -> Vec<&'static str> {
    let mut keep = vec![
        "profiles",
        "agents",
        "subagents",
        "teams",
        "traits",
        "skills",
        "accounts",
        "handoffs",
    ];
    if !remove_state {
        keep.extend(["ws", "logs", "loops", "web", "builds"]);
    }
    if !remove_config {
        keep.extend([
            "config.toml",
            "theme.toml",
            "loop.toml",
            "remote.toml",
            "trust",
            "agents.d",
            "cursor-statusline.json",
        ]);
    }
    keep
}

fn storage_root(disk_usage: &RuntimeStorage, kind: StorageKind) -> Option<&StorageRoot> {
    disk_usage.roots.iter().find(|root| root.kind == kind)
}

/// A provider's own home reads as its kind; a named account as `kind@name`.
fn hook_label(key: &rimz::ids::LoginKey) -> String {
    if key.name.is_default() {
        key.kind.to_string()
    } else {
        key.to_string()
    }
}

fn cargo_bin_dir() -> Option<PathBuf> {
    paths::env_path("CARGO_HOME")
        .or_else(|| paths::env_path("HOME").map(|home| home.join(".cargo")))
        .map(|cargo_home| cargo_home.join("bin"))
}

fn system_bin_dir() -> PathBuf {
    paths::env_path("RIMZ_SYSTEM_BIN_DIR").unwrap_or_else(|| PathBuf::from("/usr/local/bin"))
}

fn project_local_dirs(workspaces: &[KnownWorkspace]) -> Vec<PathBuf> {
    let home = paths::rimz_home();
    workspaces
        .iter()
        .filter(|workspace| !paths::holds_rimz_home(&workspace.project_root, &home))
        .map(|workspace| workspace.project_root.join(".rimz"))
        .filter(|path| path.is_dir())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn shell_quote_path(path: &Path) -> String {
    let raw = path.display().to_string();
    shlex::try_quote(&raw)
        // Existing filesystem paths cannot contain NUL bytes.
        .expect("path display string is shell-quotable")
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_cleanup_preserves_flagged_categories_and_user_files() {
        for (remove_state, remove_config) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            let temp = tempfile::tempdir().unwrap();
            let home = temp.path().join("home");
            for child in [
                "ws",
                "profiles",
                "accounts",
                "cache/providers",
                "trust",
                "handoffs",
            ] {
                std::fs::create_dir_all(home.join(child)).unwrap();
                std::fs::write(home.join(child).join("user-file"), b"keep").unwrap();
            }
            std::fs::write(home.join("config.toml"), b"config").unwrap();
            let outcomes = rimz::uninstall::remove_root_keeping(
                &home,
                &home_kept_children(remove_state, remove_config),
            );
            assert!(outcomes.iter().all(|outcome| outcome.result.is_ok()));
            assert_eq!(home.join("ws/user-file").exists(), !remove_state);
            assert_eq!(home.join("trust/user-file").exists(), !remove_config);
            assert_eq!(home.join("config.toml").exists(), !remove_config);
            for child in ["profiles", "accounts", "handoffs"] {
                assert!(home.join(child).join("user-file").exists());
            }
            assert!(!home.join("cache").exists());
        }
    }
}
