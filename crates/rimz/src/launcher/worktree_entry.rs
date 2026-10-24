//! Managed-worktree entry consent and recreation shared by launch front ends.

use super::{ClosedCohort, CohortChoice, Interaction, Notice, shell_command};
use crate::harness::worktree_entry::WorktreeEntry;
use crate::theme::fmt::rel_age;
use crate::worktree::{LandedVerdict, WorktreeMarker};
use anyhow::{Context, Result, bail};
use std::path::Path;

#[derive(Clone, Copy)]
pub enum PrintInput {
    Text,
    Stdin,
    StreamJson,
}

pub(super) fn inspect(
    store: &crate::Store,
    path: &Path,
    marker: &WorktreeMarker,
) -> Result<WorktreeEntry> {
    let audit = store.runtime_projection(crate::RuntimeScope::Audit)?;
    Ok(crate::harness::worktree_entry::read(
        path,
        marker,
        &audit.agents,
    )?)
}

#[allow(clippy::too_many_arguments)]
pub fn check(
    workspace: &crate::ResolvedWorkspace,
    config: &crate::config::MachineConfig,
    root: Option<&Path>,
    mux: Option<crate::ids::MuxName>,
    store: &crate::Store,
    name: &str,
    spec: &str,
    team: Option<&str>,
    prompt: Option<&str>,
    fresh: bool,
    fresh_command: Option<&str>,
    interaction: &mut dyn Interaction,
) -> Result<()> {
    let path = crate::worktree::worktree_path(
        workspace.launch_repo_root(),
        &config.agents.worktree,
        name,
    )?;
    let Some(marker) = crate::worktree::read_marker_for_worktree(&path)? else {
        return Ok(());
    };
    let alive = alive_snapshot(store, &workspace.session_name)?;
    let repo_root = workspace.launch_repo_root();
    if crate::harness::worktree_entry::occupied(&path, repo_root, &alive.agents)? {
        return Ok(());
    }
    let subject = cohort_subject(spec, team);
    if fresh {
        return launch_fresh(name, &subject, interaction);
    }
    let facts = inspect(store, &path, &marker)?;
    let (_, command) = relaunch_commands(name, spec, team, None, prompt, None);
    let choice = interaction.closed_cohort(ClosedCohort {
        name,
        subject: &subject,
        facts: &facts,
        resume_command: None,
        fresh_command: fresh_command.unwrap_or(&command),
    })?;
    match choice {
        CohortChoice::Recreate => remove(
            workspace,
            config,
            root,
            mux,
            store,
            name,
            &path,
            &subject,
            interaction,
        ),
        CohortChoice::Fresh | CohortChoice::Resume => launch_fresh(name, &subject, interaction),
        CohortChoice::Cancel => canceled(None, fresh_command.unwrap_or(&command)),
    }
}

pub fn facts_block(name: &str, facts: &WorktreeEntry) -> String {
    let now = jiff::Timestamp::now();
    let activity = facts
        .last_activity
        .map(|at| rel_age(at, now))
        .unwrap_or_else(|| "none recorded".to_owned());
    let landed = match (
        facts.status.landed,
        facts.status.target.as_deref(),
        facts.status.ahead,
    ) {
        (LandedVerdict::Landed, Some(target), _) => format!("landed on {target}"),
        (LandedVerdict::Landed, None, _) => "landed".to_owned(),
        (LandedVerdict::Unlanded, Some(target), Some(ahead)) => {
            format!("not landed ({ahead} commits ahead of {target})")
        }
        (LandedVerdict::Unlanded, _, _) => "not landed".to_owned(),
        (LandedVerdict::Unknown, _, _) => "landed: unknown".to_owned(),
    };
    format!(
        "worktree `{name}`: created {}\n  last agent activity: {activity}\n  {} uncommitted file(s); {landed}",
        rel_age(facts.created_at, now),
        facts.status.changed_files
    )
}

