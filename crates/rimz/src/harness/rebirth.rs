//! Two-phase previous-incarnation inspection and room rebirth materialization.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use jiff::Timestamp;

use crate::agents::AgentState;
use crate::config::{MachineConfig, ProfilesConfig, TeamsConfig};
use crate::harness::resume::{
    PlannedTeamTab, ResumePlan, materialize_team_restore_tab, resume_session_present,
    split_team_and_flat,
};
use crate::ids::{AgentKind, AgentSessionId, WorkspaceId};
use crate::mux::{MuxBackend, ResumeTab};
use crate::store::event::{LastDeathMarker, SessionDeathAgent, SessionDeathCause};
use crate::store::paths::{RuntimePaths, StatePaths, cache_home, config_home};
use crate::{Store, channel};

const CRASH_ARCHIVE_RETENTION: usize = 5;

#[derive(Debug, thiserror::Error)]
pub enum RebirthErr {
    #[error(transparent)]
    Inspect(#[from] anyhow::Error),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RebirthChoice {
    Recover,
    Fresh,
}

#[derive(Clone, Debug)]
pub struct RebirthPreview {
    death: Option<LastDeathMarker>,
    pane_count: usize,
    labels: Vec<String>,
}

impl RebirthPreview {
    pub fn death(&self) -> Option<&LastDeathMarker> {
        self.death.as_ref()
    }

    pub const fn pane_count(&self) -> usize {
        self.pane_count
    }

    pub fn labels(&self) -> &[String] {
        &self.labels
    }
}

#[derive(Clone, Debug)]
pub struct RebirthPlan {
    paths: StatePaths,
    runtime: RuntimePaths,
    boot_token: Option<String>,
    death: Option<LastDeathMarker>,
    crash_roster: Vec<AgentState>,
    crash_cache: CrashCacheSnapshot,
    planned: PlannedResume,
    empty_tabs: Vec<ResumeTab>,
    teams: TeamsConfig,
}

#[derive(Clone, Debug, Default)]
struct CrashCacheSnapshot {
    entries: Vec<CrashCacheEntry>,
    error: Option<String>,
}

#[derive(Clone, Debug)]
enum CrashCacheEntry {
    Directory(PathBuf),
    File { path: PathBuf, bytes: Vec<u8> },
}

#[derive(Clone, Debug, Default)]
struct PlannedResume {
    flat: ResumePlan,
    team: Vec<PlannedTeamTab>,
    agents: Vec<AgentState>,
}

#[derive(Clone, Debug)]
pub struct RebirthOutcome {
    pub resume: ResumePlan,
    pub death: Option<LastDeathMarker>,
}

impl RebirthPlan {
    /// Inspect prior state without changing markers, archives, event logs, or
    /// the persisted live roster.
    pub fn inspect(
        backend: &dyn MuxBackend,
        workspace_id: &WorkspaceId,
        session_name: &str,
        project_root: &Path,
        machine: &MachineConfig,
        disabled: bool,
    ) -> std::result::Result<Self, RebirthErr> {
        let paths = StatePaths::for_workspace(workspace_id.clone()).map_err(anyhow::Error::from)?;
        let runtime =
            RuntimePaths::for_workspace(workspace_id.clone()).map_err(anyhow::Error::from)?;
        let boot = boot_token();
        let cache_sources = backend.resurrection_cache_paths(session_name);
        inspect_at(
            paths,
            runtime,
            boot,
            cache_sources,
            project_root,
            machine,
            disabled,
        )
        .map_err(RebirthErr::Inspect)
    }

    pub fn preview(&self) -> RebirthPreview {
        let pane_count = self
            .planned
            .flat
            .tabs
            .iter()
            .map(ResumeTab::pane_count)
            .sum::<usize>()
            + self
                .planned
                .team
                .iter()
                .map(planned_team_pane_count)
                .sum::<usize>();
        let labels = self
            .planned
            .team
            .iter()
            .map(|tab| tab.label.clone())
            .chain(self.planned.flat.tabs.iter().map(|tab| tab.label.clone()))
            .collect();
        RebirthPreview {
            death: self.death.clone(),
            pane_count,
            labels,
        }
    }

