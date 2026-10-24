//! `rimz worktree` — create, enter, land, sweep, and present managed checkouts.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde::Serialize;

use super::{GlobalFlags, require_existing_store};
use crate::cli::render;
use rimz::config::{WorktreeBase, WorktreeConfig};
use rimz::forge::PrTarget;
use rimz::workspace::{ResolvedWorkspace, RootClass, WorkspaceResolver};
use rimz::worktree::{Holders, InspectedWorktree, KeepReason};

const CLEANUP_SIGNAL_ROSTER_GRACE: Duration = Duration::from_millis(300);

#[derive(Serialize)]
struct WorktreeListJson<'a> {
    name: &'a str,
    path: &'a Path,
    branch: Option<&'a str>,
    base_ref: &'a str,
    base_branch: Option<&'a str>,
    dirty: bool,
    landed: Option<bool>,
    reasons: Vec<KeepReason>,
    holders: &'a Holders,
}

#[derive(Debug, Args)]
pub struct WorktreeArgs {
    #[command(subcommand)]
    command: Option<WorktreeSubcmd>,
}

#[derive(Debug, Subcommand)]
enum WorktreeSubcmd {
    /// Create a RimZ-owned worktree.
    New {
        /// Name without `#`; `/` becomes `-` in the directory but stays in the branch. Omit for a generated name (or the PR's tree with --from-pr).
        #[arg(value_name = "NAME")]
        name: Option<String>,
        /// Base: `head` (main checkout's HEAD, default unless [agents.worktree] base is set), `fresh` (origin/HEAD as last fetched), or any ref.
        #[arg(long, value_parser = parse_base)]
        base: Option<WorktreeBase>,
        /// Create the worktree from a pull request number or URL.
        #[arg(long = "from-pr", value_name = "PR", value_parser = rimz::forge::parse, conflicts_with = "base")]
        from_pr: Option<rimz::forge::PrTarget>,
        /// Branch to create instead of `<name>`.
        #[arg(long)]
        branch: Option<String>,
        /// Print the name, path, branch, base provenance, and reuse status as JSON.
        #[arg(long)]
        json: bool,
    },
    /// List RimZ-owned worktrees.
    #[command(visible_alias = "ls")]
    List {
        /// Print worktree state, keep reasons, and holders as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Open a shell in a RimZ-owned worktree.
    Cd {
        /// Name without `#`; `/` becomes `-` in the directory but stays in the branch.
        #[arg(add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::worktrees
        ))]
        name: String,
    },
    /// Print the absolute path of a RimZ-owned worktree.
    Path {
        /// Name without `#`; `/` becomes `-` in the directory but stays in the branch.
        #[arg(add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::worktrees
        ))]
        name: String,
    },
    /// Rebase a clean worktree onto its base branch and fast-forward that branch to it; the tree stays.
    Merge {
        /// Name without `#`; `/` becomes `-` in the directory but stays in the branch.
        #[arg(add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::worktrees
        ))]
        name: String,
        /// Land on the target branch without asking for confirmation.
        #[arg(long)]
        yes: bool,
    },
    /// Remove a RimZ-owned worktree.
    #[command(visible_alias = "rm")]
    Remove {
        /// Name without `#`; `/` becomes `-` in the directory but stays in the branch.
        #[arg(add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::worktrees
        ))]
        name: String,
        /// Remove even when the worktree is in use, dirty, unlanded, or unknown.
        #[arg(long)]
        force: bool,
    },
    /// Remove every clean, landed, unused RimZ-owned worktree.
    Sweep {
        /// Report what would be removed without removing anything.
        #[arg(long)]
        dry_run: bool,
        /// Print the sweep report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Hidden helper used by long-lived agent wrappers.
    #[command(hide = true)]
    Cleanup(CleanupArgs),
}

#[derive(Debug, Args)]
struct CleanupArgs {
    #[arg(value_name = "PATH")]
    path: PathBuf,
    /// Keep dirty worktrees without prompting.
    #[arg(long)]
    non_interactive: bool,
}

