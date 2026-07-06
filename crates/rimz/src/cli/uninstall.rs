//! `rimz uninstall` — remove Rimz's machine-wide footprint.

use std::collections::{BTreeSet, HashSet};
use std::env;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use clap::Args;

use super::GlobalFlags;
use super::render::fmt_bytes;
use rimz::ids::{MuxName, WorkspaceId};
use rimz::ledger::paths;
use rimz::mux::{self, MuxErr};
use rimz::storage::{RuntimeStorage, StorageKind, StorageRoot};
use rimz::uninstall::{RemovalOutcome, Removed};
use rimz::workspace::{KnownWorkspace, known_workspaces};

#[derive(Debug, Args)]
pub struct UninstallArgs {
    /// Also delete durable ledgers, spend history, and shared state.
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
    storage: &'a RuntimeStorage,
    remove_state: bool,
    remove_config: bool,
    live_rooms: &'a [LiveRoom],
    hook_agents: &'a [&'static str],
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

    let storage = rimz::storage::measure();
    let (live_rooms, session_failures) = live_rooms(&workspaces);
    failures.extend(session_failures);
    let hook_agents = managed_hook_agents();
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
        storage: &storage,
        remove_state,
        remove_config,
        live_rooms: &live_rooms,
        hook_agents: &hook_agents,
        keep_binary: args.keep_binary,
        binaries: &binaries,
        project_dirs: &project_dirs,
    })?;
    if !confirm_uninstall(&args)? {
        return Ok(());
    }

    let mut stderr = std::io::stderr().lock();
    writeln!(stderr, "\nUninstalling Rimz...")?;

    teardown_rooms(&live_rooms, &mut stderr, &mut failures)?;

    match super::hooks::uninstall_managed_hooks() {
        Ok(reports) if reports.is_empty() => writeln!(stderr, "Hooks: none installed")?,
        Ok(reports) => {
            let agents = reports
                .iter()
                .map(|report| report.agent)
                .collect::<Vec<_>>()
                .join(", ");
            writeln!(stderr, "Hooks: removed {agents}")?;
        }
        Err(err) => failures.push(format!("remove managed hooks: {err}")),
    }

    remove_roots(
        &storage,
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
    writeln!(stderr, "Rimz uninstall preview")?;
    writeln!(stderr, "Storage:")?;
    for root in &preview.storage.roots {
        let action = if root_removed(root.kind, preview.remove_state, preview.remove_config) {
            "remove".to_owned()
        } else if root.kind == StorageKind::State {
            "kept (pass --state)".to_owned()
        } else if root.kind == StorageKind::Config {
            "kept (pass --config)".to_owned()
        } else {
            "kept".to_owned()
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
    if !super::confirm("Remove Rimz from this machine?")? {
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
        anyhow::bail!("detach and rerun from outside the Rimz room");
    }
    if env::var_os("TMUX").is_some()
        && current_tmux_session()
            .as_deref()
            .is_some_and(|session| sessions.contains(session))
    {
        anyhow::bail!("detach and rerun from outside the Rimz room");
    }
    Ok(())
}

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
        let backend = mux::backend_for(room.mux);
        let report = rimz::mux::recovery::teardown_room(
            backend.as_ref(),
            &room.workspace_id,
            &room.session_name,
            &runtime,
        );
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
    storage: &RuntimeStorage,
    remove_state: bool,
    remove_config: bool,
    stderr: &mut impl Write,
    failures: &mut Vec<String>,
) -> Result<()> {
    for kind in [
        StorageKind::Runtime,
        StorageKind::Cache,
        StorageKind::Data,
        StorageKind::State,
        StorageKind::Config,
    ] {
        if !root_removed(kind, remove_state, remove_config) {
            continue;
        }
        if kind == StorageKind::Runtime {
            let outcomes = rimz::uninstall::remove_runtime_root();
            render_removal_outcomes(kind.label(), &outcomes, stderr, failures, None)?;
            continue;
        }
        let Some(root) = storage_root(storage, kind) else {
            failures.push(format!("missing {} storage root", kind.label()));
            continue;
        };
        let outcomes = [rimz::uninstall::remove_root(&root.path)];
        render_removal_outcomes(kind.label(), &outcomes, stderr, failures, None)?;
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

fn root_removed(kind: StorageKind, remove_state: bool, remove_config: bool) -> bool {
    match kind {
        StorageKind::Runtime | StorageKind::Cache | StorageKind::Data => true,
        StorageKind::State => remove_state,
        StorageKind::Config => remove_config,
    }
}

fn storage_root(storage: &RuntimeStorage, kind: StorageKind) -> Option<&StorageRoot> {
    storage.roots.iter().find(|root| root.kind == kind)
}

fn managed_hook_agents() -> Vec<&'static str> {
    rimz::agents::ADAPTERS
        .iter()
        .copied()
        .filter(|adapter| adapter.managed_hook_artifacts_present())
        .map(|adapter| adapter.descriptor().kind)
        .collect()
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
    workspaces
        .iter()
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
