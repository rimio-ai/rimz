use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use super::GlobalFlags;
use rimz::ids::WorkspaceId;
use rimz::ledger::event_log::RotationOutcome;
use rimz::ledger::gc;
use rimz::workspace::WorkspaceResolver;
use rimz::{Ledger, RuntimePaths, StatePaths};

#[derive(Debug, Args)]
pub struct WorkspaceArgs {
    #[command(subcommand)]
    command: WorkspaceSubcmd,
}

#[derive(Debug, Subcommand)]
enum WorkspaceSubcmd {
    /// Resolve a path to a workspace and print the result as JSON.
    Resolve {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Move a ledger after a project root moved on disk.
    Migrate {
        old_root: PathBuf,
        new_root: PathBuf,
    },
    /// Remove known workspace ledgers whose project roots no longer exist.
    Prune,
    /// Rotate the active event log and prune older archives.
    RotateEvents(RotateEventsArgs),
}

#[derive(Debug, Args)]
pub struct RotateEventsArgs {
    /// Rotate only if the active log is at least this big (`64MiB`, `512KB`).
    #[arg(long, default_value = "64MiB", value_parser = parse_byte_size)]
    max_bytes: u64,
    /// Remove archives older than this duration. Omit to keep all archives.
    #[arg(long, value_parser = parse_retention_duration)]
    archive_older_than: Option<Duration>,
}

pub fn run(args: WorkspaceArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        WorkspaceSubcmd::Resolve { path } => {
            let workspace = WorkspaceResolver::resolve(&path, globals.root.clone())?;
            let rendered = serde_json::to_string_pretty(&workspace)?;
            #[expect(clippy::print_stdout, reason = "json emitter")]
            {
                println!("{rendered}");
            }
            Ok(())
        }
        WorkspaceSubcmd::Migrate { old_root, new_root } => migrate(old_root, new_root),
        WorkspaceSubcmd::Prune => prune(),
        WorkspaceSubcmd::RotateEvents(args) => rotate_events(args, globals),
    }
}

fn migrate(old_root: PathBuf, new_root: PathBuf) -> Result<()> {
    let old_project_root = resolve_old_project_root(&old_root)
        .with_context(|| format!("resolving old root {}", old_root.display()))?;
    let new_workspace = WorkspaceResolver::resolve(&new_root, None)
        .with_context(|| format!("resolving new root {}", new_root.display()))?;
    if !new_workspace.project_root.exists() {
        bail!(
            "new workspace root does not exist: {}",
            new_workspace.project_root.display()
        );
    }

    let old_workspace_id = WorkspaceId::from_project_root(&old_project_root);
    let old_paths = StatePaths::for_workspace(old_workspace_id.clone())
        .context("preparing old workspace paths")?;
    let new_paths = StatePaths::for_workspace(new_workspace.workspace_id.clone())
        .context("preparing new workspace paths")?;

    if !old_paths.root.exists() {
        bail!(
            "workspace ledger {} not found at {}",
            old_workspace_id,
            old_paths.root.display()
        );
    }

    if old_workspace_id != new_workspace.workspace_id {
        if new_paths.root.exists() {
            bail!(
                "destination workspace ledger {} already exists at {}",
                new_workspace.workspace_id,
                new_paths.root.display()
            );
        }
        if let Some(parent) = new_paths.root.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::rename(&old_paths.root, &new_paths.root).with_context(|| {
            format!(
                "moving ledger {} to {}",
                old_paths.root.display(),
                new_paths.root.display()
            )
        })?;
    }

    let runtime = RuntimePaths::for_workspace(new_workspace.workspace_id.clone())
        .context("preparing runtime paths")?;
    let ledger = Ledger::open(new_paths, runtime).context("opening migrated ledger")?;
    let outcome = ledger
        .rewrite_workspace_identity(&new_workspace)
        .context("rewriting workspace identity")?;

    #[expect(clippy::print_stdout, reason = "user-facing maintenance report")]
    {
        println!("migrated {} -> {}", old_workspace_id, outcome.workspace_id);
        println!("  old root      : {}", old_project_root.display());
        println!("  new root      : {}", new_workspace.project_root.display());
        println!("  feed items    : {}", outcome.feed_items_rewritten);
        println!("  events        : {}", outcome.events_rewritten);
    }
    Ok(())
}

fn prune() -> Result<()> {
    let report = gc::prune_dead_workspaces().context("pruning dead workspaces")?;
    #[expect(clippy::print_stdout, reason = "user-facing maintenance report")]
    {
        println!("pruned {} workspace(s)", report.removed.len());
        println!("  kept          : {}", report.kept);
        println!("  skipped       : {}", report.retained_unreadable.len());
        print_prune_removals(&report);
        for (workspace_id, err) in &report.retained_unreadable {
            println!("  skipped       : {workspace_id} ({err})");
        }
    }
    Ok(())
}