pub fn run(args: WorktreeArgs, globals: &GlobalFlags) -> Result<()> {
    let command = match args.command.unwrap_or(WorktreeSubcmd::List { json: false }) {
        WorktreeSubcmd::Cleanup(cleanup) => {
            return cleanup_worktree(&cleanup.path, globals, !cleanup.non_interactive);
        }
        command => command,
    };

    let workspace = WorkspaceResolver::resolve(".", globals.root.clone())
        .context("resolving current workspace")?;
    if workspace.root_class != RootClass::Repo {
        bail!("rimz worktree requires a git repository; cd into a repo checkout");
    }
    let machine_config = super::machine_config();
    if matches!(command, WorktreeSubcmd::New { .. }) {
        super::require_worktree_config(&machine_config)?;
    }
    super::report_unknown_config_keys(&machine_config)?;
    let config = machine_config.agents.worktree.clone();
    match command {
        WorktreeSubcmd::New {
            name,
            base,
            from_pr,
            branch,
            json,
        } => new_worktree(&workspace, &config, name, base, from_pr, branch, json),
        WorktreeSubcmd::List { json } => list_worktrees(&workspace, &config, globals, json),
        WorktreeSubcmd::Sweep { dry_run, json } => {
            sweep_worktrees(&workspace, &config, globals, dry_run, json)
        }
        WorktreeSubcmd::Cd { name } => cd_worktree(&workspace, &config, &name),
        WorktreeSubcmd::Path { name } => {
            let managed = resolve_worktree(&workspace, &config, &name)?;
            writeln!(
                render::out(),
                "{}",
                rimz::utils::path::normalize_path_lexical(&managed.path).display()
            )?;
            Ok(())
        }
        WorktreeSubcmd::Merge { name, yes } => {
            merge_worktree(&workspace, &config, globals, &name, yes)
        }
        WorktreeSubcmd::Remove { name, force } => {
            remove_worktree(&workspace, &config, globals, name, force)
        }
        WorktreeSubcmd::Cleanup(_) => unreachable!("cleanup returned before workspace resolution"),
    }
}

fn new_worktree(
    workspace: &ResolvedWorkspace,
    config: &WorktreeConfig,
    name: Option<String>,
    base: Option<WorktreeBase>,
    from_pr: Option<PrTarget>,
    branch: Option<String>,
    json: bool,
) -> Result<()> {
    let store = require_existing_store(workspace)?;
    let requested_name = name
        .as_deref()
        .map(rimz::worktree::parse_requested_name)
        .transpose()?;
    if let Some(name) = requested_name
        .as_ref()
        .map(|requested| requested.name.as_str())
    {
        // An unreadable snapshot holds no lane, so the name stays available.
        if let Ok(snapshot) = store.snapshot_cached() {
            rimz::channel::admit_worktree_name(&snapshot.agents, name)?;
        }
    }
    let created = if let Some(pr) = from_pr.as_ref() {
        let create = |choice| {
            rimz::worktree::create_from_pr(
                &workspace.project_root,
                config,
                pr,
                name.as_deref(),
                branch.as_deref(),
                false,
                choice,
            )
        };
        match create(None) {
            Ok(created) => created,
            Err(err @ rimz::worktree::WorktreeErr::PrBranchDiverged { .. }) => {
                let Some(choice) = super::resolve_pr_branch_choice(&err)? else {
                    writeln!(render::err(), "nothing changed.")?;
                    return Ok(());
                };
                create(Some(choice))?
            }
            Err(err) => return Err(err.into()),
        }
    } else {
        rimz::worktree::create(
            &workspace.project_root,
            config,
            name.as_deref(),
            base,
            branch.as_deref(),
            false,
            rimz::worktree::LaunchBase::Configured,
        )?
    };
    if !created.reused {
        let archival = store.archive_channel_messages(
            &created.marker.name,
            "channel recreated",
            &workspace.session_name,
        );
        rimz::launcher::checkout::emit_worktree_created(&store, workspace, &created.marker);
        archival.context("archiving messages for recreated worktree channel")?;
    }
    if let Some(stale) = &created.stale_base {
        writeln!(
            render::err(),
            "warning: {stale}; fast-forward {} first, or pass --base {}",
            stale.branch,
            stale.upstream
        )?;
    }
    if json {
        let marker = &created.marker;
        #[derive(Serialize)]
        struct CreatedJson<'a> {
            name: &'a str,
            path: PathBuf,
            branch: &'a str,
            base_branch: Option<&'a str>,
            base_ref: &'a str,
            reused: bool,
        }
        render::json(&CreatedJson {
            name: &marker.name,
            path: rimz::utils::path::normalize_path_lexical(&marker.worktree_path),
            branch: &marker.branch,
            base_branch: marker.base_branch.as_deref(),
            base_ref: &marker.base_ref,
            reused: created.reused,
        })?;
    } else {
        report_created(&created, config.hooks.created.is_some())?;
    }
    Ok(())
}

