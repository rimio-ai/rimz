//! Agent-only fresh-session swap, with the replaced pane closed by the caller.

use std::path::PathBuf;

use crate::agents::{self, AgentState, LoginCatalog};
use crate::config::MachineConfig;
use crate::config::effective::ProfileScope;
use crate::harness::ancestry::{LaunchFocus, resolve_caller, resolve_launch_caller};
use crate::harness::launch::ExecAction;
use crate::harness::resume::ResumePosture;
use crate::ids::{AgentKind, AgentSessionId, MuxName, PaneId};
use crate::mux::MuxBackend;
use crate::store::writer::{AgentLaunchRequest, AgentLaunchScope};
use crate::{ResolvedWorkspace, RuntimeScope, Store};

use super::{LaunchOverrides, agent_pane, resolve, swap, trust};

/// Why a molt did not bind a replacement.
#[derive(Debug, thiserror::Error)]
pub enum MoltErr {
    #[error(
        "molt runs only inside an agent: only the session holding the context can write the note. Type /rimz-molt in the agent's pane, or send it with `rimz message @<agent> '/rimz-molt'`"
    )]
    OutsideAgent,
    #[error("molt requires non-empty NOTE or --stdin")]
    EmptyNote,
    #[error("subagents cannot molt; finish and report to your caller; do not retry this command")]
    Subagent,
    #[error("unknown agent kind `{0}`")]
    UnknownKind(AgentKind),
    #[error("unknown replacement provider")]
    UnknownReplacement,
    #[error("molt produced no agent cell")]
    MissingCell,
    #[error("molt needs the caller's own pane; nothing was changed")]
    NoPane,
    #[error("{0}; nothing was changed")]
    WrongHolder(String),
    #[error("{0}; nothing was changed")]
    Find(Box<agent_pane::FindErr>),
    #[error(transparent)]
    Output(#[from] std::io::Error),
    #[error(transparent)]
    Swap(Box<swap::SwapErr>),
    // Preserve upstream launch and preflight errors, including their source chains.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl MoltErr {
    fn other(error: impl Into<anyhow::Error>) -> Self {
        Self::Other(error.into())
    }
}

impl From<swap::SwapErr> for MoltErr {
    fn from(error: swap::SwapErr) -> Self {
        Self::Swap(Box::new(error))
    }
}

type Result<T> = std::result::Result<T, MoltErr>;

/// A failed molt and the notices accumulated before it refused.
#[derive(Debug, thiserror::Error)]
#[error("{error}")]
pub struct MoltFailure {
    #[source]
    pub error: MoltErr,
    pub notices: Vec<String>,
}

/// The caller and its retained runtime projection, resolved before CLI input is read.
pub struct MoltSource<'a> {
    store: &'a Store,
    agents: Vec<AgentState>,
    source: AgentState,
}

impl<'a> MoltSource<'a> {
    /// Resolve the calling launch and refuse launched children.
    pub fn resolve(store: &'a Store) -> Result<Self> {
        let snapshot = store
            .runtime_projection(RuntimeScope::Runtime)
            .map_err(MoltErr::other)?;
        let caller = resolve_caller(&snapshot.agents).ok_or(MoltErr::OutsideAgent)?;
        let source = resolve_launch_caller(&snapshot.agents, &caller).map_err(MoltErr::other)?;
        if source.is_launched_child() {
            return Err(MoltErr::Subagent);
        }
        Ok(Self {
            store,
            source: source.clone(),
            agents: snapshot.agents,
        })
    }

    /// Launch from a resolved caller, retaining notices and also handing them to `present`
    /// when they arise, so a CLI can show them before waiting for the new pane to bind.
    pub fn launch(
        self,
        workspace: &ResolvedWorkspace,
        mux: Option<MuxName>,
        machine: &MachineConfig,
        note: String,
        alias: Option<String>,
        mut present: impl FnMut(&str) -> std::io::Result<()>,
    ) -> std::result::Result<Molted, MoltFailure> {
        let mut notices = Vec::new();
        let result =
            launch_replacement(workspace, self, mux, machine, note, alias, &mut |notice| {
                let result = present(&notice);
                notices.push(notice);
                result
            });
        match result {
            Ok(mut receipt) => {
                receipt.notices = notices;
                Ok(receipt)
            }
            Err(error) => Err(MoltFailure { error, notices }),
        }
    }
}

