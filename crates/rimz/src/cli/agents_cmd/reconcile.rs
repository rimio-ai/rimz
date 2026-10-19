use super::*;
use crate::cli::send;
use rimz::harness::ancestry::LaunchFocus;
use rimz::message::dispatch::{DispatchMode, DispatchRequest};
use rimz::store::message::DeliveryGate;
use std::io::IsTerminal;

pub(super) enum Reconciled {
    Continue,
    Removed,
    Done,
    PromptDispatched,
    Resume(PathBuf),
}

#[allow(clippy::too_many_arguments)]
pub(super) fn reconcile_cohort_launch(
    workspace: &rimz::ResolvedWorkspace,
    machine_config: &rimz::config::MachineConfig,
    backend: &dyn rimz::mux::MuxBackend,
    store: &rimz::Store,
    name: &str,
    path: &Path,
    spec_display: &str,
    team: Option<&str>,
    cells: &[rimz::harness::plan::CohortCell],
    leader_index: Option<usize>,
    prompt: Option<&str>,
    fresh: bool,
    focus: LaunchFocus,
    loop_fire: bool,
) -> Result<Reconciled> {
    if !path.exists() {
        return Ok(Reconciled::Continue);
    }
    let projection = store.runtime_projection(rimz::RuntimeScope::Audit)?;
    let subject = cohort_subject(spec_display, team);
    match rimz::harness::resume::inspect_cohort_relaunch(
        &projection.agents,
        path,
        cells,
        team,
        leader_index,
    ) {
        rimz::harness::resume::CohortRelaunchState::Absent => Ok(Reconciled::Continue),
        rimz::harness::resume::CohortRelaunchState::Present {
            mut focus_pane,
            leader,
            members,
        } => {
            let running = already_running(name, &subject, team);
            let peers = projection.agents.iter().collect::<Vec<_>>();
            let leader = leader
                .as_ref()
                .and_then(|id| projection.agents.iter().find(|agent| &agent.agent_id == id));
            if loop_fire {
                let leader = leader
                    .map(|agent| {
                        format!(
                            ", leader {}",
                            rimz::address::agent_handle(agent, &peers, true)
                        )
                    })
                    .unwrap_or_default();
                bail!(
                    "{running}{leader}; prompt was not delivered; stop the cohort or retry with --takeover"
                );
            }
            if let Some(prompt) = prompt {
                let Some(leader) = leader else {
                    let hints = members
                        .iter()
                        .filter_map(|id| {
                            projection.agents.iter().find(|agent| &agent.agent_id == id)
                        })
                        .map(|member| {
                            shell_command([
                                "rimz",
                                "message",
                                &rimz::address::agent_handle(member, &peers, true),
                                prompt,
                            ])
                        })
                        .collect::<Vec<_>>()
                        .join("; ");
                    bail!("{running}, but its leader is not live; message a live member: {hints}");
                };
                let caller = send::resolve_caller(store)?;
                let channel = crate::cli::current_channel(workspace, Some(store));
                let target = format!("@{}", leader.agent_id);
                let result = rimz::message::dispatch::dispatch(
                    workspace,
                    store,
                    DispatchRequest {
                        target: target.clone(),
                        text: prompt.to_owned(),
                        target_scope: leader.channel(),
                        current_channel: channel.address_context(),
                        sender: send::sender_for(caller.as_ref(), channel.as_deref(), false),
                        caller,
                        automated: false,
                        allow_fanout: false,
                        reply: None,
                        mux: Some(backend.name()),
                        enter: true,
                        force: false,
                        auto_compact: None,
                        mode: DispatchMode::Boundary {
                            gate: DeliveryGate::Done,
                            not_before: None,
                            after: Vec::new(),
                            when: Vec::new(),
                        },
                    },
                )?;
                writeln!(
                    std::io::stderr().lock(),
                    "{running}; prompt sent to its leader"
                )?;
                send::report_dispatch(
                    rimz::message::DeliveryKind::Boundary,
                    &target,
                    &result.outcomes,
                    &result.compacted,
                )?;
                focus_pane = leader.pane.as_ref().map(|pane| pane.pane_id.clone());
            }
            if focus.takes_focus()
                && let Some(pane_id) = focus_pane
            {
                let focused = focus_existing(workspace, backend, store, pane_id);
                if prompt.is_some() {
                    // The prompt is already sent or queued, so a failed focus must
                    // not exit 1 and invite a retry that sends it twice.
                    if let Err(error) = focused {
                        writeln!(
                            std::io::stderr().lock(),
                            "warning: could not focus the leader's pane: {error:#}"
                        )?;
                    }
                    return Ok(Reconciled::PromptDispatched);
                }
                focused?;
                writeln!(std::io::stderr().lock(), "{running}; focused it")?;
                return Ok(Reconciled::Done);
            }
            if prompt.is_some() {
                return Ok(Reconciled::PromptDispatched);
            }
            writeln!(std::io::stderr().lock(), "{running}")?;
            Ok(Reconciled::Done)
        }
        rimz::harness::resume::CohortRelaunchState::Closed => {
            let Some(marker) = rimz::worktree::read_marker_for_worktree(path)? else {
                return Ok(Reconciled::Continue);
            };
            if fresh {
                return launch_fresh(name, &subject);
            }
            let (resume_command, fresh_command) =
                relaunch_commands(name, spec_display, team, prompt);
            // Cohort liveness was already decided above, so Git state alone
            // separates "recreate it" from "resume into it". A concurrent
            // cleanup may still remove the checkout after this decision.
            let protections = rimz::worktree::ProtectionSet::default();
            let memo = rimz::worktree::LandedMemo::load(path);
            let state = rimz::worktree::inspect(path, &marker, &protections, &memo)?;
            // A loop fire is unattended even when `loop fire` runs from a
            // terminal: it refuses with the guidance instead of prompting.
            let terminal = !loop_fire && std::io::stdin().is_terminal();
            let result = if state.removable() {
                recreate_or_done(
                    workspace,
                    machine_config,
                    store,
                    name,
                    path,
                    &subject,
                    &protections,
                    &resume_command,
                    &fresh_command,
                    &memo,
                    terminal,
                )
            } else {
                resume_or_done(
                    name,
                    &subject,
                    path,
                    &resume_command,
                    &fresh_command,
                    terminal,
                )
            };
            memo.save();
            result
        }
    }
}