fn report_created(created: &rimz::worktree::CreatedWorktree, created_hook_ran: bool) -> Result<()> {
    let marker = &created.marker;
    let mut out = render::out();
    writeln!(
        out,
        "{} {}",
        if created.reused { "reused" } else { "created" },
        marker.name
    )?;
    writeln!(
        out,
        "  path   : {}",
        render::home_relative_path(&marker.worktree_path)
    )?;
    let base = marker.base_ref.get(..7).unwrap_or(&marker.base_ref);
    if let Some(base_branch) = marker.base_branch.as_deref() {
        writeln!(
            out,
            "  branch : {}, from {base_branch} at {base}",
            marker.branch
        )?;
    } else {
        writeln!(out, "  branch : {}, from {base}", marker.branch)?;
    }
    if created.reused {
        writeln!(
            out,
            "  next   : rimz agents <spec> -w {}    or    rimz worktree cd {}",
            marker.name, marker.name
        )?;
        return Ok(());
    }
    if let Some(destination) = created.push_destination.as_ref() {
        let remote = &destination.remote;
        let merge_ref = &destination.merge_ref;
        writeln!(out, "  pushes : {remote} {merge_ref}")?;
        if merge_ref.strip_prefix("refs/heads/") != Some(marker.branch.as_str()) {
            let head = merge_ref.strip_prefix("refs/heads/").unwrap_or(merge_ref);
            writeln!(out, "  push   : git push {remote} HEAD:{head}")?;
        }
    }
    if let Some(reason) = created.review_only_reason.as_deref() {
        writeln!(
            out,
            "  review : review-only checkout ({reason}); pushes are not configured — install gh/tea for a pushable checkout"
        )?;
    }
    if created.included > 0 {
        writeln!(
            out,
            "  seeded : {} file(s) from .worktreeinclude",
            created.included
        )?;
    }
    if created.linked > 0 {
        writeln!(
            out,
            "  linked : {} dir(s) from .worktreelink",
            created.linked
        )?;
    }
    if created_hook_ran {
        writeln!(out, "  hook   : worktree.created ran")?;
    }
    writeln!(
        out,
        "  next   : rimz agents <spec> -w {}    or    rimz worktree cd {}",
        marker.name, marker.name
    )?;
    Ok(())
}

fn list_worktrees(
    workspace: &ResolvedWorkspace,
    config: &WorktreeConfig,
    globals: &GlobalFlags,
    json: bool,
) -> Result<()> {
    let guard = super::worktree_protection::for_listing(workspace, globals);
    let entries = rimz::worktree::inspect_owned(&workspace.project_root, &guard.protections)?;
    if json {
        render_worktree_json(&entries)?;
    } else {
        let dir = rimz::worktree::worktree_parent(&workspace.project_root, config)?;
        render_worktree_table(&dir, &entries)?;
    }
    Ok(())
}

fn render_worktree_json(entries: &[InspectedWorktree]) -> Result<()> {
    let rows = entries.iter().map(worktree_json_row).collect::<Vec<_>>();
    render::json_pretty(&rows)
}

fn worktree_json_row(entry: &InspectedWorktree) -> WorktreeListJson<'_> {
    WorktreeListJson {
        name: &entry.managed.marker.name,
        path: &entry.managed.path,
        branch: entry.managed.branch.as_deref(),
        base_ref: &entry.managed.marker.base_ref,
        base_branch: entry.managed.marker.base_branch.as_deref(),
        dirty: entry.state.status.dirty(),
        landed: match entry.state.status.landed {
            rimz::worktree::LandedVerdict::Landed => Some(true),
            rimz::worktree::LandedVerdict::Unlanded => Some(false),
            rimz::worktree::LandedVerdict::Unknown => None,
        },
        reasons: entry.state.keep_reasons(),
        holders: &entry.state.holders,
    }
}

fn render_worktree_table(dir: &Path, entries: &[InspectedWorktree]) -> Result<()> {
    let mut out = render::out();
    let dir = render::home_relative_path(&rimz::utils::path::normalize_path_lexical(dir));
    if entries.is_empty() {
        writeln!(out, "no worktrees in {dir}")?;
        return Ok(());
    }
    let mut parts = vec![format!("{} worktrees in {dir}", entries.len())];
    let removable = entries
        .iter()
        .filter(|entry| entry.state.removable())
        .count();
    let in_use = entries
        .iter()
        .filter(|entry| !entry.state.holders.is_empty())
        .count();
    let unlanded = entries
        .iter()
        .filter(|entry| {
            entry.state.holders.is_empty()
                && entry.state.status.landed != rimz::worktree::LandedVerdict::Landed
        })
        .count();
    if removable > 0 {
        parts.push(format!("{removable} removable (rimz worktree sweep)"));
    }
    if in_use > 0 {
        parts.push(format!("{in_use} in use"));
    }
    if unlanded > 0 {
        parts.push(format!("{unlanded} with unlanded work"));
    }
    writeln!(out, "{}\n", parts.join(" · "))?;
    let mut table = render::Table::new(["WORKTREE", "BRANCH", "STATE", "HOLDERS"]);
    for entry in entries {
        append_worktree_row(&mut table, entry);
    }
    table.render(&mut out)?;
    Ok(())
}