/// The bound successor. The replaced session stays open until
/// [`close_replaced`](Self::close_replaced) runs.
#[must_use = "the caller must decide when to close the replaced session"]
pub struct Molted {
    pub handle: String,
    pub successor_kind: AgentKind,
    /// The successor's launch reservation key, before the provider registers its session id.
    pub successor_id: AgentSessionId,
    pub notices: Vec<String>,
    workspace: ResolvedWorkspace,
    store: Store,
    source: AgentState,
    old_pane: PaneId,
    backend: Box<dyn MuxBackend>,
}

impl Molted {
    /// After delivering the receipt, release and conditionally close the replaced pane.
    /// Returns cleanup notices even if their optional presentation fails.
    pub fn close_replaced(self, present: impl FnMut(&str) -> std::io::Result<()>) -> Vec<String> {
        swap::finish_replacement(
            &self.workspace,
            &self.store,
            self.backend.as_ref(),
            &self.source,
            &self.old_pane,
            true,
            present,
        )
    }
}

/// Swap the calling agent into a fresh session. Does not present notices or close the old pane.
pub fn molt(
    workspace: &ResolvedWorkspace,
    store: &Store,
    mux: Option<MuxName>,
    machine: &MachineConfig,
    note: String,
    alias: Option<String>,
) -> std::result::Result<Molted, MoltFailure> {
    MoltSource::resolve(store)
        .map_err(|error| MoltFailure {
            error,
            notices: Vec::new(),
        })?
        .launch(workspace, mux, machine, note, alias, |_| Ok(()))
}

