//! Resume planning and reboot detection for room rebirth.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;
use jiff::Timestamp;
use rimz::mux::{MuxBackend, SessionHealth};
use rimz::{Ledger, RuntimePaths, StatePaths};

use crate::cli::agents_cmd::team_restore::{
    PlannedTeamTab, materialize_team_restore_tab, plan_team_restore_tabs,
    planned_team_matches_agent,
};

pub(crate) fn session_is_healthy_live(backend: &dyn MuxBackend, session_name: &str) -> bool {
    let exists = backend
        .list_sessions()
        .map(|sessions| sessions.iter().any(|name| name == session_name))
        .unwrap_or(false);
    exists
        && matches!(
            backend.probe_session_health(session_name),
            Ok(SessionHealth::Healthy)
        )
}

#[cfg(target_os = "linux")]
fn boot_token() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .and_then(|id| tagged_boot_token("uuid", &id))
        .or_else(|| {
            std::fs::read_to_string("/proc/stat")
                .ok()
                .and_then(|stat| parse_proc_btime(&stat))
                .and_then(|btime| tagged_boot_token("btime", &btime))
        })
}

#[cfg(target_os = "macos")]
fn boot_token() -> Option<String> {
    sysctl_value("kern.bootsessionuuid")
        .and_then(|id| tagged_boot_token("uuid", &id))
        .or_else(|| {
            sysctl_value("kern.boottime")
                .and_then(|out| parse_kern_boottime(&out))
                .and_then(|btime| tagged_boot_token("btime", &btime))
        })
}