fn append_worktree_row(table: &mut render::Table, entry: &InspectedWorktree) {
    let state = match entry.state.status.landed {
        rimz::worktree::LandedVerdict::Landed => "landed",
        rimz::worktree::LandedVerdict::Unlanded => "unlanded",
        rimz::worktree::LandedVerdict::Unknown => "unknown",
    };
    let mut state = render::cell(state).fg(render::status::worktree(entry.state.status.landed));
    if entry.state.status.dirty() {
        state = state.suffix("· dirty", render::palette::warn());
    }
    table.row([
        render::cell(&entry.managed.marker.name).fg(render::palette::accent()),
        render::cell(entry.managed.branch.as_deref().unwrap_or("-")).dash(),
        state,
        render::cell(holder_chips(&entry.state.holders, 3))
            .fg(render::palette::accent())
            .dash(),
    ]);
}

fn holder_chips(holders: &Holders, limit: usize) -> String {
    let mut chips = holders
        .agents
        .iter()
        .take(limit)
        .cloned()
        .collect::<Vec<_>>();
    if holders.agents.len() > limit {
        chips.push(format!("+{}", holders.agents.len() - limit));
    }
    if holders.agents.is_empty() && holders.panes > 0 {
        chips.push("pane".to_owned());
    }
    chips.extend(
        holders
            .loop_tasks
            .iter()
            .map(|name| format!("loop task {name}")),
    );
    chips.extend(holders.teams.iter().map(|name| format!("team {name}")));
    if chips.is_empty() {
        return "-".to_owned();
    }
    chips.join(" ")
}

fn remove_worktree(
    workspace: &ResolvedWorkspace,
    config: &WorktreeConfig,
    globals: &GlobalFlags,
    name: String,
    force: bool,
) -> Result<()> {
    let store = require_existing_store(workspace)?;
    let guard =
        super::worktree_protection::for_explicit_removal(&[&workspace.project_root], globals);
    let path = rimz::worktree::worktree_path(&workspace.project_root, config, &name)?;
    if force && guard.protections.protects(&path) {
        writeln!(
            std::io::stderr().lock(),
            "warning: worktree `{name}` is in use by {}; removing it anyway",
            guard.protections.holders(&path)
        )?;
    }
    // The domain owns the refusal; this only names who is holding the checkout.
    let memo = rimz::worktree::LandedMemo::load(&workspace.project_root);
    let result = rimz::worktree::remove(
        &workspace.project_root,
        config,
        &name,
        &path,
        force,
        &guard.protections,
        &memo,
    );
    memo.save();
    let removed = match result {
        Err(rimz::worktree::WorktreeErr::InUse { .. }) => bail!(
            "worktree `{name}` is in use by {}; use --force to remove it",
            guard.protections.holders(&path)
        ),
        result => named_worktree_result(result, workspace, config, &name)?,
    };
    let retirement = rimz::worktree::retire_removal(
        &store,
        &removed,
        rimz::worktree::WORKTREE_REMOVED_ARCHIVE_REASON,
        &workspace.session_name,
    );
    let session_retirement = retirement
        .session_retirement
        .context("retiring sessions for removed worktree");
    let message_archival = retirement
        .message_archival
        .context("archiving messages for removed worktree channel");
    let mut out = render::out();
    writeln!(out, "removed {name}")?;
    if removed.branch_deletion() == rimz::worktree::BranchDeletion::KeptUnmerged {
        writeln!(
            out,
            "  branch kept: its work is not proven landed on {}",
            removed.target().unwrap_or("its base")
        )?;
    } else if force && let Some(tip) = removed.branch_tip() {
        let branch = removed.branch();
        writeln!(
            out,
            "  branch deleted: {branch} was at {tip}; restore with `git branch {branch} {tip}`"
        )?;
    }
    session_retirement?;
    message_archival?;
    Ok(())
}

fn sweep_worktrees(
    workspace: &ResolvedWorkspace,
    config: &WorktreeConfig,
    globals: &GlobalFlags,
    dry_run: bool,
    json: bool,
) -> Result<()> {
    let store = if dry_run {
        let Some(store) = super::open_existing_store(workspace)? else {
            if json {
                render::json_pretty(&WorktreeSweepJson {
                    dry_run,
                    skipped: Some("no_store"),
                    sweep: &rimz::worktree::WorktreeSweep::default(),
                })?;
            } else {
                writeln!(render::out(), "sweep — skipped · no RimZ store here")?;
            }
            return Ok(());
        };
        store
    } else {
        require_existing_store(workspace)?
    };
    let guard = super::worktree_protection::for_automatic_gc(workspace, &store, globals)
        .context("reading protection facts before sweeping worktrees")?;
    let sweep = rimz::worktree::sweep_owned(
        &workspace.project_root,
        &guard.protections,
        &store,
        &workspace.session_name,
        dry_run,
        &config.hooks,
    )?;
    if json {
        render::json_pretty(&WorktreeSweepJson {
            dry_run,
            skipped: None,
            sweep: &sweep,
        })?;
    } else {
        let dir = rimz::worktree::worktree_parent(&workspace.project_root, config)?;
        report_sweep(&sweep, dry_run, &dir)?;
    }
    let problems = sweep.failed.len()
        + sweep
            .removed
            .iter()
            .filter(|removed| removed.archive_error.is_some())
            .count();
    if problems > 0 {
        bail!("worktree sweep completed with {problems} problem(s)");
    }
    Ok(())
}