/// Render the per-workspace removal lines shared by `workspace prune` and `gc`.
#[expect(clippy::print_stdout, reason = "user-facing maintenance report")]
pub(super) fn print_prune_removals(report: &gc::WorkspacePruneReport) {
    for removed in &report.removed {
        match &removed.project_root {
            Some(root) => println!(
                "  removed       : {} {}",
                removed.workspace_id,
                root.display()
            ),
            None => println!(
                "  removed       : {} (abandoned scaffold)",
                removed.workspace_id
            ),
        }
    }
}

fn rotate_events(args: RotateEventsArgs, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve(".", globals.root.clone())
        .context("resolving current workspace")?;
    let paths = StatePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing ledger paths")?;
    let runtime = RuntimePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing runtime paths")?;
    let ledger = Ledger::open(paths, runtime).context("opening ledger")?;
    let outcome = ledger
        .rotate_event_log(args.max_bytes, args.archive_older_than)
        .context("rotating event log")?;

    #[expect(clippy::print_stdout, reason = "user-facing maintenance report")]
    {
        match &outcome.rotation {
            RotationOutcome::Skipped { current_bytes } => {
                println!("event-log rotation skipped");
                println!("  workspace     : {}", workspace.workspace_id);
                println!("  current bytes : {current_bytes}");
                println!("  threshold     : {}", args.max_bytes);
            }
            RotationOutcome::Rotated {
                archive_path,
                bytes_rotated,
            } => {
                println!("event-log rotated");
                println!("  workspace     : {}", workspace.workspace_id);
                println!("  bytes rotated : {bytes_rotated}");
                println!("  archive       : {}", archive_path.display());
                println!("  carryover     : {} agent(s)", outcome.carryover_agents);
            }
        }
        if args.archive_older_than.is_some() {
            println!(
                "  pruned        : {} archive(s)",
                outcome.pruned.files_removed
            );
            println!("  bytes pruned  : {}", outcome.pruned.bytes_removed);
        }
    }
    Ok(())
}

fn resolve_old_project_root(path: &std::path::Path) -> Result<PathBuf> {
    if path.exists() {
        let workspace = WorkspaceResolver::resolve(path, None)?;
        return Ok(workspace.project_root);
    }
    absolutize(path)
}

fn absolutize(path: &std::path::Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn parse_byte_size(raw: &str) -> std::result::Result<u64, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("size is empty".to_owned());
    }
    let (digits, suffix) = split_suffix(trimmed);
    let n: u64 = digits
        .parse()
        .map_err(|err| format!("size `{raw}` is not an integer: {err}"))?;
    let factor: u64 = match suffix {
        "" | "B" => 1,
        "K" | "KB" => 1_000,
        "KiB" => 1_024,
        "M" | "MB" => 1_000_000,
        "MiB" => 1_024 * 1_024,
        "G" | "GB" => 1_000_000_000,
        "GiB" => 1_024 * 1_024 * 1_024,
        other => {
            return Err(format!(
                "unknown size unit `{other}`; use B/KB/KiB/MB/MiB/GB/GiB"
            ));
        }
    };
    n.checked_mul(factor)
        .ok_or_else(|| format!("size `{raw}` overflows u64"))
}

fn parse_retention_duration(raw: &str) -> std::result::Result<Duration, String> {
    super::parse::parse_duration_units(raw, &[("s", 1), ("m", 60), ("h", 3600), ("d", 86_400)])
}

fn split_suffix(raw: &str) -> (&str, &str) {
    let cut = raw
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit())
        .map(|(i, _)| i)
        .unwrap_or(raw.len());
    (&raw[..cut], &raw[cut..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_size_parses_units() {
        assert_eq!(parse_byte_size("0").unwrap(), 0);
        assert_eq!(parse_byte_size("512").unwrap(), 512);
        assert_eq!(parse_byte_size("1KB").unwrap(), 1_000);
        assert_eq!(parse_byte_size("1KiB").unwrap(), 1_024);
        assert_eq!(parse_byte_size("64MiB").unwrap(), 64 * 1024 * 1024);
        assert!(parse_byte_size("").is_err());
        assert!(parse_byte_size("3PB").is_err());
    }

    #[test]
    fn retention_duration_accepts_days() {
        assert_eq!(
            parse_retention_duration("30d").unwrap(),
            Duration::from_secs(30 * 86_400)
        );
        assert_eq!(
            parse_retention_duration("7d").unwrap(),
            Duration::from_secs(7 * 86_400)
        );
        assert_eq!(
            parse_retention_duration("12h").unwrap(),
            Duration::from_secs(12 * 3_600)
        );
        assert!(parse_retention_duration("30").is_err());
        assert!(parse_retention_duration("30y").is_err());
    }
}