    /// Commit post-choice side effects after the multiplexer session exists.
    pub fn materialize(self, choice: RebirthChoice, session_name: &str) -> RebirthOutcome {
        if let Some(boot) = self.boot_token.as_deref() {
            write_boot_marker(&self.paths.boot_marker, boot);
        }

        let store = match Store::open(self.paths.clone(), self.runtime.clone()) {
            Ok(store) => Some(store),
            Err(err) => {
                tracing::warn!(workspace = %self.paths.workspace_id, error = %err, "rebirth store unavailable");
                None
            }
        };
        if let Some(death) = self.death.as_ref() {
            if let Some(store) = store.as_ref() {
                append_session_death(store, &self.paths.workspace_id, session_name, death);
            }
            write_last_death_marker(&self.paths, death);
        }
        if self
            .death
            .as_ref()
            .is_some_and(|death| death.cause == SessionDeathCause::Crash)
            && let Err(err) = archive_crash(
                &self.paths,
                &self.crash_cache,
                &self.crash_roster,
                self.death
                    .as_ref()
                    .map_or(Timestamp::now(), |death| death.at),
            )
        {
            tracing::debug!(workspace = %self.paths.workspace_id, error = %err, "crash archive skipped");
        }

        let resume = if choice == RebirthChoice::Recover {
            materialize_recovery(
                store.as_ref(),
                &self.paths,
                session_name,
                &self.teams,
                self.planned,
                self.empty_tabs,
            )
        } else {
            ResumePlan::default()
        };
        let recovered = resume.tabs.iter().map(ResumeTab::pane_count).sum();
        let death = self.death.map(|mut death| {
            death.recovered = Some(recovered);
            write_last_death_marker(&self.paths, &death);
            death
        });
        append_rebirth_and_consume(
            store.as_ref(),
            &self.paths,
            &self.paths.workspace_id,
            session_name,
        );
        RebirthOutcome { resume, death }
    }
}

fn inspect_at(
    paths: StatePaths,
    runtime: RuntimePaths,
    current_boot: Option<String>,
    cache_sources: Vec<PathBuf>,
    project_root: &Path,
    machine: &MachineConfig,
    disabled: bool,
) -> Result<RebirthPlan> {
    let previous_boot = read_boot_marker(&paths.boot_marker);
    let reboot = boot_changed(previous_boot.as_deref(), current_boot.as_deref());
    let audit = Store::open_existing(paths.clone(), runtime.clone()).and_then(|store| {
        store
            .runtime_projection(crate::RuntimeScope::Audit)
            .ok()
            .map(|projection| (store, projection))
    });
    let roster = audit
        .as_ref()
        .map(|(_, projection)| recovery_roster(&paths, &projection.agents))
        .unwrap_or_default();
    let recover_agents = reboot || !roster.is_empty();
    let death = audit
        .as_ref()
        .filter(|_| recover_agents)
        .map(|(_, projection)| {
            let cause = if reboot {
                SessionDeathCause::Reboot
            } else {
                SessionDeathCause::Crash
            };
            LastDeathMarker {
                cause,
                lost_agents: lost_agent_summaries(&projection.agents, &roster),
                at: Timestamp::now(),
                recovered: None,
            }
        });
    let crash_roster = audit
        .as_ref()
        .map(|(_, projection)| lost_agent_roster(&projection.agents, &roster))
        .unwrap_or_default();
    let crash_cache = if death
        .as_ref()
        .is_some_and(|death| death.cause == SessionDeathCause::Crash)
    {
        capture_cache_sources(&cache_home(), &cache_sources)
    } else {
        CrashCacheSnapshot::default()
    };

    let recovery_enabled = !disabled && machine.resume.on_rebirth;
    let teams_and_profiles = effective_teams_and_profiles(machine, project_root);
    let planned = if recovery_enabled && recover_agents {
        plan_recovery(
            audit.as_ref().map(|(_, projection)| projection),
            &paths,
            &roster,
            &machine.resume,
            &teams_and_profiles.0,
            &teams_and_profiles.1,
            &machine.agents.commands,
        )
    } else {
        PlannedResume::default()
    };
    let empty_tabs = empty_named_channel_tabs(&paths);
    Ok(RebirthPlan {
        paths,
        runtime,
        boot_token: current_boot,
        death,
        crash_roster,
        crash_cache,
        planned,
        empty_tabs,
        teams: teams_and_profiles.0,
    })
}

fn effective_teams_and_profiles(
    machine: &MachineConfig,
    project_root: &Path,
) -> (TeamsConfig, ProfilesConfig) {
    match crate::config::effective::load(&machine.agents, project_root, &config_home()) {
        Ok(launch) => (launch.teams, launch.profiles),
        Err(err) => {
            tracing::warn!(error = %err, "effective agent config unavailable; team resume uses machine config only");
            (
                machine.agents.teams.clone(),
                machine.agents.profiles.clone(),
            )
        }
    }
}

fn plan_recovery(
    projection: Option<&crate::RuntimeProjection>,
    paths: &StatePaths,
    roster: &BTreeSet<(AgentKind, AgentSessionId)>,
    resume_cfg: &crate::config::ResumeConfig,
    teams: &TeamsConfig,
    profiles: &ProfilesConfig,
    commands: &crate::config::CommandsConfig,
) -> PlannedResume {
    let Some(projection) = projection else {
        return PlannedResume::default();
    };
    let agents = scope_to_roster(projection.agents.clone(), roster);
    let project_root = crate::store::workspace_record::read(&paths.workspace_record)
        .ok()
        .map(|record| record.project_root);
    let (team, flat_agents) = split_team_and_flat(
        &agents,
        teams,
        profiles,
        commands,
        project_root.as_deref(),
        Path::is_dir,
        resume_session_present,
    );
    let team_panes = team.iter().map(planned_team_pane_count).sum::<usize>();
    let flat = crate::harness::resume::plan_resume(
        &flat_agents,
        &projection.ended,
        resume_cfg.max.saturating_sub(team_panes),
        project_root.as_deref(),
        Path::is_dir,
        resume_session_present,
        &crate::proc::rimz_exe(),
    );
    PlannedResume { flat, team, agents }
}

fn materialize_recovery(
    store: Option<&Store>,
    paths: &StatePaths,
    session_name: &str,
    teams: &TeamsConfig,
    planned: PlannedResume,
    empty_tabs: Vec<ResumeTab>,
) -> ResumePlan {
    let mut final_plan = ResumePlan {
        tabs: Vec::new(),
        skipped: planned.flat.skipped,
        tombstone: planned.flat.tombstone,
    };
    let mut tabs = Vec::new();
    for team in &planned.team {
        let Some(store) = store else {
            continue;
        };
        match materialize_team_restore_tab(store, &paths.workspace_id, session_name, teams, team) {
            Ok(tab) => tabs.push(MaterializedTab {
                freshest: Some(team.freshest),
                tab,
            }),
            Err(err) => {
                tracing::warn!(workspace = %paths.workspace_id, team = %team.team, error = %err, "team resume materialization skipped")
            }
        }
    }
    for tab in planned.flat.tabs {
        tabs.push(MaterializedTab {
            freshest: flat_tab_freshness(&tab, &planned.agents),
            tab,
        });
    }
    tabs.sort_by(materialized_tab_cmp);
    final_plan.tabs = tabs.into_iter().map(|tab| tab.tab).collect();
    for tab in empty_tabs {
        if !final_plan
            .tabs
            .iter()
            .any(|existing| existing.label == tab.label)
        {
            final_plan.tabs.push(tab);
        }
    }
    if let Some(store) = store {
        record_worktree_gone_tombstones(store, &paths.workspace_id, session_name, &final_plan);
    }
    final_plan
}

struct MaterializedTab {
    tab: ResumeTab,
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

fn flat_tab_freshness(tab: &ResumeTab, agents: &[AgentState]) -> Option<Timestamp> {
    agents
        .iter()
        .filter(|agent| flat_agent_matches_tab(agent, tab))
        .map(|agent| agent.last_activity)
        .max()
}

fn flat_agent_matches_tab(agent: &AgentState, tab: &ResumeTab) -> bool {
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

fn empty_named_channel_tabs(paths: &StatePaths) -> Vec<ResumeTab> {
    let Ok(record) = crate::store::workspace_record::read(&paths.workspace_record) else {
        return Vec::new();
    };
    channel::list(&paths.channels_record)
        .unwrap_or_default()
        .into_iter()
        .map(|channel| {
            ResumeTab::flat(
                format!("#{}", channel.name),
                record.project_root.clone(),
                Vec::new(),
            )
        })
        .collect()
}

fn record_worktree_gone_tombstones(
    store: &Store,
    workspace_id: &WorkspaceId,
    session_name: &str,
    plan: &ResumePlan,
) {
    for (kind, agent_id) in &plan.tombstone {
        let observation = crate::agents::AgentLifecycleObservation::new(
            Some(agent_id.clone()),
            crate::agents::LifecycleSignal::Ended,
        );
        let event = crate::EventEnvelope::agent_lifecycle(
            workspace_id.clone(),
            session_name,
            kind.as_str(),
            "rimz.worktree-gone",
            &observation,
        );
        if let Err(err) = store.append_event(&event) {
            tracing::warn!(workspace = %workspace_id, kind = %kind, agent_id = %agent_id, error = %err, "resume: could not tombstone missing-worktree agent");
        }
    }
}

fn recovery_roster(
    paths: &StatePaths,
    agents: &[AgentState],
) -> BTreeSet<(AgentKind, AgentSessionId)> {
    let Some(roster) = crate::store::live_roster::read(&paths.live_roster) else {
        return BTreeSet::new();
    };
    let audited = agents
        .iter()
        .map(|agent| (agent.kind.clone(), agent.agent_id.clone()))
        .collect::<BTreeSet<_>>();
    roster.agents.intersection(&audited).cloned().collect()
}

fn scope_to_roster(
    agents: Vec<AgentState>,
    roster: &BTreeSet<(AgentKind, AgentSessionId)>,
) -> Vec<AgentState> {
    agents
        .into_iter()
        .filter(|agent| roster.contains(&(agent.kind.clone(), agent.agent_id.clone())))
        .collect()
}

fn lost_agent_summaries(
    agents: &[AgentState],
    lost: &BTreeSet<(AgentKind, AgentSessionId)>,
) -> Vec<SessionDeathAgent> {
    lost.iter()
        .map(|(kind, agent_id)| SessionDeathAgent {
            kind: kind.clone(),
            agent_id: agent_id.clone(),
            name: agents
                .iter()
                .find(|agent| agent.kind == *kind && agent.agent_id == *agent_id)
                .and_then(|agent| agent.name.clone()),
        })
        .collect()
}

fn lost_agent_roster(
    agents: &[AgentState],
    lost: &BTreeSet<(AgentKind, AgentSessionId)>,
) -> Vec<AgentState> {
    agents
        .iter()
        .filter(|agent| lost.contains(&(agent.kind.clone(), agent.agent_id.clone())))
        .cloned()
        .collect()
}

fn append_session_death(
    store: &Store,
    workspace_id: &WorkspaceId,
    session_name: &str,
    marker: &LastDeathMarker,
) {
    let event = crate::EventEnvelope::session_death(
        workspace_id.clone(),
        session_name,
        marker.cause,
        marker.lost_agents.clone(),
    );
    if let Err(err) = store.append_event(&event) {
        tracing::warn!(workspace = %workspace_id, session = %session_name, error = %err, "session death event skipped");
    }
}

fn write_last_death_marker(paths: &StatePaths, marker: &LastDeathMarker) {
    if let Err(err) = crate::store::atomic::write_temp_then_rename(&paths.last_death_marker, marker)
    {
        tracing::debug!(path = %paths.last_death_marker.display(), error = %err, "last death marker write skipped");
    }
}

fn append_rebirth_and_consume(
    store: Option<&Store>,
    paths: &StatePaths,
    workspace_id: &WorkspaceId,
    session_name: &str,
) {
    if let Some(store) = store {
        let event = crate::EventEnvelope::session_rebirth(workspace_id.clone(), session_name);
        if let Err(err) = store.append_event(&event) {
            tracing::warn!(workspace = %workspace_id, error = %err, "rebirth boundary skipped");
        }
    }
    clear_live_roster(&paths.live_roster);
}

pub fn record_boundary(workspace_id: &WorkspaceId, session_name: &str) {
    let result = (|| -> Result<()> {
        let paths = StatePaths::for_workspace(workspace_id.clone())?;
        let runtime = RuntimePaths::for_workspace(workspace_id.clone())?;
        record_boundary_at(paths, runtime, workspace_id, session_name);
        Ok(())
    })();
    if let Err(err) = result {
        tracing::warn!(workspace = %workspace_id, error = %err, "rebirth boundary skipped");
    }
}

fn record_boundary_at(
    paths: StatePaths,
    runtime: RuntimePaths,
    workspace_id: &WorkspaceId,
    session_name: &str,
) {
    let store = match Store::open(paths.clone(), runtime) {
        Ok(store) => Some(store),
        Err(err) => {
            tracing::warn!(workspace = %workspace_id, error = %err, "rebirth store unavailable");
            None
        }
    };
    append_rebirth_and_consume(store.as_ref(), &paths, workspace_id, session_name);
}

fn clear_live_roster(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            tracing::debug!(path = %path.display(), error = %err, "live roster clear skipped")
        }
    }
}

fn archive_crash(
    paths: &StatePaths,
    cache: &CrashCacheSnapshot,
    roster: &[AgentState],
    at: Timestamp,
) -> Result<()> {
    let archive = paths.crashes_dir.join(archive_name(at));
    let mux_cache = archive.join("mux-cache");
    std::fs::create_dir_all(&mux_cache)
        .with_context(|| format!("creating crash archive {}", mux_cache.display()))?;
    write_cache_snapshot(cache, &mux_cache)?;
    crate::store::atomic::write_temp_then_rename(&archive.join("roster.json"), &roster)
        .with_context(|| format!("writing crash roster {}", archive.display()))?;
    prune_crash_archives(&paths.crashes_dir)
}

fn archive_name(at: Timestamp) -> String {
    at.strftime("%Y%m%dT%H%M%SZ").to_string()
}

fn cache_archive_relative(cache_root: &Path, source: &Path) -> PathBuf {
    if let Ok(relative) = source.strip_prefix(cache_root)
        && !relative.as_os_str().is_empty()
    {
        return relative.to_path_buf();
    }
    PathBuf::from(source.file_name().unwrap_or_else(|| OsStr::new("cache")))
}

fn capture_cache_sources(cache_root: &Path, sources: &[PathBuf]) -> CrashCacheSnapshot {
    let mut snapshot = CrashCacheSnapshot::default();
    for source in sources {
        if let Err(err) = capture_cache_path(
            source,
            &cache_archive_relative(cache_root, source),
            &mut snapshot.entries,
        )
        .with_context(|| format!("reading mux cache {}", source.display()))
        {
            snapshot.error = Some(format!("{err:#}"));
            break;
        }
    }
    snapshot
}

fn capture_cache_path(
    source: &Path,
    relative: &Path,
    entries: &mut Vec<CrashCacheEntry>,
) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(source)?;
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    if meta.is_dir() {
        entries.push(CrashCacheEntry::Directory(relative.to_path_buf()));
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            capture_cache_path(&entry.path(), &relative.join(entry.file_name()), entries)?;
        }
    } else if meta.is_file() {
        entries.push(CrashCacheEntry::File {
            path: relative.to_path_buf(),
            bytes: std::fs::read(source)?,
        });
    }
    Ok(())
}