#[derive(Serialize)]
struct WorktreeSweepJson<'a> {
    dry_run: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    skipped: Option<&'static str>,
    #[serde(flatten)]
    sweep: &'a rimz::worktree::WorktreeSweep,
}

fn in_use_details(holders: &Holders) -> Vec<String> {
    let mut details = Vec::new();
    if !holders.agents.is_empty() {
        details.push(holder_chips(
            &Holders {
                agents: holders.agents.clone(),
                ..Holders::default()
            },
            4,
        ));
    }
    if holders.panes > 0 {
        details.push(
            Holders {
                panes: holders.panes,
                ..Holders::default()
            }
            .to_string(),
        );
    }
    details.extend(
        holders
            .loop_tasks
            .iter()
            .map(|name| format!("loop task {name}")),
    );
    details.extend(holders.teams.iter().map(|name| format!("team {name}")));
    details
}

fn report_sweep(sweep: &rimz::worktree::WorktreeSweep, dry_run: bool, dir: &Path) -> Result<()> {
    let mut out = render::out();
    let action = if dry_run { "would remove" } else { "removed" };
    if sweep.removed.is_empty() {
        writeln!(out, "sweep — nothing to remove · {} kept", sweep.kept.len())?;
    } else {
        writeln!(
            out,
            "sweep — {action} {} · {} · {} kept",
            sweep.removed.len(),
            render::fmt_bytes(sweep.bytes()),
            sweep.kept.len(),
        )?;
    }
    for reason in [
        KeepReason::InUse,
        KeepReason::Unlanded,
        KeepReason::Dirty,
        KeepReason::Unknown,
    ] {
        let group = sweep
            .kept
            .iter()
            .filter(|kept| kept.reasons.first() == Some(&reason))
            .collect::<Vec<_>>();
        if group.is_empty() {
            continue;
        }
        let label = reason.text();
        writeln!(out, "\nkept, {label} ({})", group.len())?;
        if reason != KeepReason::InUse && group.iter().all(|kept| kept.reasons.len() == 1) {
            let names = group
                .iter()
                .map(|kept| kept.name.as_str())
                .collect::<Vec<_>>()
                .join("  ");
            for line in render::wrap_words(
                &names,
                render::terminal_columns(80).saturating_sub(2).max(1),
            ) {
                writeln!(out, "  {line}")?;
            }
            continue;
        }
        let mut table = render::Table::new(["", ""]).without_headers().indent(2);
        for kept in group {
            let mut details = if reason == KeepReason::InUse {
                in_use_details(&kept.holders)
            } else {
                Vec::new()
            };
            for extra in kept.reasons.iter().skip(1) {
                details.push(format!(
                    "also {}",
                    if *extra == KeepReason::Dirty {
                        "dirty"
                    } else {
                        extra.text()
                    }
                ));
            }
            table.row([render::cell(&kept.name), render::cell(details.join(" · "))]);
        }
        table.render(&mut out)?;
    }
    if !sweep.removed.is_empty() {
        writeln!(
            out,
            "\n{action} ({}) from {}",
            sweep.removed.len(),
            render::home_relative_path(&rimz::utils::path::normalize_path_lexical(dir))
        )?;
        let names = sweep
            .removed
            .iter()
            .map(|removed| removed.name.as_str())
            .collect::<Vec<_>>()
            .join("  ");
        for line in render::wrap_words(
            &names,
            render::terminal_columns(80).saturating_sub(2).max(1),
        ) {
            writeln!(out, "  {line}")?;
        }
    }
    for removed in &sweep.removed {
        if let Some(err) = removed.archive_error.as_deref() {
            writeln!(out, "    message archive failed: {err}")?;
        }
    }
    if dry_run && !sweep.removed.is_empty() {
        writeln!(
            out,
            "\nnothing was removed; run `rimz worktree sweep` to remove the {}",
            sweep.removed.len()
        )?;
    }
    for failed in &sweep.failed {
        writeln!(
            out,
            "  failed: {} — {}",
            failed.path.display(),
            failed.error
        )?;
    }
    Ok(())
}

fn cd_worktree(workspace: &ResolvedWorkspace, config: &WorktreeConfig, name: &str) -> Result<()> {
    let managed = resolve_worktree(workspace, config, name)?;
    let path = render::home_relative_path(&managed.path);
    if !std::io::stdin().is_terminal() {
        bail!(
            "rimz worktree cd needs a terminal; the tree is at {path} (`rimz worktree path {name}` prints it)"
        );
    }
    writeln!(
        render::err(),
        "entered {path} in a new shell; exit returns to {}",
        render::home_relative_path(&std::env::current_dir()?)
    )?;
    exec_shell(&managed.path)
}