fn launch_replacement(
    workspace: &ResolvedWorkspace,
    caller: MoltSource<'_>,
    mux: Option<MuxName>,
    machine: &MachineConfig,
    note: String,
    alias: Option<String>,
    report: &mut impl FnMut(String) -> std::io::Result<()>,
) -> Result<Molted> {
    if note.trim().is_empty() {
        return Err(MoltErr::EmptyNote);
    }
    let store = caller.store;
    let source = &caller.source;
    let cwd = source
        .worktree_path
        .as_deref()
        .map_or_else(|| workspace.worktree_root.clone(), PathBuf::from);
    let (kind, posture) = if let Some(alias) = alias.as_deref() {
        let effective = crate::config::effective::load(machine, &workspace.project_root)
            .map_err(MoltErr::other)?;
        effective.block_set_failure().map_err(MoltErr::other)?;
        replacement_posture(
            source,
            machine,
            &effective,
            alias,
            Some((store.runtime_paths(), store.paths())),
            report,
        )?
    } else {
        let (posture, warning) = swap::restart_posture(source, workspace, machine)?;
        if let Some(warning) = warning {
            report(warning)?;
        }
        (source.kind.clone(), posture)
    };
    let mut agent = source.clone();
    agent.kind = kind.clone();
    let adapter =
        agents::find_definition(kind.as_str()).ok_or_else(|| MoltErr::UnknownKind(kind.clone()))?;
    let isolation = crate::config::Isolation::resolve(
        source.isolation,
        posture.launch.isolation_default,
        machine.agents.isolation,
    );
    crate::sandbox::preflight_launch(
        isolation,
        &kind,
        posture.launch.skills.is_some(),
        adapter.manual_skill(),
    )
    .map_err(MoltErr::other)?;
    posture
        .launch
        .validate_fresh_agent_file_lists(&kind)
        .map_err(MoltErr::other)?;
    crate::harness::launch::preflight_agent_kind(&workspace.project_root, kind.as_str(), &cwd)
        .map_err(MoltErr::other)?;
    let logins = agents::room_accounts(
        &store.paths().workspace_record,
        Some(&workspace.project_root),
        machine,
    )
    .map_err(MoltErr::other)?;
    let catalog = LoginCatalog::room_view(&machine.accounts).0;
    let login = if alias.is_some() {
        Some(logins.name(&kind).map_err(MoltErr::other)?)
    } else {
        crate::harness::resume::relaunch_login(source, &logins, &catalog).map_err(MoltErr::other)?
    };
    let provider_login =
        agents::session_login(&kind, login.as_ref(), &machine.accounts).map_err(MoltErr::other)?;
    if let Some(notice) = trust::preflight_hooks(
        provider_login.health(&agents::ambient_env()),
        || Some(trust::Unattended::Agent),
        || {
            crate::config::definitions::hook_trust_fix(
                &kind,
                alias.as_deref().or(source.profile.as_deref()),
                None,
            )
        },
        &mut Default::default(),
    )? {
        report(notice.to_string())?;
    }
    let repo_root = if source.checkout.is_none() {
        workspace
            .at_launch_dir(&cwd)
            .map_err(MoltErr::other)?
            .cwd_project_root
    } else {
        None
    };
    if let Some(notice) = trust::preflight(
        kind.as_str(),
        &cwd,
        adapter.folder_trust(
            &cwd,
            repo_root.as_deref(),
            &provider_login.env(&agents::ambient_env()),
        ),
        None,
    )? {
        report(notice.to_string())?;
    }
    let action = ExecAction::Launch {
        prompt: Some(note.clone()),
        extra_args: Vec::new(),
    };
    if !isolation.is_scoped() {
        crate::harness::launch::preflight_agent_process(
            &workspace.project_root,
            &swap::relaunch_request(&agent, &posture, action.clone(), login.clone(), None),
            &cwd,
            Some(store.runtime_paths()),
        )
        .map_err(MoltErr::other)?;
    }
    let mut notices = Vec::new();
    // An accepted window, as at restart's re-find: the briefing and the reservation run after
    // this read, while the close stays guarded by the post-bind hosting read.
    let found = agent_pane::find(
        workspace,
        store,
        mux,
        &source.kind,
        source.launch_key(),
        &mut notices,
    );
    for notice in notices {
        report(format!("note: {notice}"))?;
    }
    let old_pane = match found {
        Ok(pane) if !swap::held_by(&pane, source) => {
            return Err(MoltErr::WrongHolder(swap::held_elsewhere(&pane)));
        }
        Ok(pane) => pane.pane_id,
        Err(agent_pane::FindErr::Nowhere) => return Err(MoltErr::NoPane),
        Err(err) => return Err(MoltErr::Find(Box::new(err))),
    };
    let mut request = molt_request(&agent, &posture, login.clone())?;
    request.prompt = swap::swap_briefing(
        store,
        source,
        &caller.agents,
        &workspace.project_root,
        Some(&note),
    )?;
    let action = ExecAction::Launch {
        prompt: request.prompt.clone(),
        extra_args: Vec::new(),
    };
    let runs = if source.workflow.is_some() {
        crate::harness::run::list(store.paths()).map_err(MoltErr::other)?
    } else {
        Vec::new()
    };
    let handover = swap::open_session_run(&runs, source);
    let batch = swap::reserve_swap(
        store,
        source,
        &request,
        AgentLaunchScope {
            session_name: workspace.session_name.clone(),
            cwd: cwd.clone(),
            branch: None,
            description: None,
        },
        "molt",
    )?;
    let launch = batch.single_identity().map_err(MoltErr::other)?;
    let mut invocation = swap::relaunch_request(&agent, &posture, action, login, Some(launch));
    if let Some(run) = handover {
        invocation = swap::with_run(invocation, run);
    }
    let backend = crate::mux::backend_for(old_pane.mux());
    if let Some(warning) = swap::replace(
        workspace,
        store,
        backend.as_ref(),
        source,
        &old_pane,
        &invocation,
        &batch,
        launch,
        &caller.agents,
        &cwd,
        LaunchFocus::Keep,
        handover,
        "molt",
    )? {
        let _ = report(warning);
    }
    if alias.is_some() {
        crate::harness::assist_log::record_tier_fallbacks(batch.identities());
    }
    let peers = caller
        .agents
        .iter()
        .filter(|agent| !agent.is_provider_subagent())
        .collect::<Vec<_>>();
    let handle = crate::address::agent_handle(source, &peers, true);
    crate::harness::assist_log::append(&crate::harness::assist_log::AssistRecord {
        at: jiff::Timestamp::now(),
        assist: crate::harness::assist_log::Assist::Molt {
            kind: source.kind.clone(),
            agent_id: source.agent_id.clone(),
            label: handle.clone(),
            successor_kind: kind.clone(),
            successor_id: launch.agent_id.clone(),
            successor_label: handle.clone(),
            generation: source.launch_depth,
        },
    });
    Ok(Molted {
        handle,
        successor_kind: kind,
        successor_id: launch.agent_id.clone(),
        notices: Vec::new(),
        workspace: workspace.clone(),
        store: store.clone(),
        source: caller.source,
        old_pane,
        backend,
    })
}

