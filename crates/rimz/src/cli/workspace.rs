//! `rimz workspace` — identity and maintenance: resolve, migrate, rotate-events.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use super::GlobalFlags;
use crate::cli::render;
use rimz::ids::WorkspaceId;
use rimz::store::event_log::RotationOutcome;
use rimz::workspace::WorkspaceResolver;
use rimz::{RuntimePaths, StatePaths, Store};

const MIB: u64 = 1024 * 1024;
pub(crate) const DEFAULT_EVENT_LOG_ROTATE_BYTES: u64 = 64 * MIB;
const DEFAULT_EVENT_LOG_ARCHIVE_RETENTION: &str = rimz::store::event_log::DEFAULT_RETENTION_ARG;

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
    /// Move a store after a project root moved on disk.
    Migrate {
        old_root: PathBuf,
        new_root: PathBuf,
    },
    /// Rotate the active event log and prune older archives.
    RotateEvents(RotateEventsArgs),
}

#[derive(Debug, Args)]
pub struct RotateEventsArgs {
    /// Rotate only if the active log is at least this big. Accepts `64MiB`, `512KB`.
    #[arg(long, default_value_t = DEFAULT_EVENT_LOG_ROTATE_BYTES, value_parser = parse_byte_size)]
    max_bytes: u64,
    /// Remove archives older than this duration.
    #[arg(long, default_value = DEFAULT_EVENT_LOG_ARCHIVE_RETENTION, value_parser = parse_retention_duration)]
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
            "workspace store {} not found at {}",
            old_workspace_id,
            old_paths.root.display()
        );
    }

    if old_workspace_id != new_workspace.workspace_id {
        if new_paths.root.exists() {
            bail!(
                "destination workspace store {} already exists at {}",
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
                "moving store {} to {}",
                old_paths.root.display(),
                new_paths.root.display()
            )
        })?;
    }

    let runtime = RuntimePaths::for_workspace(new_workspace.workspace_id.clone())
        .context("preparing runtime paths")?;
    let store = Store::open(new_paths, runtime).context("opening migrated store")?;
    let outcome = store
        .rewrite_workspace_identity(&new_workspace)
        .context("rewriting workspace identity")?;

    use std::io::Write;
    let mut out = render::out();
    writeln!(
        out,
        "migrated {} -> {}",
        old_workspace_id, outcome.workspace_id
    )?;
    let mut kv = render::KeyVals::new().indent(2);
    kv.push(
        "old root",
        render::cell(old_project_root.display().to_string()),
    );
    kv.push(
        "new root",
        render::cell(new_workspace.project_root.display().to_string()).fg(render::palette::ACCENT),
    );
    kv.push(
        "messages",
        render::cell(outcome.messages_rewritten.to_string()),
    );
    kv.push("events", render::cell(outcome.events_rewritten.to_string()));
    kv.render(&mut out)?;
    Ok(())
}

fn rotate_events(args: RotateEventsArgs, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve(".", globals.root.clone())
        .context("resolving current workspace")?;
    let paths = StatePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing store paths")?;
    let runtime = RuntimePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing runtime paths")?;
    let store = Store::open(paths, runtime).context("opening store")?;
    let outcome = store
        .rotate_event_log(args.max_bytes, args.archive_older_than)
        .context("rotating event log")?;

    use std::io::Write;
    let mut out = render::out();
    let mut kv = render::KeyVals::new().indent(2);
    match &outcome.rotation {
        RotationOutcome::Skipped { current_bytes } => {
            writeln!(out, "event-log rotation skipped")?;
            kv.push(
                "workspace",
                render::cell(workspace.workspace_id.to_string()).fg(render::palette::ACCENT),
            );
            kv.push("current bytes", render::cell(current_bytes.to_string()));
            kv.push("threshold", render::cell(args.max_bytes.to_string()));
        }
        RotationOutcome::Rotated {
            archive_path,
            bytes_rotated,
        } => {
            writeln!(out, "event-log rotated")?;
            kv.push(
                "workspace",
                render::cell(workspace.workspace_id.to_string()).fg(render::palette::ACCENT),
            );
            kv.push("bytes rotated", render::cell(bytes_rotated.to_string()));
            kv.push("archive", render::cell(archive_path.display().to_string()));
            kv.push(
                "carryover",
                render::cell(format!("{} agent(s)", outcome.carryover_agents)),
            );
        }
    }
    if args.archive_older_than.is_some() {
        kv.push(
            "pruned",
            render::cell(format!("{} archive(s)", outcome.pruned.files_removed)),
        );
        kv.push(
            "bytes pruned",
            render::cell(outcome.pruned.bytes_removed.to_string()),
        );
    }
    kv.render(&mut out)?;
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
    fn default_rotation_threshold_matches_the_human_size() {
        assert_eq!(
            DEFAULT_EVENT_LOG_ROTATE_BYTES,
            parse_byte_size("64MiB").unwrap()
        );
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

    #[test]
    fn default_archive_retention_is_fourteen_days() {
        assert_eq!(
            parse_retention_duration(DEFAULT_EVENT_LOG_ARCHIVE_RETENTION).unwrap(),
            Duration::from_secs(14 * 86_400)
        );
    }
}