fn resolve_worktree(
    workspace: &ResolvedWorkspace,
    config: &WorktreeConfig,
    name: &str,
) -> Result<rimz::worktree::ManagedWorktree> {
    named_worktree_result(
        rimz::worktree::resolve_owned(&workspace.project_root, config, name),
        workspace,
        config,
        name,
    )
}

fn named_worktree_result<T>(
    result: rimz::worktree::Result<T>,
    workspace: &ResolvedWorkspace,
    config: &WorktreeConfig,
    name: &str,
) -> Result<T> {
    match result {
        Err(rimz::worktree::WorktreeErr::Missing { .. }) => {
            let managed = rimz::worktree::discover_owned(&workspace.project_root)?;
            let names: Vec<_> = managed
                .iter()
                .map(|tree| tree.marker.name.as_str())
                .collect();
            let parent = rimz::worktree::worktree_parent(&workspace.project_root, config)?;
            bail!("{}", missing_worktree_message(name, &parent, &names));
        }
        result => result.map_err(Into::into),
    }
}

fn missing_worktree_message(name: &str, parent: &Path, names: &[&str]) -> String {
    let parent = render::home_relative_path(parent);
    let suggestion = rimz::utils::suggest::nearest_name(name, names.iter().copied())
        .map(|near| format!("; did you mean `{near}`?"))
        .unwrap_or_default();
    let inventory = if names.is_empty() {
        "none exist".to_owned()
    } else {
        format!("`rimz worktree list` shows the {} that exist", names.len())
    };
    let separator = if suggestion.is_empty() { "; " } else { " " };
    format!("no worktree `{name}` in {parent}{suggestion}{separator}{inventory}")
}

fn merge_worktree(
    workspace: &ResolvedWorkspace,
    config: &WorktreeConfig,
    globals: &GlobalFlags,
    name: &str,
    yes: bool,
) -> Result<()> {
    let plan = named_worktree_result(
        rimz::worktree::landing_plan(&workspace.project_root, config, name),
        workspace,
        config,
        name,
    )?;
    let target = &plan.target;
    if !yes {
        if !std::io::stdin().is_terminal() {
            bail!(
                "rimz worktree merge needs a terminal to confirm; pass --yes to land `{name}` on `{target}` without asking"
            );
        }
        if !super::confirm(&format!("land {name} on {target}?"))? {
            writeln!(render::err(), "nothing changed.")?;
            return Ok(());
        }
    }
    let guard =
        super::worktree_protection::for_explicit_removal(&[&workspace.project_root], globals);
    let memo = rimz::worktree::LandedMemo::load(&workspace.project_root);
    let result = rimz::worktree::land(&plan, &guard.protections, &memo);
    memo.save();
    let merged = match result {
        Err(rimz::worktree::WorktreeErr::MergeInUse { path, .. }) => bail!(
            "cannot merge worktree `{name}`: it is in use by {}",
            guard.protections.holders(&path)
        ),
        result => result?,
    };
    let mut out = render::out();
    writeln!(out, "landed {} on {target}", merged.name)?;
    writeln!(out, "  branch : {}", merged.branch)?;
    writeln!(out, "  head   : {}", merged.head)?;
    writeln!(
        out,
        "  the worktree stays at {}; `rimz worktree remove {name}` reclaims it",
        render::home_relative_path(&plan.managed.path)
    )?;
    Ok(())
}

fn parse_base(raw: &str) -> std::result::Result<WorktreeBase, String> {
    raw.parse()
        .map_err(|_| "base ref cannot be empty".to_owned())
}

pub(super) fn cleanup_worktree(
    path: &Path,
    globals: &GlobalFlags,
    interactive: bool,
) -> Result<()> {
    let Some(marker) = rimz::worktree::read_marker_for_worktree(path)? else {
        return Ok(());
    };
    let config = super::machine_config();
    if !interactive {
        std::thread::sleep(CLEANUP_SIGNAL_ROSTER_GRACE);
    }
    let workspace = cleanup_workspace(&marker, globals)?;
    let protections =
        match super::worktree_protection::for_wrapper_cleanup(&workspace, path, globals) {
            Ok(protection) => protection.protections,
            Err(error) => {
                tracing::debug!(%error, "loop tasks unavailable; wrapper checkout kept");
                return Ok(());
            }
        };
    let memo = rimz::worktree::LandedMemo::load(path);
    let state = rimz::worktree::inspect(path, &marker, &protections, &memo)?;
    let result = (|| {
        match state.keep_reasons().first() {
            None => {
                let removed = remove_for_cleanup(
                    path,
                    &marker,
                    &workspace,
                    false,
                    &config.agents.worktree.hooks,
                    &memo,
                )?;
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "rimz: removed clean worktree {}",
                    path.display()
                );
                report_kept_branch(&removed);
            }
            Some(KeepReason::Dirty | KeepReason::Unlanded | KeepReason::Unknown) => {
                if interactive {
                    match dirty_choice(path)? {
                        DirtyChoice::Keep => {}
                        DirtyChoice::Remove => {
                            let removed = remove_for_cleanup(
                                path,
                                &marker,
                                &workspace,
                                true,
                                &config.agents.worktree.hooks,
                                &memo,
                            )?;
                            report_kept_branch(&removed);
                        }
                        DirtyChoice::Shell => exec_shell(path)?,
                    }
                }
            }
            Some(KeepReason::InUse) => {}
        }
        Ok(())
    })();
    memo.save();
    result
}