fn replacement_posture(
    agent: &AgentState,
    machine: &crate::config::MachineConfig,
    effective: &crate::config::effective::LaunchAgents,
    alias: &str,
    room: Option<(&crate::RuntimePaths, &crate::StatePaths)>,
    report: &mut impl FnMut(String) -> std::io::Result<()>,
) -> Result<(AgentKind, ResumePosture)> {
    let (scope, _) = ProfileScope::for_launch(agent.profile_scope, agent.is_launched_child())
        .resolve_launch(
            agent.profile.as_deref(),
            &effective.profiles,
            &effective.subagent_profiles,
        );
    let spec = match (agent.team.as_deref(), agent.role.as_deref()) {
        (Some(team), Some(role)) => format!("{team}.{role}"),
        _ => agent
            .profile
            .clone()
            .unwrap_or_else(|| agent.kind.to_string()),
    };
    let finalized = resolve::resolve_finalized_layout(
        None,
        machine,
        effective,
        scope,
        Some(&spec),
        None,
        &LaunchOverrides {
            agent: Some(alias.into()),
            isolation: agent.isolation,
            ..Default::default()
        },
        None,
        None,
        None,
        true,
        room,
    )?;
    for warning in &finalized.warnings {
        report(warning.to_string())?;
    }
    let mut cell = finalized
        .resolved
        .layout
        .agent_cells()
        .next()
        .ok_or(MoltErr::MissingCell)?
        .clone();
    if let Some(mode) = agent.mode {
        let adapter = crate::agents::find_definition(cell.kind.as_str())
            .ok_or(MoltErr::UnknownReplacement)?;
        adapter.spec().launch.strip_permission_args(&mut cell.args);
        cell.args
            .extend(adapter.spec().launch.permission_args(mode));
        cell.launch.mode =
            adapter
                .spec()
                .launch
                .settle_permission_mode(&mut cell.args, Some(mode), None);
    }
    cell.launch.isolation = agent.isolation;
    let record = cell.launch.record.get_or_insert_with(Default::default);
    record.agent = Some(alias.into());
    record.notes = agent
        .record
        .as_ref()
        .and_then(|record| record.notes.clone());
    record.loop_section = agent
        .record
        .as_ref()
        .and_then(|record| record.loop_section.clone());
    Ok((
        cell.kind.clone(),
        ResumePosture {
            launch: crate::harness::plan::ResumeLaunchPosture::from(&cell),
            degraded: None,
        },
    ))
}