pub(super) fn cohort_worktree_path(
    workspace: &rimz::ResolvedWorkspace,
    config: &rimz::config::WorktreeConfig,
    name: &str,
) -> rimz::worktree::Result<PathBuf> {
    rimz::worktree::worktree_path(workspace.launch_repo_root(), config, name)
}

fn already_running(name: &str, subject: &str, team: Option<&str>) -> String {
    match team {
        Some(team) => format!("{team}#{name} is already running"),
        None => format!("{subject} is already running in worktree `{name}`"),
    }
}

fn focus_existing(
    workspace: &rimz::ResolvedWorkspace,
    backend: &dyn rimz::mux::MuxBackend,
    store: &rimz::Store,
    pane_id: rimz::ids::PaneId,
) -> Result<()> {
    let runtime = rimz::RuntimePaths::for_project_root(&workspace.project_root)?;
    rimz::harness::hibernate::wake_pane(store, &pane_id)?;
    rimz::mux::focus_anchor::execute_action(backend, &runtime, &workspace.session_name, pane_id)?;
    Ok(())
}

fn cohort_subject(spec_display: &str, team: Option<&str>) -> String {
    match team {
        Some(team) => format!("team `{team}`"),
        None => format!("`{spec_display}`"),
    }
}

fn resume_or_done(
    name: &str,
    subject: &str,
    path: &Path,
    resume_command: &str,
    fresh_command: &str,
    terminal: bool,
) -> Result<Reconciled> {
    if !terminal {
        bail!(
            "worktree `{name}` has work in progress; resume with `{resume_command}` or launch fresh with `{fresh_command}`"
        );
    }
    match crate::cli::choose(
        &format!(
            "worktree `{name}` has work in progress; resume {subject}'s closed sessions, launch new agents into the same checkout, or cancel?"
        ),
        &["resume", "fresh", "cancel"],
        0,
    )? {
        Some(0) => Ok(Reconciled::Resume(path.to_owned())),
        Some(1) => launch_fresh(name, subject),
        _ => canceled(resume_command, fresh_command),
    }
}