fn cleanup_workspace(
    marker: &rimz::worktree::WorktreeMarker,
    globals: &GlobalFlags,
) -> Result<ResolvedWorkspace> {
    resolve_cleanup_workspace(marker, globals.root.clone(), verified_room_pin())
}

fn resolve_cleanup_workspace(
    marker: &rimz::worktree::WorktreeMarker,
    root_override: Option<PathBuf>,
    room_pin: Option<PathBuf>,
) -> Result<ResolvedWorkspace> {
    let root = root_override
        .or(room_pin)
        .unwrap_or_else(|| marker.repo_root.clone());
    WorkspaceResolver::resolve(".", Some(root)).context("resolving cleanup workspace")
}

fn verified_room_pin() -> Option<PathBuf> {
    let id = std::env::var_os(rimz::workspace::ENV_WORKSPACE_ID)?;
    let root = PathBuf::from(std::env::var_os(rimz::workspace::ENV_PROJECT_ROOT)?);
    rimz::workspace::verify_pin(&id.to_string_lossy(), &root)
}

fn remove_for_cleanup(
    path: &Path,
    marker: &rimz::worktree::WorktreeMarker,
    workspace: &ResolvedWorkspace,
    force: bool,
    hooks: &rimz::config::WorktreeHooks,
    memo: &rimz::worktree::LandedMemo,
) -> Result<rimz::worktree::RemovalOutcome> {
    let removed = rimz::worktree::remove_marked_worktree(
        &marker.repo_root,
        path,
        marker,
        force,
        hooks,
        memo,
    )?;
    let store = require_existing_store(workspace);
    match store {
        Ok(store) => {
            let retirement = rimz::worktree::retire_removal(
                &store,
                &removed,
                rimz::worktree::WORKTREE_REMOVED_ARCHIVE_REASON,
                &workspace.session_name,
            );
            if let Err(err) = retirement.session_retirement {
                tracing::debug!(
                    branch = %marker.branch,
                    error = %err,
                    "could not retire sessions for removed worktree",
                );
            }
            if let Err(err) = retirement.message_archival {
                tracing::debug!(
                    branch = %marker.branch,
                    error = %err,
                    "could not archive messages for removed worktree",
                );
            }
        }
        Err(err) => tracing::debug!(
            branch = %marker.branch,
            error = %err,
            "could not open store for removed worktree cleanup",
        ),
    }
    Ok(removed)
}