pub(super) fn cohort_subject(spec_display: &str, team: Option<&str>) -> String {
    match team {
        Some(team) => format!("team `{team}`"),
        None => format!("`{spec_display}`"),
    }
}

pub(super) fn launch_fresh(
    name: &str,
    subject: &str,
    interaction: &mut dyn Interaction,
) -> Result<()> {
    interaction.notice(Notice::Reconcile(&format!(
        "launching {subject} fresh in worktree `{name}`"
    )))
}

fn canceled<T>(resume_command: Option<&str>, fresh_command: &str) -> Result<T> {
    let resume = resume_command
        .map(|command| format!("resume with `{command}` or "))
        .unwrap_or_default();
    bail!("canceled; nothing launched; {resume}launch fresh with `{fresh_command}`")
}

#[allow(clippy::too_many_arguments)]
pub(super) fn remove(
    workspace: &crate::ResolvedWorkspace,
    config: &crate::config::MachineConfig,
    root: Option<&Path>,
    mux: Option<crate::ids::MuxName>,
    store: &crate::Store,
    name: &str,
    path: &Path,
    subject: &str,
    interaction: &mut dyn Interaction,
) -> Result<()> {
    // Read holders from both this room and the room of the repository that owns the tree.
    let protections = explicit_removal_protections(
        &[&workspace.project_root, workspace.launch_repo_root()],
        root,
        mux,
    );
    let memo = crate::worktree::LandedMemo::load(path);
    let result = crate::worktree::remove(
        workspace.launch_repo_root(),
        &config.agents.worktree,
        name,
        path,
        false,
        &protections,
        &memo,
    );
    memo.save();
    let removed = match result {
        Err(crate::worktree::WorktreeErr::Missing { .. }) => {
            interaction.notice(Notice::Reconcile(&format!(
                "worktree `{name}` was already removed; recreating {subject}"
            )))?;
            return Ok(());
        }
        Err(crate::worktree::WorktreeErr::InUse { .. }) => bail!(
            "worktree `{name}` is in use by {}; nothing removed; nothing launched",
            protections.holders(path)
        ),
        result => result?,
    };
    let retirement = crate::worktree::retire_removal(
        store,
        &removed,
        "worktree recreated",
        &workspace.session_name,
    );
    let session_retirement = retirement
        .session_retirement
        .context("retiring sessions for recreated worktree");
    let message_archival = retirement
        .message_archival
        .context("archiving messages for recreated worktree channel");
    session_retirement?;
    message_archival?;
    interaction.notice(Notice::Reconcile(&format!(
        "removed worktree `{name}`; recreating {subject}"
    )))?;
    Ok(())
}

pub fn relaunch_commands(
    name: &str,
    spec_display: &str,
    team: Option<&str>,
    channel: Option<&str>,
    prompt: Option<&str>,
    print_input: Option<PrintInput>,
) -> (String, String) {
    let target = team.map(|team| format!("{team}#{name}"));
    let resume_target = team
        .zip(channel)
        .map(|(team, channel)| format!("{team}#{channel}"));
    let (mut resume, mut fresh) = match target.as_deref() {
        Some(target) => (
            vec![
                "rimz",
                "teams",
                "resume",
                resume_target.as_deref().unwrap_or(target),
            ],
            vec!["rimz", "teams", target, "--fresh"],
        ),
        None => {
            let mut fresh = vec!["rimz", "agents", spec_display];
            if print_input.is_some() {
                fresh.push("-p");
            }
            fresh.extend(["-w", name, "--fresh"]);
            match print_input {
                Some(PrintInput::Stdin) => fresh.push("--stdin"),
                Some(PrintInput::StreamJson) => fresh.extend(["--input-format", "stream-json"]),
                _ => {}
            }
            (
                vec!["rimz", "agents", spec_display, "-w", name, "--continue"],
                fresh,
            )
        }
    };
    if let Some(prompt) = prompt {
        if team.is_some() && prompt.starts_with('-') {
            resume.push("--");
            fresh.push("--");
        }
        resume.push(prompt);
        fresh.push(prompt);
    }
    (shell_command(resume), shell_command(fresh))
}