#[allow(clippy::too_many_arguments)]
fn recreate_or_done(
    workspace: &rimz::ResolvedWorkspace,
    machine_config: &rimz::config::MachineConfig,
    store: &rimz::Store,
    name: &str,
    path: &Path,
    subject: &str,
    protections: &rimz::worktree::ProtectionSet,
    resume_command: &str,
    fresh_command: &str,
    memo: &rimz::worktree::LandedMemo,
    terminal: bool,
) -> Result<Reconciled> {
    if !terminal {
        bail!(
            "worktree `{name}` is clean and landed; recreate {subject} with `rimz worktree remove {name}` then relaunch, resume with `{resume_command}`, or launch fresh into it with `{fresh_command}`"
        );
    }
    match crate::cli::choose(
        &format!(
            "worktree `{name}` is clean and landed; remove it and recreate {subject}, launch it fresh, or cancel?"
        ),
        &["remove", "fresh", "cancel"],
        2,
    )? {
        Some(0) => {}
        Some(1) => return launch_fresh(name, subject),
        _ => return canceled(resume_command, fresh_command),
    }
    let removed = match rimz::worktree::remove(
        workspace.launch_repo_root(),
        &machine_config.agents.worktree,
        name,
        path,
        false,
        protections,
        memo,
    ) {
        Err(rimz::worktree::WorktreeErr::Missing { .. }) => {
            writeln!(
                std::io::stderr().lock(),
                "worktree `{name}` was already removed; recreating {subject}"
            )?;
            return Ok(Reconciled::Removed);
        }
        result => result?,
    };
    let retirement = rimz::worktree::retire_removal(
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
    Ok(Reconciled::Removed)
}

fn launch_fresh(name: &str, subject: &str) -> Result<Reconciled> {
    writeln!(
        std::io::stderr().lock(),
        "launching {subject} fresh in worktree `{name}`"
    )?;
    Ok(Reconciled::Continue)
}

fn canceled(resume_command: &str, fresh_command: &str) -> Result<Reconciled> {
    bail!(
        "canceled; nothing launched; resume with `{resume_command}` or launch fresh with `{fresh_command}`"
    )
}

fn relaunch_commands(
    name: &str,
    spec_display: &str,
    team: Option<&str>,
    prompt: Option<&str>,
) -> (String, String) {
    let target = team.map(|team| format!("{team}#{name}"));
    let (mut resume, mut fresh) = match target.as_deref() {
        Some(target) => (
            vec!["rimz", "teams", "resume", target],
            vec!["rimz", "teams", target, "--fresh"],
        ),
        None => (
            vec!["rimz", "agents", spec_display, "-w", name, "--continue"],
            vec!["rimz", "agents", spec_display, "-w", name, "--fresh"],
        ),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_refuses_without_launching() {
        assert!(canceled("resume", "fresh").is_err());
    }

    #[test]
    fn nonterminal_work_in_progress_refuses() {
        assert!(
            resume_or_done(
                "topic",
                "forge",
                Path::new("/topic"),
                "resume",
                "fresh",
                false
            )
            .is_err()
        );
    }

    #[test]
    fn nonterminal_clean_landed_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let id = rimz::WorkspaceId::from_project_root(dir.path());
        let workspace = rimz::ResolvedWorkspace {
            workspace_id: id.clone(),
            project_root: dir.path().into(),
            cwd_project_root: None,
            root_class: rimz::workspace::RootClass::Directory,
            worktree_root: dir.path().into(),
            worktree_branch: None,
            session_name: "reconcile-test".into(),
            mux_hint: None,
        };
        let store = rimz::Store::open(
            rimz::StatePaths::under(id.clone(), &dir.path().join("state")).unwrap(),
            rimz::RuntimePaths::under(id, &dir.path().join("runtime")).unwrap(),
        )
        .unwrap();
        let result = recreate_or_done(
            &workspace,
            &rimz::config::MachineConfig::default(),
            &store,
            "topic",
            dir.path(),
            "forge",
            &rimz::worktree::ProtectionSet::default(),
            "resume",
            "fresh",
            &rimz::worktree::LandedMemo::load(dir.path()),
            false,
        );
        assert!(result.is_err());
        assert!(dir.path().exists());
    }

    #[test]
    fn running_cohorts_name_a_team_address_or_an_inline_spec_and_worktree() {
        assert_eq!(
            already_running("topic", "`forge`", Some("forge")),
            "forge#topic is already running"
        );
        assert_eq!(
            already_running("topic", "`claude,codex`", None),
            "`claude,codex` is already running in worktree `topic`"
        );
    }

    #[test]
    fn relaunch_commands_cover_teams_and_inline_specs() {
        assert_eq!(
            relaunch_commands("topic", "forge", Some("forge"), None),
            (
                "rimz teams resume 'forge#topic'".to_owned(),
                "rimz teams 'forge#topic' --fresh".to_owned(),
            )
        );
        assert_eq!(
            relaunch_commands("topic", "claude,codex", None, None),
            (
                "rimz agents 'claude,codex' -w topic --continue".to_owned(),
                "rimz agents 'claude,codex' -w topic --fresh".to_owned(),
            )
        );
    }

    #[test]
    fn relaunch_commands_preserve_prompt_as_one_argument() {
        for (team, prompt) in [
            (Some("forge"), "review 'this' and $(that)"),
            (None, "review 'this' and $(that)"),
            (Some("forge"), "--fresh"),
        ] {
            let (resume, fresh) = relaunch_commands("topic", "claude,codex", team, Some(prompt));
            for command in [resume, fresh] {
                let words = shlex::split(&command).unwrap();
                assert_eq!(words.last().map(String::as_str), Some(prompt));
                if prompt.starts_with('-') {
                    assert_eq!(words[words.len() - 2], "--");
                }
            }
        }
    }

    #[test]
    fn cohort_reconciliation_resolves_worktree_from_current_repo() {
        let project_root = PathBuf::from("/repos/room");
        let workspace = rimz::ResolvedWorkspace {
            workspace_id: rimz::WorkspaceId::from_project_root(&project_root),
            project_root: project_root.clone(),
            cwd_project_root: Some(PathBuf::from("/repos/current")),
            root_class: rimz::workspace::RootClass::Repo,
            worktree_root: project_root,
            worktree_branch: Some("main".to_owned()),
            session_name: "rimz-room".to_owned(),
            mux_hint: None,
        };

        let path = cohort_worktree_path(
            &workspace,
            &rimz::config::WorktreeConfig::default(),
            "cross-root",
        )
        .expect("worktree path");

        assert_eq!(
            path,
            PathBuf::from("/repos/current/../current-worktrees/cross-root")
        );
    }
}