fn report_kept_branch(removed: &rimz::worktree::RemovalOutcome) {
    if removed.branch_deletion() == rimz::worktree::BranchDeletion::KeptUnmerged {
        let _ = writeln!(
            std::io::stderr().lock(),
            "rimz: kept branch {}: its work is not proven landed on its base",
            removed.branch()
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DirtyChoice {
    Keep,
    Remove,
    Shell,
}

fn dirty_choice(path: &Path) -> Result<DirtyChoice> {
    if !std::io::stdin().is_terminal() {
        return Ok(DirtyChoice::Keep);
    }
    let mut stderr = std::io::stderr().lock();
    writeln!(
        stderr,
        "rimz: worktree {} has uncommitted changes or unlanded work.",
        path.display()
    )?;
    drop(stderr);
    Ok(
        match super::choose("Choose", &["keep", "remove", "shell"], 0).unwrap_or(None) {
            Some(1) => DirtyChoice::Remove,
            Some(2) => DirtyChoice::Shell,
            _ => DirtyChoice::Keep,
        },
    )
}

#[cfg(unix)]
fn exec_shell(path: &Path) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let shell = rimz::proc::user_shell_program();
    let err = Command::new(&shell).current_dir(path).exec();
    Err::<(), _>(err).with_context(|| format!("execing {shell}"))
}

#[cfg(not(unix))]
fn exec_shell(path: &Path) -> Result<()> {
    let shell = rimz::proc::user_shell_program();
    let status = Command::new(&shell)
        .current_dir(path)
        .status()
        .with_context(|| format!("running {shell}"))?;
    if !status.success() {
        bail!("shell exited with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holder_chips_include_teams_after_loop_tasks() {
        let holders = Holders {
            agents: vec!["@coder".into(), "@reviewer".into()],
            loop_tasks: vec!["nightly".into()],
            teams: vec!["forge".into()],
            ..Holders::default()
        };
        assert_eq!(
            holder_chips(&holders, 1),
            "@coder +1 loop task nightly team forge"
        );
        assert_eq!(holder_chips(&Holders::default(), 3), "-");
    }

    #[test]
    fn sweep_in_use_details_include_teams_after_loop_tasks() {
        let holders = Holders {
            agents: vec!["@coder".into()],
            panes: 2,
            loop_tasks: vec!["nightly".into()],
            teams: vec!["forge".into()],
        };
        assert_eq!(
            in_use_details(&holders),
            ["@coder", "2 open panes", "loop task nightly", "team forge"]
        );
    }

    #[test]
    fn missing_worktree_names_show_near_matches_and_owned_counts() {
        let parent = Path::new("/repo-worktrees");
        assert_eq!(
            missing_worktree_message("dmo", parent, &["demo", "feature"]),
            "no worktree `dmo` in /repo-worktrees; did you mean `demo`? `rimz worktree list` shows the 2 that exist"
        );
        assert_eq!(
            missing_worktree_message("missing", parent, &["demo"]),
            "no worktree `missing` in /repo-worktrees; `rimz worktree list` shows the 1 that exist"
        );
        assert_eq!(
            missing_worktree_message("missing", parent, &[]),
            "no worktree `missing` in /repo-worktrees; none exist"
        );
    }

    #[test]
    fn created_path_display_folds_parent_components() {
        // `worktree.dir` defaults to `../{repo}-worktrees`, so the marker path
        // the create report prints carries a `..` the printed row must not.
        assert_eq!(
            render::home_relative_path(Path::new(
                "/code/query-engine/../query-engine-worktrees/feat-a"
            )),
            render::home_relative("/code/query-engine-worktrees/feat-a")
        );
    }

    #[test]
    fn list_json_maps_typed_landing_without_marker_fields() {
        for (verdict, landed, reasons) in [
            (rimz::worktree::LandedVerdict::Landed, Some(true), vec![]),
            (
                rimz::worktree::LandedVerdict::Unlanded,
                Some(false),
                vec!["unlanded"],
            ),
            (
                rimz::worktree::LandedVerdict::Unknown,
                None,
                vec!["unknown"],
            ),
        ] {
            let entry = inspected_worktree(verdict);
            let value = serde_json::to_value(worktree_json_row(&entry)).expect("serialize row");

            assert_eq!(
                value,
                serde_json::json!({
                    "name": "demo",
                    "path": "/repo-worktrees/demo",
                    "branch": "feature/demo",
                    "base_ref": "abc123",
                    "base_branch": "main",
                    "dirty": false,
                    "landed": landed,
                    "reasons": reasons,
                    "holders": {"agents": [], "panes": 0, "loop_tasks": [], "teams": []},
                })
            );
        }
    }

    #[test]
    fn cleanup_workspace_uses_room_pin_and_marker_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        let room_root = dir.path().join("room");
        let repo_root = dir.path().join("repo");
        std::fs::create_dir_all(&room_root).expect("room root");
        std::fs::create_dir_all(&repo_root).expect("repo root");
        let room_root = room_root.canonicalize().expect("canonical room");
        let repo_root = repo_root.canonicalize().expect("canonical repo");
        let marker = rimz::worktree::WorktreeMarker {
            repo_root: repo_root.clone(),
            ..worktree_marker()
        };

        let pinned =
            resolve_cleanup_workspace(&marker, None, Some(room_root.clone())).expect("pinned room");
        assert_eq!(pinned.project_root, room_root);

        let fallback =
            resolve_cleanup_workspace(&marker, None, None).expect("marker repository fallback");
        assert_eq!(fallback.project_root, repo_root);
    }

    fn inspected_worktree(landed: rimz::worktree::LandedVerdict) -> InspectedWorktree {
        let path = PathBuf::from("/repo-worktrees/demo");
        InspectedWorktree {
            managed: rimz::worktree::ManagedWorktree {
                marker: worktree_marker(),
                path,
                branch: Some("feature/demo".to_owned()),
            },
            state: rimz::worktree::WorktreeState {
                status: rimz::worktree::WorktreeStatus {
                    changed_files: 0,
                    landed,
                    ..rimz::worktree::WorktreeStatus::default()
                },
                holders: Holders::default(),
            },
        }
    }

    fn worktree_marker() -> rimz::worktree::WorktreeMarker {
        rimz::worktree::WorktreeMarker {
            version: 4,
            name: "demo".to_owned(),
            branch: "recorded/demo".to_owned(),
            base_branch: Some("main".to_owned()),
            from_pr: Some(42),
            base_ref: "abc123".to_owned(),
            repo_root: PathBuf::from("/repo"),
            worktree_path: PathBuf::from("/recorded/demo"),
            created_at: jiff::Timestamp::now(),
        }
    }
}