pub(super) fn refuse<T>(offer: &ClosedCohort<'_>) -> Result<T> {
    let ClosedCohort {
        name,
        subject,
        facts,
        resume_command,
        fresh_command,
    } = offer;
    let state = if facts.removable {
        "is clean and landed"
    } else {
        "has work in progress"
    };
    let resume = resume_command
        .map(|command| format!("resume with `{command}`, or "))
        .unwrap_or_default();
    let remove = if facts.removable {
        let command = shell_command(["rimz", "worktree", "remove", name]);
        format!("recreate {subject} with `{command}` then relaunch, ")
    } else {
        String::new()
    };
    bail!(
        "worktree `{name}` {state}; {remove}{resume}launch fresh with `{fresh_command}`\n{}",
        facts_block(name, facts)
    )
}

fn alive_snapshot(
    store: &crate::Store,
    session: &str,
) -> Result<crate::store::snapshot::SidebarSnapshot> {
    let base = store.snapshot_cached().context("reading agent snapshot")?;
    Ok(crate::sidebar::consumer::cached_alive_snapshot(
        base,
        store.runtime_paths(),
        session,
    ))
}

pub fn explicit_removal_protections(
    repo_roots: &[&Path],
    root: Option<&Path>,
    mux: Option<crate::ids::MuxName>,
) -> crate::worktree::ProtectionSet {
    let mut workspace_ids = std::collections::HashSet::new();
    let workspaces = repo_roots.iter().filter_map(|repo_root| match crate::workspace::WorkspaceResolver::resolve(repo_root, root.map(Path::to_path_buf)) {
        Ok(workspace) => workspace_ids.insert(workspace.workspace_id.clone()).then_some(workspace),
        Err(err) => {
            tracing::debug!(path = %repo_root.display(), error = %err, "could not resolve workspace while gathering worktree protection facts");
            None
        }
    }).collect::<Vec<_>>();
    runtime_protections(&workspaces, mux, crate::worktree::Occupancy::ProvenLive)
}

pub fn runtime_protections(
    workspaces: &[crate::ResolvedWorkspace],
    mux: Option<crate::ids::MuxName>,
    occupancy: crate::worktree::Occupancy,
) -> crate::worktree::ProtectionSet {
    let mut panes = Vec::new();
    let mut agents = Vec::new();
    let mut own_pane = None;
    for workspace in workspaces {
        if let Ok(mux) = crate::mux::auto_detect_backend(mux) {
            own_pane = own_pane.or_else(|| crate::mux::own_pane_id(mux));
            panes.extend(
                crate::mux::backend_for(mux)
                    .list_panes(crate::mux::PaneListOptions {
                        session_name: Some(workspace.session_name.clone()),
                        workspace_id: Some(workspace.workspace_id.clone()),
                        ..Default::default()
                    })
                    .map(|listing| listing.panes)
                    .unwrap_or_default(),
            );
        }
        let snapshot = (|| -> Result<_> {
            let paths = crate::StatePaths::for_project_root(&workspace.project_root)?;
            if crate::workspace::record::read(&paths.workspace_record).is_err() {
                return Ok(None);
            }
            let runtime = crate::RuntimePaths::for_state(&paths)?;
            crate::Store::open_existing(paths, runtime)
                .map(|store| alive_snapshot(&store, &workspace.session_name))
                .transpose()
        })()
        .ok()
        .flatten();
        if let Some(snapshot) = snapshot {
            agents.extend(std::sync::Arc::unwrap_or_clone(snapshot.agents));
        }
    }
    crate::worktree::protection_set_from_runtime(&panes, &agents, own_pane.as_ref(), occupancy)
}