fn write_cache_snapshot(cache: &CrashCacheSnapshot, mux_cache: &Path) -> Result<()> {
    for entry in &cache.entries {
        match entry {
            CrashCacheEntry::Directory(path) => std::fs::create_dir_all(mux_cache.join(path))
                .with_context(|| format!("archiving mux cache {}", path.display()))?,
            CrashCacheEntry::File { path, bytes } => {
                let destination = mux_cache.join(path);
                if let Some(parent) = destination.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&destination, bytes)
                    .with_context(|| format!("archiving mux cache {}", path.display()))?;
            }
        }
    }
    if let Some(error) = cache.error.as_deref() {
        anyhow::bail!("{error}");
    }
    Ok(())
}

fn prune_crash_archives(crashes_dir: &Path) -> Result<()> {
    let mut archives = std::fs::read_dir(crashes_dir)
        .with_context(|| format!("reading crash archives {}", crashes_dir.display()))?
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .collect::<Vec<_>>();
    archives.sort_by_key(|entry| std::cmp::Reverse(entry.file_name()));
    for archive in archives.into_iter().skip(CRASH_ARCHIVE_RETENTION) {
        std::fs::remove_dir_all(archive.path())
            .with_context(|| format!("removing old crash archive {}", archive.path().display()))?;
    }
    Ok(())
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
    if let Err(err) = crate::store::atomic::write_temp_then_rename_cache(path, &marker) {
        tracing::debug!(path = %path.display(), error = %err, "boot marker write skipped");
    }
}

fn boot_changed(previous: Option<&str>, current: Option<&str>) -> bool {
    match (previous, current) {
        (Some(previous), Some(current)) => previous != current,
        (None, Some(_)) => true,
        (_, None) => false,
    }
}

#[cfg(test)]
mod tests;