fn molt_request(
    agent: &AgentState,
    posture: &ResumePosture,
    login: Option<crate::ids::LoginName>,
) -> Result<AgentLaunchRequest> {
    let mut request = swap::fresh_launch_request(
        agent,
        swap::restart_cell(agent, posture),
        posture.launch.mode,
    )?;
    request.login = crate::store::writer::LaunchLogin::Pinned(login.unwrap_or_default());
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::PermissionMode;
    use crate::store::writer::AgentLaunchName;

    #[test]
    fn replacement_resolves_a_child_fork_and_its_alias_in_the_subagent_table() {
        let dir = tempfile::tempdir().unwrap();
        let mut machine = crate::config::MachineConfig::default();
        machine.subagents.profiles = serde_json::from_value(serde_json::json!({
            "worker": {"agent": "claude", "model": "opus"},
            "replacement": {"agent": "codex", "model": "gpt-6.1-sol"}
        }))
        .unwrap();
        let effective = crate::config::effective::load(&machine, dir.path()).unwrap();
        let mut agent = crate::testkit::agent_state("claude", "fork", jiff::Timestamp::UNIX_EPOCH);
        agent.profile = Some("worker".into());
        agent.profile_scope = Some(ProfileScope::Subagents);
        let actual = replacement_posture(
            &agent,
            &machine,
            &effective,
            "replacement",
            None,
            &mut |_| Ok(()),
        )
        .map(|(kind, posture)| (kind.to_string(), posture.launch.model))
        .map_err(|error| error.to_string());
        assert_eq!(actual, Ok(("codex".into(), Some("gpt-6.1-sol".into()))));
    }

    #[test]
    fn mini_molt_rebases_the_variant_without_losing_the_role() {
        let dir = tempfile::tempdir().unwrap();
        let mut machine = crate::config::MachineConfig::default();
        machine.agents.profiles = toml::from_str("[worker]\nagent = 'opencode-mini'").unwrap();
        let effective = crate::config::effective::load(&machine, dir.path()).unwrap();
        let mut agent =
            crate::testkit::agent_state("opencode", "mini", jiff::Timestamp::UNIX_EPOCH);
        agent.profile = Some("worker".into());
        let actual = ["opencode-mini", "opencode"].map(|base| {
            replacement_posture(&agent, &machine, &effective, base, None, &mut |_| Ok(()))
                .map(|(kind, posture)| {
                    (
                        kind.to_string(),
                        posture.launch.args.first().map(String::as_str) == Some("mini"),
                        posture.launch.record.unwrap().agent,
                    )
                })
                .map_err(|error| error.to_string())
        });
        assert_eq!(
            actual,
            [
                Ok(("opencode".into(), true, Some("opencode-mini".into()))),
                Ok(("opencode".into(), false, Some("opencode".into()))),
            ]
        );
    }

    #[test]
    fn replacement_rebases_the_same_role_without_replaying_the_old_record() {
        let dir = tempfile::tempdir().unwrap();
        let role_prompt = dir.path().join("role.md");
        std::fs::write(&role_prompt, "Keep the role's job").unwrap();
        let mut machine = crate::config::MachineConfig::default();
        machine.agents.profiles = serde_json::from_value(serde_json::json!({
            "worker": {"agent": "claude", "model": "opus"},
            "replacement": {
                "agent": "codex", "model": "gpt-6.1-sol", "effort": "medium",
                "auto-compact": "200k"
            }
        }))
        .unwrap();
        machine.agents.teams = serde_json::from_value(serde_json::json!({
            "forge": {"roles": [{"role": "coder", "profile": "worker", "system-prompt-file": role_prompt}]}
        })).unwrap();
        let effective = crate::config::effective::load(&machine, dir.path()).unwrap();
        let mut agent = crate::testkit::agent_state("claude", "old", jiff::Timestamp::UNIX_EPOCH);
        agent.profile = Some("worker".into());
        agent.team = Some("forge".into());
        agent.role = Some("coder".into());
        agent.mode = Some(PermissionMode::Ask);
        agent.isolation = Some(crate::config::Isolation::Scoped);
        agent.record = Some(Box::new(crate::agents::LaunchRecord {
            model: Some("old-record".into()),
            effort: Some("xhigh".into()),
            fast: Some(true),
            agent: None,
            auto_compact: None,
            notes: Some("20261107/002336Z-loop-review".into()),
            loop_section: None,
        }));
        let mut record = serde_json::to_value(&agent.record).unwrap();
        record["loop_section"] = "The original loop fire".into();
        agent.record = serde_json::from_value(record).unwrap();
        let (kind, posture) = replacement_posture(
            &agent,
            &machine,
            &effective,
            "replacement",
            None,
            &mut |_| Ok(()),
        )
        .unwrap();
        assert_eq!(kind.as_str(), "codex");
        assert_eq!(posture.launch.model.as_deref(), Some("gpt-6.1-sol"));
        assert_eq!(posture.launch.effort.as_deref(), Some("medium"));
        assert_eq!(
            posture.launch.record.as_ref().unwrap().notes,
            agent.record.as_ref().unwrap().notes,
            "changing the provider must keep this run's notes"
        );
        assert_eq!(posture.launch.fast, None);
        assert_eq!(
            serde_json::to_value(&posture.launch.record).unwrap()["loop_section"],
            "The original loop fire",
            "replacement molt keeps the original loop context"
        );
        assert_eq!(
            serde_json::to_value(posture.launch.record.as_ref().unwrap()).unwrap()["auto_compact"],
            200_000
        );
        assert_eq!(posture.launch.mode, agent.mode);
        assert_eq!(posture.launch.isolation, agent.isolation);
        assert_eq!(posture.launch.system_prompt_file, Some(role_prompt.into()));
        assert_eq!(
            posture.launch.record.as_ref().unwrap().agent.as_deref(),
            Some("replacement")
        );

        machine
            .agents
            .profiles
            .0
            .get_mut("replacement")
            .unwrap()
            .args = Some("-c approvals_reviewer=user".into());
        let effective = crate::config::effective::load(&machine, dir.path()).unwrap();
        agent.mode = Some(PermissionMode::Auto);
        let (_, posture) = replacement_posture(
            &agent,
            &machine,
            &effective,
            "replacement",
            None,
            &mut |_| Ok(()),
        )
        .unwrap();
        assert_eq!(
            posture.launch.mode, None,
            "raw reviewer makes the replay unknown"
        );
    }

    #[test]
    fn molt_request_copies_the_seat_and_lineage_without_opening_a_peer_turn() {
        let mut agent =
            crate::testkit::agent_state("claude", "session", jiff::Timestamp::UNIX_EPOCH);
        agent.name = Some("fox".into());
        agent.launch_id = Some("launch_old".into());
        agent.team = Some("forge".into());
        agent.role = Some("coder".into());
        agent.profile = Some("worker".into());
        agent.worktree_path = Some("/repo".into());
        crate::testkit::set_lane(&mut agent, "lane");
        agent.launch_group = Some("group".into());
        agent.launch_ordinal = Some(2);
        agent.launch_depth = Some(3);
        agent.loop_task = Some("fixer".into());
        agent.launched_by = Some(crate::agents::LaunchedBy {
            kind: AgentKind::new_unchecked("codex"),
            agent_id: "launch_root".into(),
        });
        agent.profile_scope = Some(ProfileScope::Subagents);
        let posture = ResumePosture {
            launch: Default::default(),
            degraded: None,
        };
        let request = molt_request(&agent, &posture, Some("work".parse().unwrap())).unwrap();
        assert_eq!(request.launch.launch_depth, agent.launch_depth);
        assert_eq!(
            request.launch.launched_by.as_deref(),
            agent.launched_by.as_ref()
        );
        assert_eq!(request.launch.parent_agent_id, agent.parent_agent_id);
        assert_eq!(request.launch.parent_agent_kind, agent.parent_agent_kind);
        assert_eq!(request.launch.team, agent.team);
        assert_eq!(request.launch.role, agent.role);
        assert_eq!(request.launch.profile, agent.profile);
        assert_eq!(
            request
                .launch
                .lane
                .as_ref()
                .map(|lane| String::from(lane.routing_name())),
            Some("lane".to_owned())
        );
        assert_eq!(request.launch.launch_group, agent.launch_group);
        assert_eq!(request.launch.launch_ordinal, agent.launch_ordinal);
        assert_eq!(request.launch.loop_task, agent.loop_task);
        assert_eq!(
            request.login,
            crate::store::writer::LaunchLogin::Pinned("work".parse().unwrap())
        );
        assert_eq!(request.name, AgentLaunchName::Soft("fox".into()));
        assert!(request.run_id.is_none());
        assert_eq!(request.launch.profile_scope, Some(ProfileScope::Subagents));
    }
}
