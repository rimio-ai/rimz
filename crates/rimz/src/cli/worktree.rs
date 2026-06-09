//! `rimz worktree` — Rimz-owned git worktree lifecycle.

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use super::GlobalFlags;
use rimz::config::WorktreeBase;
use rimz::workspace::{RootClass, WorkspaceResolver};

#[derive(Debug, Args)]
pub struct WorktreeArgs {
    #[command(subcommand)]
    command: WorktreeSubcmd,
}

#[derive(Debug, Subcommand)]
enum WorktreeSubcmd {
    /// Create a Rimz-owned worktree.
    New {
        #[arg(value_name = "NAME")]
        name: Option<String>,
        /// Base ref: `head`, `fresh`, or any git ref.
        #[arg(long, value_parser = parse_base)]
        base: Option<WorktreeBase>,
        /// Branch to create instead of `<name>`.
        #[arg(long)]
        branch: Option<String>,
    },
    /// List Rimz-owned worktrees.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Remove a Rimz-owned worktree.
    Remove {
        name: String,
        /// Remove even when the worktree is dirty or ahead of its base.
        #[arg(long)]
        force: bool,
    },
}

pub fn run(args: WorktreeArgs, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve(".", globals.root.clone())
        .context("resolving current workspace")?;
    if workspace.root_class != RootClass::Repo {
        bail!("rimz worktree requires a git repository; cd into a repo checkout");
    }
    let config = super::machine_config().worktree;
    match args.command {
        WorktreeSubcmd::New { name, base, branch } => {
            let created = rimz::worktree::create(
                &workspace.project_root,
                &config,
                name.as_deref(),
                base,
                branch.as_deref(),
                false,
            )?;
            #[expect(clippy::print_stdout, reason = "user-facing lifecycle report")]
            {
                println!("created {}", created.name);
                println!("  path   : {}", created.path.display());
                println!("  branch : {}", created.branch);
                println!("  base   : {}", created.base_ref);
            }
            Ok(())
        }
        WorktreeSubcmd::List { json } => {
            let entries = rimz::worktree::list(&workspace.project_root)?;
            if json {
                let rendered = serde_json::to_string_pretty(&entries)?;
                #[expect(clippy::print_stdout, reason = "json emitter")]
                {
                    println!("{rendered}");
                }
            } else {
                for entry in entries {
                    let commits_ahead = entry
                        .commits_ahead
                        .map_or_else(|| "?".to_owned(), |count| count.to_string());
                    #[expect(clippy::print_stdout, reason = "human listing")]
                    {
                        println!(
                            "{}\t{}\t{}\t{} ahead{}",
                            entry.name,
                            entry.path.display(),
                            entry.branch.as_deref().unwrap_or("-"),
                            commits_ahead,
                            if entry.dirty { " dirty" } else { "" }
                        );
                    }
                }
            }
            Ok(())
        }
        WorktreeSubcmd::Remove { name, force } => {
            rimz::worktree::remove(&workspace.project_root, &config, &name, force)?;
            #[expect(clippy::print_stdout, reason = "user-facing lifecycle report")]
            {
                println!("removed {name}");
            }
            Ok(())
        }
    }
}

fn parse_base(raw: &str) -> std::result::Result<WorktreeBase, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("base ref cannot be empty".to_owned());
    }
    Ok(match trimmed {
        "head" => WorktreeBase::Head,
        "fresh" => WorktreeBase::Fresh,
        other => WorktreeBase::Explicit(other.to_owned()),
    })
}