#[cfg(target_os = "macos")]
fn sysctl_value(name: &str) -> Option<String> {
    std::process::Command::new("sysctl")
        .args(["-n", name])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn boot_token() -> Option<String> {
    None
}

fn tagged_boot_token(source: &str, value: &str) -> Option<String> {
    non_empty_trimmed(value).map(|value| format!("{source}:{value}"))
}

fn non_empty_trimmed(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_proc_btime(stat: &str) -> Option<String> {
    stat.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        if fields.next()? != "btime" {
            return None;
        }
        let epoch = fields.next()?;
        if fields.next().is_some() || !epoch.chars().all(|ch| ch.is_ascii_digit()) {
            return None;
        }
        Some(epoch.to_owned())
    })
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_kern_boottime(out: &str) -> Option<String> {
    out.split([',', '{', '}']).find_map(|part| {
        let (key, value) = part.split_once('=')?;
        if key.trim() != "sec" {
            return None;
        }
        let epoch = value.trim();
        if epoch.is_empty() || !epoch.chars().all(|ch| ch.is_ascii_digit()) {
            return None;
        }
        Some(epoch.to_owned())
    })
}

#[derive(serde::Deserialize, serde::Serialize)]
struct BootMarker {
    boot_id: String,
}

fn read_boot_marker(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let marker = serde_json::from_slice::<BootMarker>(&bytes).ok()?;
    non_empty_trimmed(&marker.boot_id)
}

fn write_boot_marker(path: &Path, boot_id: &str) {
    let marker = BootMarker {
        boot_id: boot_id.to_owned(),
    };
    if let Err(err) = rimz::ledger::atomic::write_temp_then_rename_cache(path, &marker) {
        tracing::debug!(
            path = %path.display(),
            error = %err,
            "boot marker write skipped",
        );
    }
}

fn boot_changed(previous: Option<&str>, current: Option<&str>) -> bool {
    match (previous, current) {
        (Some(previous), Some(current)) => previous != current,
        (None, Some(_)) => true,
        (_, None) => false,
    }
}

pub(super) fn reboot_since_last_birth(workspace_id: &rimz::WorkspaceId) -> bool {
    let Ok(paths) = StatePaths::for_workspace(workspace_id.clone()) else {
        return true;
    };
    let previous = read_boot_marker(&paths.boot_marker);
    let current = boot_token();
    if let Some(current) = current.as_deref() {
        write_boot_marker(&paths.boot_marker, current);
    }
    let recover = boot_changed(previous.as_deref(), current.as_deref());
    tracing::debug!(previous = ?previous, current = ?current, recover = recover, "reboot gate");
    recover
}

/// Plan a reborn session. Prior agents are re-seeded only when the caller's
/// recovery gate is open, using the durable *audit* rollup — the one that keeps
/// the dead-process agents a runtime read would expel. Empty named channel tabs
/// restore on every ordinary rebirth. Best-effort: disabled recovery,
/// `--no-resume`, or any planning read error never blocks the launch.
pub(super) fn plan_room_resume(
    workspace_id: &rimz::WorkspaceId,
    resume_cfg: &rimz::config::ResumeConfig,
    disabled: bool,
    recover_agents: bool,
    teams: &rimz::config::TeamsConfig,
    profiles: &rimz::config::ProfilesConfig,
    commands: &rimz::config::CommandsConfig,
) -> RoomResumePlan {
    if disabled || !resume_cfg.on_rebirth {
        return RoomResumePlan::default();
    }
    let planned = (|| -> Result<RoomResumePlan> {
        let paths = StatePaths::for_workspace(workspace_id.clone())?;
        let runtime = RuntimePaths::for_workspace(workspace_id.clone())?;
        plan_room_resume_at(
            &paths,
            &runtime,
            resume_cfg,
            recover_agents,
            teams,
            profiles,
            commands,
        )
    })();
    planned.unwrap_or_else(|err| {
        tracing::warn!(workspace = %workspace_id, error = %err, "resume planning skipped");
        RoomResumePlan::default()
    })
}

#[derive(Clone, Debug, Default)]
pub(super) struct RoomResumePlan {
    flat: rimz::harness::resume::ResumePlan,
    team: Vec<PlannedTeamTab>,
}

impl RoomResumePlan {
    pub(super) fn pane_count(&self) -> usize {
        self.flat
            .tabs
            .iter()
            .map(rimz::mux::ResumeTab::pane_count)
            .sum::<usize>()
            + self.team.iter().map(planned_team_pane_count).sum::<usize>()
    }

    pub(super) fn labels(&self) -> Vec<String> {
        self.team
            .iter()
            .map(|tab| tab.label.clone())
            .chain(self.flat.tabs.iter().map(|tab| tab.label.clone()))
            .collect()
    }
}

fn plan_room_resume_at(
    paths: &StatePaths,
    runtime: &RuntimePaths,
    resume_cfg: &rimz::config::ResumeConfig,
    recover_agents: bool,
    teams: &rimz::config::TeamsConfig,
    profiles: &rimz::config::ProfilesConfig,
    commands: &rimz::config::CommandsConfig,
) -> Result<RoomResumePlan> {
    let mut plan = RoomResumePlan::default();
    if recover_agents {
        match plan_agent_resume_at(paths, runtime, resume_cfg, teams, profiles, commands) {
            Ok(agent_plan) => plan = agent_plan,
            Err(err) => {
                tracing::warn!(
                    workspace = %paths.workspace_id,
                    error = %err,
                    "agent resume planning skipped",
                );
            }
        }
    }
    Ok(plan)
}

fn plan_agent_resume_at(
    paths: &StatePaths,
    runtime: &RuntimePaths,
    resume_cfg: &rimz::config::ResumeConfig,
    teams: &rimz::config::TeamsConfig,
    profiles: &rimz::config::ProfilesConfig,
    commands: &rimz::config::CommandsConfig,
) -> Result<RoomResumePlan> {
    let ledger = Ledger::open(paths.clone(), runtime.clone())?;
    let projection = ledger.runtime_projection(rimz::RuntimeScope::Audit)?;
    let rimz_bin = rimz::proc::rimz_exe();
    let workspace_record = rimz::ledger::workspace_record::read(&paths.workspace_record).ok();
    let project_root = workspace_record
        .as_ref()
        .map(|record| record.project_root.as_path());
    let team = plan_team_restore_tabs(
        &projection.agents,
        teams,
        profiles,
        commands,
        project_root,
        |path| path.is_dir(),
    );
    let flat_agents = projection
        .agents
        .iter()
        .filter(|agent| {
            !team
                .iter()
                .any(|planned| planned_team_matches_agent(planned, agent))
        })
        .cloned()
        .collect::<Vec<_>>();
    let team_panes = team.iter().map(planned_team_pane_count).sum::<usize>();
    let flat = rimz::harness::resume::plan_resume(
        &flat_agents,
        &projection.ended,
        resume_cfg.max.saturating_sub(team_panes),
        project_root,
        |path| path.is_dir(),
        &rimz_bin,
    );
    Ok(RoomResumePlan { flat, team })
}

pub(super) fn materialize_room_resume(
    plan: RoomResumePlan,
    paths: &StatePaths,
    runtime: &RuntimePaths,
    session_name: &str,
    teams: &rimz::config::TeamsConfig,
) -> rimz::harness::resume::ResumePlan {
    let mut final_plan = rimz::harness::resume::ResumePlan {
        tabs: Vec::new(),
        skipped: plan.flat.skipped,
        tombstone: plan.flat.tombstone,
    };
    let ledger = match Ledger::open(paths.clone(), runtime.clone()) {
        Ok(ledger) => Some(ledger),
        Err(err) => {
            tracing::warn!(
                workspace = %paths.workspace_id,
                error = %err,
                "team resume materialization skipped",
            );
            None
        }
    };

    let flat_agents = ledger
        .as_ref()
        .and_then(|ledger| {
            ledger
                .runtime_projection(rimz::RuntimeScope::Audit)
                .ok()
                .map(|projection| projection.agents)
        })
        .unwrap_or_default();
    let mut tabs = Vec::new();
    for planned in &plan.team {
        let Some(ledger) = ledger.as_ref() else {
            continue;
        };
        match materialize_team_restore_tab(
            ledger,
            &paths.workspace_id,
            session_name,
            teams,
            planned,
        ) {
            Ok(tab) => tabs.push(MaterializedTab {
                freshest: Some(planned.freshest),
                tab,
            }),
            Err(err) => tracing::warn!(
                workspace = %paths.workspace_id,
                team = %planned.team,
                error = %err,
                "team resume materialization skipped",
            ),
        }
    }
    for tab in plan.flat.tabs {
        let freshest = flat_tab_freshness(&tab, &flat_agents);
        tabs.push(MaterializedTab { tab, freshest });
    }
    tabs.sort_by(materialized_tab_cmp);
    final_plan.tabs = tabs.into_iter().map(|tab| tab.tab).collect();
    add_empty_named_channel_tabs(paths, &mut final_plan);
    if let Some(ledger) = ledger.as_ref() {
        record_worktree_gone_tombstones(ledger, &paths.workspace_id, session_name, &final_plan);
    }
    final_plan
}

struct MaterializedTab {
    tab: rimz::mux::ResumeTab,
    freshest: Option<Timestamp>,
}

fn materialized_tab_cmp(left: &MaterializedTab, right: &MaterializedTab) -> std::cmp::Ordering {
    match (left.freshest, right.freshest) {
        (Some(left), Some(right)) => right.cmp(&left),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
    .then_with(|| left.tab.label.cmp(&right.tab.label))
}

fn planned_team_pane_count(tab: &PlannedTeamTab) -> usize {
    tab.layout
        .columns
        .iter()
        .map(|column| column.rows.len())
        .sum()
}

fn flat_tab_freshness(
    tab: &rimz::mux::ResumeTab,
    agents: &[rimz::agents::AgentState],
) -> Option<Timestamp> {
    agents
        .iter()
        .filter(|agent| flat_agent_matches_tab(agent, tab))
        .map(|agent| agent.last_activity)
        .max()
}

fn flat_agent_matches_tab(agent: &rimz::agents::AgentState, tab: &rimz::mux::ResumeTab) -> bool {
    if agent.parent_agent_id.is_some() || agent.agent_id.is_empty() || agent.pane.is_none() {
        return false;
    }
    let Some(worktree) = agent
        .worktree_path
        .as_deref()
        .filter(|path| !path.is_empty())
    else {
        return false;
    };
    let cwd = PathBuf::from(worktree);
    if let Some(channel) = agent
        .channel
        .as_deref()
        .filter(|channel| !channel.is_empty())
    {
        tab.label == format!("#{channel}")
    } else {
        tab.cwd == cwd
    }
}

fn add_empty_named_channel_tabs(paths: &StatePaths, plan: &mut rimz::harness::resume::ResumePlan) {
    let Ok(record) = rimz::ledger::workspace_record::read(&paths.workspace_record) else {
        return;
    };
    let Ok(channels) = rimz::channel::list(&paths.channels_record) else {
        return;
    };
    for channel in channels {
        let label = format!("#{}", channel.name);
        if plan.tabs.iter().any(|tab| tab.label == label) {
            continue;
        }
        plan.tabs.push(rimz::mux::ResumeTab::flat(
            label,
            record.project_root.clone(),
            Vec::new(),
        ));
    }
}

/// Draw the rebirth boundary in the ledger: a reborn mux session renumbers
/// panes from zero, so every pane stamp in the rollup now names a pane that no
/// longer exists — and the new session reuses those ids. The appended
/// `session.rebirth` event makes the fold clear all prior stamps, so a stale
/// session can never bind (or block stamp recovery of) a reborn pane id.
/// Called only on a genuine birth (`!was_live`), *after* resume planning —
/// the planner reads the old stamps to pick its candidates. Best-effort like
/// the plan itself: boundary hygiene never blocks the launch.
pub(super) fn record_rebirth_boundary(workspace_id: &rimz::WorkspaceId, session_name: &str) {
    let appended = (|| -> Result<()> {
        let paths = StatePaths::for_workspace(workspace_id.clone())?;
        let runtime = RuntimePaths::for_workspace(workspace_id.clone())?;
        let ledger = Ledger::open(paths, runtime)?;
        let event = rimz::EventEnvelope::session_rebirth(workspace_id.clone(), session_name);
        ledger.append_event(&event)?;
        Ok(())
    })();
    if let Err(err) = appended {
        tracing::warn!(workspace = %workspace_id, error = %err, "rebirth boundary skipped");
    }
}

/// Tell the user which prior agents the reborn room brought back, and which it
/// could not — to stderr, so the attach command on stdout stays clean for
/// scripting. Silent when there is nothing to resume.
pub(super) fn report_resume(plan: &rimz::harness::resume::ResumePlan) {
    if !plan.tabs.is_empty() {
        let agents = plan
            .tabs
            .iter()
            .map(rimz::mux::ResumeTab::pane_count)
            .sum::<usize>();
        let labels = plan
            .tabs
            .iter()
            .map(|tab| tab.label.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        if agents == 0 {
            let _ = writeln!(std::io::stderr(), "rimz: restored channel tab(s): {labels}");
        } else {
            let _ = writeln!(
                std::io::stderr(),
                "rimz: resumed {} agent{}: {labels}",
                agents,
                if agents == 1 { "" } else { "s" },
            );
        }
    }
    if !plan.skipped.is_empty() {
        let detail = plan
            .skipped
            .iter()
            .map(|skip| format!("{} ({})", skip.label, resume_skip_reason(skip.reason)))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(std::io::stderr(), "rimz: not resumed: {detail}");
    }
}

fn resume_skip_reason(reason: rimz::harness::resume::ResumeSkipReason) -> &'static str {
    match reason {
        rimz::harness::resume::ResumeSkipReason::NoResumeSupport => "no resume CLI",
        rimz::harness::resume::ResumeSkipReason::OverCap => "over the resume cap",
    }
}

fn record_worktree_gone_tombstones(
    ledger: &Ledger,
    workspace_id: &rimz::WorkspaceId,
    session_name: &str,
    plan: &rimz::harness::resume::ResumePlan,
) {
    for (kind, agent_id) in &plan.tombstone {
        let observation = rimz::agents::AgentLifecycleObservation::new(
            Some(agent_id.clone()),
            rimz::agents::LifecycleSignal::Ended,
        );
        let event = rimz::EventEnvelope::agent_lifecycle(
            workspace_id.clone(),
            session_name,
            kind.as_str(),
            "rimz.worktree-gone",
            &observation,
        );
        if let Err(err) = ledger.append_event(&event) {
            tracing::warn!(
                workspace = %workspace_id,
                kind = %kind,
                agent_id = %agent_id,
                error = %err,
                "resume: could not tombstone missing-worktree agent",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};
    use rimz::ids::{MuxName, PaneId};

    #[test]
    fn boot_changed_opens_only_on_unknown_or_different_boot() {
        assert!(boot_changed(None, Some("boot-a")));
        assert!(!boot_changed(Some("boot-a"), Some("boot-a")));
        assert!(boot_changed(Some("boot-a"), Some("boot-b")));
        assert!(!boot_changed(Some("boot-a"), None));
        assert!(!boot_changed(None, None));
    }

    #[test]
    fn parse_proc_btime_reads_boot_epoch() {
        let stat = "\
cpu  7705 0 3770 842810 99 0 123 0 0 0
intr 114930548
btime 1780040667
processes 2915
";

        assert_eq!(parse_proc_btime(stat), Some("1780040667".to_owned()));
        assert_eq!(parse_proc_btime("cpu 1 2 3\nprocesses 2915\n"), None);
        assert_eq!(parse_proc_btime("btime nope\n"), None);
    }

    #[test]
    fn parse_kern_boottime_reads_boot_epoch() {
        assert_eq!(
            parse_kern_boottime("{ sec = 1780040667, usec = 0 } Thu Jan 1 00:00:00 2026"),
            Some("1780040667".to_owned()),
        );
        assert_eq!(parse_kern_boottime("{ usec = 0 } Thu Jan 1"), None);
        assert_eq!(parse_kern_boottime("{ sec = nope, usec = 0 }"), None);
    }

    #[test]
    fn boot_marker_round_trips_and_ignores_unreadable_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("boot.json");

        assert_eq!(read_boot_marker(&path), None);
        write_boot_marker(&path, "boot-a");
        assert_eq!(read_boot_marker(&path), Some("boot-a".to_owned()));

        std::fs::write(&path, b"not-json").expect("write garbage");
        assert_eq!(read_boot_marker(&path), None);
    }

    #[test]
    fn plan_room_resume_at_recovery_gate_controls_agent_seeds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace_id = rimz::WorkspaceId::from_project_root(dir.path());
        let state_root = dir.path().join("state");
        let runtime_root = dir.path().join("runtime");
        let paths = StatePaths::under(workspace_id.clone(), &state_root).expect("state paths");
        let runtime =
            RuntimePaths::under(workspace_id.clone(), &runtime_root).expect("runtime paths");
        paths.ensure_dirs().expect("state dirs");
        let ledger = Ledger::open(paths.clone(), runtime.clone()).expect("open ledger");
        let worktree = dir.path().join("worktree");
        std::fs::create_dir_all(&worktree).expect("worktree");
        let mut observation =
            AgentLifecycleObservation::new(Some("sess-claude".into()), LifecycleSignal::Registered);
        observation.agent_name = Some("warm-drift".to_owned());
        observation.worktree_path = Some(worktree.display().to_string());
        observation.worktree_branch = Some("feature".to_owned());
        observation.pane_id = Some(PaneId::from_parts(MuxName::Tmux, "%99"));
        ledger
            .append_event(&rimz::EventEnvelope::agent_lifecycle(
                workspace_id,
                "rimz-test",
                "claude",
                "SessionStart",
                &observation,
            ))
            .expect("append registered agent");

        let blocked = plan_room_resume_at(
            &paths,
            &runtime,
            &rimz::config::ResumeConfig::default(),
            false,
            &rimz::config::TeamsConfig::default(),
            &rimz::config::ProfilesConfig::default(),
            &rimz::config::CommandsConfig::default(),
        )
        .expect("plan with gate closed");
        assert_eq!(agent_count(&blocked), 0);

        let allowed = plan_room_resume_at(
            &paths,
            &runtime,
            &rimz::config::ResumeConfig::default(),
            true,
            &rimz::config::TeamsConfig::default(),
            &rimz::config::ProfilesConfig::default(),
            &rimz::config::CommandsConfig::default(),
        )
        .expect("plan with gate open");
        assert_eq!(agent_count(&allowed), 1);
    }

    fn agent_count(plan: &RoomResumePlan) -> usize {
        plan.pane_count()
    }
}
