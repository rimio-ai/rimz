//! Hibernation admission, read-only selection, pacing, and record projection.
//! Detached freeze/reclaim writes live in the helper module; the exec wrapper stays live.

use crate::config::HibernateConfig;
use crate::ids::WorkspaceId;
use std::path::{Path, PathBuf};

use super::cache_keepalive::KeepWarmPolicy;
use crate::agents::AgentState;
use crate::config::{MachineConfig, RemoteControlConfig};
use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::proc::cgroup::ScopePath;
use crate::{Store, store::StoreErr};

const FOCUS_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

mod execute;
pub use execute::run_request;
mod wake;
pub use crate::proc::cgroup::scope_prefix;
pub(crate) use wake::wake_pane_in;
pub use wake::{
    ReconcileReport, RefaultHelperRequest, Woken, reconcile, reconcile_at, sample_wake_refault,
    wake_pane, wake_pane_at, wake_request,
};

pub struct Policy {
    schedule: crate::config::Schedule,
    remote_control: RemoteControlConfig,
    harness: crate::config::HarnessConfig,
    keep_warm: KeepWarmPolicy,
}

impl Policy {
    pub fn new(
        config: &MachineConfig,
        keep_warm: &KeepWarmPolicy,
    ) -> Result<Self, crate::config::HibernateScheduleErr> {
        Ok(Self {
            schedule: config.agents.hibernate.schedule()?,
            remote_control: config.remote_control.clone(),
            harness: config.harness.clone(),
            keep_warm: keep_warm.clone(),
        })
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct HibernateHelperRequest {
    pub workspace_id: WorkspaceId,
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    pub pane_id: PaneId,
    pub label: String,
    pub root_pid: u32,
    pub idle_secs: u64,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct WakeHelperRequest {
    pub workspace_id: WorkspaceId,
    pub pane_id: PaneId,
    pub requested_at_ms: Option<u64>,
}

pub(crate) fn spawn_wake(runtime: &crate::RuntimePaths, pane: &PaneId, requested_at_ms: u64) {
    let request = WakeHelperRequest {
        workspace_id: runtime.workspace_id.clone(),
        pane_id: pane.clone(),
        requested_at_ms: Some(requested_at_ms),
    };
    let args = crate::child_process::agent_helper_argv("wake", &request);
    if let Err(error) = crate::child_process::spawn_detached_rimz(runtime, args, "agent-wake") {
        tracing::warn!(%error, %pane, "failed to spawn hibernation wake helper");
    }
}

pub(crate) fn wake_focused(
    runtime: &crate::RuntimePaths,
    event: &crate::wakeup::events::SidebarEvent,
) {
    if !matches!(
        event,
        crate::wakeup::events::SidebarEvent::FocusChanged { .. }
    ) {
        return;
    }
    let now = crate::utils::time::unix_now_ms();
    let Ok(paths) = crate::StatePaths::for_workspace(runtime.workspace_id.clone()) else {
        return;
    };
    let entries = crate::store::hibernate::read(&paths);
    wake_focused_with(runtime, event, &entries, now, |pane| {
        spawn_wake(runtime, pane, now)
    });
}

fn wake_focused_with(
    runtime: &crate::RuntimePaths,
    event: &crate::wakeup::events::SidebarEvent,
    entries: &[crate::store::hibernate::Entry],
    now: u64,
    mut spawn: impl FnMut(&PaneId),
) {
    let path = runtime.live_path("hibernate").join("wake.json");
    let mut pacing: std::collections::HashMap<PaneId, u64> = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    for pane in focus_wake_targets(event, entries, &pacing, now) {
        pacing.insert(pane.clone(), now);
        if let Err(error) = crate::disk::atomic::write_temp_then_rename_cache(&path, &pacing) {
            tracing::debug!(%error, "failed to record wake pacing");
        }
        spawn(&pane);
    }
}

fn focus_wake_targets(
    event: &crate::wakeup::events::SidebarEvent,
    entries: &[crate::store::hibernate::Entry],
    pacing: &std::collections::HashMap<PaneId, u64>,
    now: u64,
) -> Vec<PaneId> {
    let crate::wakeup::events::SidebarEvent::FocusChanged { focused, .. } = event else {
        return Vec::new();
    };
    focused
        .iter()
        .filter(|pane| {
            entries.iter().any(|entry| &entry.pane_id == *pane)
                && pacing
                    .get(*pane)
                    .is_none_or(|last| now.saturating_sub(*last) >= 2_000)
        })
        .cloned()
        .collect()
}

#[derive(Clone, Debug)]
pub struct ScopeView {
    scope: Option<ScopePath>,
    provider_pid: u32,
    members: Vec<u32>,
    parents: std::collections::BTreeMap<u32, (String, u32)>,
    focused: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hold {
    Ended,
    ProviderSubagent,
    AwaitingInput,
    BudgetPark,
    Compacting,
    Busy,
    Clock,
    WakeClock,
    Owed(super::owed::OwedWake),
    Message,
    OpenRun,
    KeepWarm,
    Focused,
    RemoteControl,
    NoScope,
    ForeignProcess { pid: u32, comm: String },
    AlreadyHibernating,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Hibernate { idle_secs: u64 },
    Deepen { step: u32, idle_secs: u64 },
    Hold(Hold),
}

pub fn decide(
    store: &Store,
    agent: &AgentState,
    config: &Policy,
    now: jiff::Timestamp,
    scope: &ScopeView,
) -> Result<Verdict, StoreErr> {
    if let Some(entry) = crate::store::hibernate::read(store.paths())
        .iter()
        .find(|entry| entry.kind == agent.kind && entry.agent_id == agent.agent_id)
    {
        return Ok(next_step(entry, config.schedule, now).map_or(
            Verdict::Hold(Hold::AlreadyHibernating),
            |step| Verdict::Deepen {
                step,
                idle_secs: now.duration_since(rested_at(agent)).as_secs().max(0) as u64,
            },
        ));
    }
    if let Some(hold) = cheap_hold(agent, config, now, scope.focused) {
        return Ok(Verdict::Hold(hold));
    }
    if wake_stamp_holds(
        store.runtime_paths(),
        agent.pane.as_ref().map(|pane| &pane.pane_id),
        config.schedule.after,
        now,
    ) {
        return Ok(Verdict::Hold(Hold::WakeClock));
    }
    if scope.scope.is_none() {
        return Ok(Verdict::Hold(Hold::NoScope));
    }
    if !scope.members.contains(&scope.provider_pid) {
        return Ok(Verdict::Hold(Hold::NoScope));
    }
    let mut ancestors = std::collections::BTreeSet::new();
    let mut pid = scope.provider_pid;
    while scope.members.contains(&pid) && ancestors.insert(pid) {
        let Some((_, parent)) = scope.parents.get(&pid) else {
            break;
        };
        pid = *parent;
    }
    for &member in &scope.members {
        if ancestors.contains(&member) {
            continue;
        }
        let mut pid = member;
        let mut visited = std::collections::BTreeSet::new();
        while pid != scope.provider_pid {
            if !visited.insert(pid) || !scope.parents.contains_key(&pid) {
                return Ok(Verdict::Hold(Hold::ForeignProcess {
                    pid: member,
                    comm: scope
                        .parents
                        .get(&member)
                        .map(|(comm, _)| comm.clone())
                        .unwrap_or_else(|| "unknown".into()),
                }));
            }
            pid = scope.parents[&pid].1;
        }
    }
    let messages = store.list_messages()?;
    let runs = super::run::list(store.paths())?;
    if let Some(hold) = super::idle_stop::debt(store, &messages, &runs, agent)? {
        return Ok(Verdict::Hold(shared_hold(hold)));
    }
    Ok(Verdict::Hibernate {
        idle_secs: now.duration_since(rested_at(agent)).as_secs().max(0) as u64,
    })
}

fn next_step(
    entry: &crate::store::hibernate::Entry,
    schedule: crate::config::Schedule,
    now: jiff::Timestamp,
) -> Option<u32> {
    let step = entry.step.checked_add(1)?;
    (step <= schedule.final_step()
        && now.duration_since(entry.frozen_at).as_nanos() >= schedule.due(step).as_nanos() as i128)
        .then_some(step)
}

fn shared_hold(hold: super::idle_stop::Hold) -> Hold {
    use super::idle_stop::Hold as StopHold;
    match hold {
        StopHold::Ended => Hold::Ended,
        StopHold::ProviderSubagent => Hold::ProviderSubagent,
        StopHold::AwaitingInput => Hold::AwaitingInput,
        StopHold::BudgetPark => Hold::BudgetPark,
        StopHold::Compacting => Hold::Compacting,
        StopHold::Busy => Hold::Busy,
        StopHold::Clock => Hold::Clock,
        StopHold::Owed(owed) => Hold::Owed(owed),
        StopHold::Message => Hold::Message,
        StopHold::OpenRun => Hold::OpenRun,
        // Only resting/debt reach this conversion; neither evaluates a stop's tree or board.
        StopHold::Board | StopHold::Tree => {
            unreachable!("stop-specific hold from shared rest/debt")
        }
    }
}

pub fn rested_at(agent: &AgentState) -> jiff::Timestamp {
    super::idle_stop::rested_since(agent, jiff::Timestamp::MIN)
}

fn cheap_hold(
    agent: &AgentState,
    config: &Policy,
    now: jiff::Timestamp,
    focused: bool,
) -> Option<Hold> {
    if let Err(hold) = super::idle_stop::resting(agent) {
        return Some(shared_hold(hold));
    }
    if now.duration_since(rested_at(agent)).as_nanos() < config.schedule.after.as_nanos() as i128 {
        return Some(Hold::Clock);
    }
    if focused {
        return Some(Hold::Focused);
    }
    if config.remote_control.enabled_for(agent.kind.as_str()) {
        return Some(Hold::RemoteControl);
    }
    if config
        .keep_warm
        .holding(agent, &config.harness, now)
        .is_some()
    {
        return Some(Hold::KeepWarm);
    }
    None
}

fn scoped_provider(
    mut root_pid: u32,
    workspace: &WorkspaceId,
    provider: &dyn Fn(u32) -> Option<u32>,
    scope: &dyn Fn(u32) -> Option<ScopePath>,
    children: &dyn Fn(u32) -> Vec<u32>,
) -> Option<(u32, ScopePath)> {
    let prefix = scope_prefix(workspace);
    loop {
        let pid = provider(root_pid)?;
        if let Some(scope) = scope(pid).filter(|scope| {
            scope
                .as_path()
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&prefix))
        }) {
            return Some((pid, scope));
        }
        // Liveness also recognizes the exec wrapper, which stays outside this agent's
        // scope and may sit in another room's agent scope its mux server inherited.
        let children = children(pid);
        let [child] = children.as_slice() else {
            return None;
        };
        root_pid = *child;
    }
}

impl ScopeView {
    pub fn probe(kind: &AgentKind, workspace: &WorkspaceId, root_pid: u32, focused: bool) -> Self {
        let (provider_pid, scope) = scoped_provider(
            root_pid,
            workspace,
            &|pid| {
                crate::proc::in_pane_agent_process_for_root(kind.as_str(), pid)
                    .map(|process| process.pid)
            },
            &crate::proc::cgroup::scope_of,
            &crate::proc::children,
        )
        .map_or((0, None), |(pid, scope)| (pid, Some(scope)));
        let members = scope
            .as_ref()
            .map(crate::proc::cgroup::procs)
            .unwrap_or_default();
        let mut parents = std::collections::BTreeMap::new();
        for &member in &members {
            let mut pid = member;
            while pid != 0 && !parents.contains_key(&pid) {
                let Some((comm, parent)) = crate::proc::comm_and_ppid(pid) else {
                    break;
                };
                parents.insert(pid, (comm, parent));
                if pid == provider_pid {
                    break;
                }
                pid = parent;
            }
        }
        Self {
            scope,
            provider_pid,
            members,
            parents,
            focused,
        }
    }
}

pub fn hibernate_idle_agents(
    snapshot: &crate::store::snapshot::SidebarSnapshot,
    paths: &crate::StatePaths,
    runtime: &crate::RuntimePaths,
    config: &MachineConfig,
    keep_warm: &KeepWarmPolicy,
) {
    hibernate_idle_agents_with(
        snapshot,
        paths,
        runtime,
        config,
        keep_warm,
        |request| {
            let args = crate::child_process::agent_helper_argv("hibernate", request);
            match crate::child_process::spawn_detached_rimz(runtime, args, "agent-hibernate") {
                Ok(_) => true,
                Err(error) => {
                    tracing::debug!(%error, "failed to spawn hibernation helper");
                    false
                }
            }
        },
        |pane| spawn_wake(runtime, pane, crate::utils::time::unix_now_ms()),
    );
}

fn hibernate_idle_agents_with(
    snapshot: &crate::store::snapshot::SidebarSnapshot,
    paths: &crate::StatePaths,
    runtime: &crate::RuntimePaths,
    config: &MachineConfig,
    keep_warm: &KeepWarmPolicy,
    mut spawn: impl FnMut(&HibernateHelperRequest) -> bool,
    wake: impl FnMut(&PaneId),
) {
    if !cfg!(target_os = "linux") {
        return;
    }
    update_focus_trail(runtime, snapshot.focused_pane.as_ref(), snapshot.now);
    let entries = crate::store::hibernate::read(paths);
    if let Some(pane) = &snapshot.focused_pane {
        let event = crate::wakeup::events::SidebarEvent::FocusChanged {
            focused: vec![pane.clone()],
            unfocused: vec![],
        };
        wake_focused_with(
            runtime,
            &event,
            &entries,
            snapshot.now.as_millisecond().max(0) as u64,
            wake,
        );
    }
    if !config.agents.hibernate.enabled() || !Admission::read(runtime).available {
        return;
    }
    let policy = match Policy::new(config, keep_warm) {
        Ok(policy) => policy,
        Err(error) => {
            tracing::warn!(%error, "hibernation disabled for this pass");
            return;
        }
    };
    for agent in snapshot.agents.iter() {
        let entry = entries
            .iter()
            .find(|entry| entry.kind == agent.kind && entry.agent_id == agent.agent_id);
        if entry.is_some_and(|entry| next_step(entry, policy.schedule, snapshot.now).is_none()) {
            continue;
        }
        let Some(pane) = snapshot.agent_panes.iter().find(|pane| {
            pane.kind == agent.kind && pane.agent_id.as_ref() == Some(&agent.agent_id)
        }) else {
            continue;
        };
        let Some(root_pid) = pane.pane_pid else {
            continue;
        };
        if entry.is_none()
            && (cheap_hold(
                agent,
                &policy,
                snapshot.now,
                snapshot.focused_pane.as_ref() == Some(&pane.pane_id),
            )
            .is_some()
                || wake_stamp_holds(
                    runtime,
                    Some(&pane.pane_id),
                    policy.schedule.after,
                    snapshot.now,
                ))
        {
            continue;
        }
        let path = fire_record_path(runtime, &agent.kind, &agent.agent_id);
        let mut record = read_fire_record(&path);
        if record.fired_at.is_some_and(|fired| {
            snapshot.now.as_second() - fired.as_second() < 30
                && (entry.is_some()
                    || !matches!(
                        read_wake_stamp(runtime, Some(&pane.pane_id)),
                        Some(WakeStamp::Focus { focus }) if focus > fired
                    ))
        }) {
            continue;
        }
        let peers = crate::address::addressable_agents(snapshot);
        let request = HibernateHelperRequest {
            workspace_id: runtime.workspace_id.clone(),
            kind: agent.kind.clone(),
            agent_id: agent.agent_id.clone(),
            pane_id: pane.pane_id.clone(),
            root_pid,
            label: crate::address::agent_handle(agent, &peers, false),
            idle_secs: snapshot
                .now
                .duration_since(rested_at(agent))
                .as_secs()
                .max(0) as u64,
        };
        // Publish before spawning: the helper may immediately update this record's hold reason.
        record.fired_at = Some(snapshot.now);
        if let Err(error) = crate::disk::atomic::write_temp_then_rename_cache(&path, &record) {
            tracing::warn!(%error, "failed to record hibernation pacing");
        }
        if !spawn(&request) {
            record.fired_at = None;
            if let Err(error) = crate::disk::atomic::write_temp_then_rename_cache(&path, &record) {
                tracing::debug!(%error, "failed to clear hibernation pacing after spawn failure");
            }
        }
    }
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct FireRecord {
    fired_at: Option<jiff::Timestamp>,
    hold: Option<String>,
}

fn wake_stamp_path(runtime: &crate::RuntimePaths, pane: Option<&PaneId>) -> PathBuf {
    let name = pane.map_or_else(
        || "woke-workspace.json".to_owned(),
        |pane| {
            format!(
                "woke-{}.json",
                crate::store::sidecar::digest("pane", &pane.to_string())
            )
        },
    );
    runtime.live_path("hibernate").join(name)
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
enum WakeStamp {
    Sync(jiff::Timestamp),
    Focus { focus: jiff::Timestamp },
}

fn read_wake_stamp(runtime: &crate::RuntimePaths, pane: Option<&PaneId>) -> Option<WakeStamp> {
    std::fs::read(wake_stamp_path(runtime, pane))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

fn wake_stamp_holds(
    runtime: &crate::RuntimePaths,
    pane: Option<&PaneId>,
    after: std::time::Duration,
    now: jiff::Timestamp,
) -> bool {
    if let Some(WakeStamp::Sync(at)) = read_wake_stamp(runtime, None)
        && now.duration_since(at).as_nanos() < after.as_nanos() as i128
    {
        return true;
    }
    let Some(pane) = pane else {
        return false;
    };
    let (at, grace) = match read_wake_stamp(runtime, Some(pane)) {
        Some(WakeStamp::Sync(at)) => (at, after),
        Some(WakeStamp::Focus { focus }) => (
            read_focus_trail(runtime)
                .departed
                .get(pane)
                .copied()
                .unwrap_or(focus)
                .max(focus),
            FOCUS_GRACE,
        ),
        None => return false,
    };
    now.duration_since(at).as_nanos() < grace.as_nanos() as i128
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct FocusTrail {
    focused: Option<PaneId>,
    departed: std::collections::HashMap<PaneId, jiff::Timestamp>,
}

fn read_focus_trail(runtime: &crate::RuntimePaths) -> FocusTrail {
    std::fs::read(runtime.live_path("hibernate").join("focus.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn update_focus_trail(
    runtime: &crate::RuntimePaths,
    focused: Option<&PaneId>,
    now: jiff::Timestamp,
) {
    let mut trail = read_focus_trail(runtime);
    if trail.focused.as_ref() == focused {
        return;
    }
    trail
        .departed
        .retain(|_, at| now.duration_since(*at).as_nanos() < FOCUS_GRACE.as_nanos() as i128);
    if let Some(previous) = trail.focused.take() {
        trail.departed.insert(previous, now);
    }
    trail.focused = focused.cloned();
    if let Err(error) = crate::disk::atomic::write_temp_then_rename_cache(
        &runtime.live_path("hibernate").join("focus.json"),
        &trail,
    ) {
        tracing::debug!(%error, "failed to record hibernation focus trail");
    }
}

fn fire_record_path(
    runtime: &crate::RuntimePaths,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
) -> PathBuf {
    runtime.live_path("hibernate").join(format!(
        "{}.json",
        crate::store::sidecar::digest(kind.as_str(), agent_id.as_str())
    ))
}

fn read_fire_record(path: &Path) -> FireRecord {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub(crate) fn project_hibernation(
    snapshot: &mut crate::store::snapshot::SidebarSnapshot,
    paths: &crate::StatePaths,
) {
    let entries = crate::store::hibernate::read(paths);
    let projected = |agent: &crate::agents::state::AgentState| {
        entries
            .iter()
            .find(|entry| {
                agent.ended_at.is_none()
                    && entry.kind == agent.kind
                    && entry.agent_id == agent.agent_id
            })
            .map(|entry| crate::agents::state::Hibernation {
                frozen_at: entry.frozen_at,
                step: entry.step,
                reclaimed_bytes: entry.reclaimed_bytes,
                wake_error: entry.wake_error.clone(),
            })
    };
    if snapshot
        .agents
        .iter()
        .all(|agent| agent.hibernation == projected(agent))
    {
        return;
    }
    for agent in std::sync::Arc::make_mut(&mut snapshot.agents) {
        agent.hibernation = projected(agent);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HibernateErr {
    #[error(transparent)]
    Cache(#[from] crate::disk::atomic::AtomicErr),
    #[error(transparent)]
    Store(#[from] StoreErr),
    #[error(transparent)]
    Record(#[from] crate::store::hibernate::HibernateRecordErr),
    #[error(transparent)]
    Cgroup(#[from] crate::proc::cgroup::CgroupErr),
    #[error(transparent)]
    Schedule(#[from] crate::config::HibernateScheduleErr),
}

impl From<HibernateErr> for crate::blocker::Blocker {
    fn from(error: HibernateErr) -> Self {
        match error {
            HibernateErr::Schedule(error) => error.into(),
            other => Self::new(other.to_string()),
        }
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
    Warning,
    Error,
}

#[derive(Debug, serde::Serialize)]
pub struct Check {
    pub name: &'static str,
    pub status: CheckStatus,
    pub detail: String,
}

#[derive(Debug, serde::Serialize)]
pub struct PreflightReport {
    pub enabled: bool,
    pub skipped: Option<String>,
    pub checks: Vec<Check>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Admission {
    pub available: bool,
    pub reason: Option<String>,
}

impl Admission {
    pub(crate) fn publish(&self, runtime: &crate::RuntimePaths) -> Result<(), HibernateErr> {
        crate::disk::atomic::write_temp_then_rename_cache(
            &runtime.live_path("hibernate").join("admission.json"),
            self,
        )?;
        Ok(())
    }

    pub fn read(runtime: &crate::RuntimePaths) -> Self {
        std::fs::read(runtime.live_path("hibernate").join("admission.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_else(|| Self {
                available: false,
                reason: Some("admission unavailable; run rimz start".into()),
            })
    }
}

pub fn admit(config: &HibernateConfig) -> Result<Admission, HibernateErr> {
    admit_with(config, &SystemHost)
}

fn admit_with(config: &HibernateConfig, host: &impl Host) -> Result<Admission, HibernateErr> {
    config.schedule()?;
    let report = inspect_with(config, host);
    Ok(Admission {
        available: report.enabled && report.skipped.is_none(),
        reason: report.skipped,
    })
}

trait Host {
    fn systemd_run(&self) -> bool;
    fn manager(&self) -> bool;
    fn app_slice(&self) -> Result<PathBuf, String>;
    fn read(&self, path: &Path) -> std::io::Result<String>;
    fn writable(&self, path: &Path) -> bool;
    fn root(&self) -> &Path;
}

pub fn inspect(config: &HibernateConfig) -> PreflightReport {
    inspect_with(config, &SystemHost)
}

struct SystemHost;

impl Host for SystemHost {
    fn systemd_run(&self) -> bool {
        which::which("systemd-run").is_ok()
    }
    fn manager(&self) -> bool {
        let mut command = std::process::Command::new("systemctl");
        command.args(["--user", "show-environment"]);
        crate::proc::run_bounded_output(&mut command, std::time::Duration::from_secs(3))
            .is_ok_and(|output| !output.timed_out && output.status.success())
    }
    fn app_slice(&self) -> Result<PathBuf, String> {
        crate::proc::cgroup::user_app_slice().map_err(|error| error.to_string())
    }
    fn read(&self, path: &Path) -> std::io::Result<String> {
        std::fs::read_to_string(path)
    }
    fn writable(&self, path: &Path) -> bool {
        nix::unistd::access(path, nix::unistd::AccessFlags::W_OK).is_ok()
    }
    fn root(&self) -> &Path {
        Path::new("/sys/fs/cgroup")
    }
}

fn inspect_with(config: &HibernateConfig, host: &impl Host) -> PreflightReport {
    let mut report = PreflightReport {
        enabled: config.enabled(),
        skipped: None,
        checks: Vec::new(),
    };
    if let Err(error) = config.schedule() {
        report.skipped = Some(error.to_string());
        report.checks.push(Check {
            name: "schedule",
            status: CheckStatus::Error,
            detail: error.to_string(),
        });
        return report;
    }
    if !cfg!(target_os = "linux") {
        report.skipped = Some("Linux cgroup freezing is unavailable".into());
        return report;
    }
    if !config.enabled() {
        report.skipped = Some("disabled".into());
        return report;
    }
    let mut check = |name, ok: bool, success: &str, failure: &str| {
        if !ok && report.skipped.is_none() {
            report.skipped = Some(failure.into());
        }
        report.checks.push(Check {
            name,
            status: if ok {
                CheckStatus::Ok
            } else {
                CheckStatus::Warning
            },
            detail: if ok { success.into() } else { failure.into() },
        });
    };
    check(
        "systemd-run",
        host.systemd_run(),
        "found",
        "systemd-run is unavailable; install systemd or set agents.hibernate.enabled = false",
    );
    check(
        "user manager",
        host.manager(),
        "reachable",
        "systemd user manager is unreachable; run loginctl enable-linger <user> or log in through a systemd session, or, when XDG_RUNTIME_DIR is unset or overridden (an SSH login, a sandbox), export XDG_RUNTIME_DIR=/run/user/$(id -u)",
    );
    let slice = host.app_slice();
    let memory = slice.as_ref().is_ok_and(|slice| {
        host.writable(slice)
            && host
                .read(&slice.join("cgroup.controllers"))
                .is_ok_and(|controllers| {
                    controllers.split_whitespace().any(|name| name == "memory")
                })
    });
    check(
        "memory controller",
        memory,
        "delegated app.slice is writable",
        "user app.slice is unavailable, unwritable or lacks memory; enable delegation with systemctl edit user@.service and Delegate=memory pids cpu",
    );
    for (name, fix) in [
        (
            "cgroup.freeze",
            "enable writable cgroup-v2 freezer delegation or set agents.hibernate.enabled = false",
        ),
        (
            "memory.reclaim",
            "use Linux 5.19 or newer with writable memory controller delegation or set agents.hibernate.enabled = false",
        ),
    ] {
        check(
            name,
            slice
                .as_ref()
                .is_ok_and(|slice| host.writable(&slice.join(name))),
            "available and writable",
            &format!("{name} is absent or unwritable; {fix}"),
        );
    }
    let swap = host
        .read(Path::new("/proc/meminfo"))
        .ok()
        .and_then(|text| crate::proc::memory::parse_meminfo(&text).ok());
    check(
        "swap",
        swap.is_some_and(|memory| memory.swap_total_bytes > 0),
        "configured",
        "swap is absent or unreadable; add zram, zswap or a swapfile; in a container, allow swap",
    );
    // A namespace can hide tighter ancestors. Only a visible zero is proof of refusal.
    let limit = slice.as_ref().ok().map(|slice| {
        for path in slice
            .ancestors()
            .take_while(|path| path.starts_with(host.root()))
        {
            match host.read(&path.join("memory.swap.max")) {
                Ok(value) if value.trim() == "max" => {}
                Ok(value) if value.trim().parse::<u64>().is_ok_and(|limit| limit > 0) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                _ => return false,
            }
        }
        true
    });
    check(
        "swap limit",
        limit != Some(false),
        "no visible zero limit (not proof of swap availability)",
        "ancestor memory.swap.max is zero or unreadable; raise the ancestor memory.swap.max limit",
    );
    if swap.is_some_and(|memory| memory.swap_total_bytes > 0 && memory.swap_free_bytes == 0) {
        report.checks.push(Check {
            name: "free swap",
            status: CheckStatus::Warning,
            detail: "swap is configured but full; free swap space".into(),
        });
    }
    report
}

pub(crate) fn scope_argv(workspace: &WorkspaceId, kind: &str, argv: &[String]) -> Vec<String> {
    let unit = format!(
        "{}{}.scope",
        scope_prefix(workspace),
        uuid::Uuid::now_v7().simple()
    );
    let mut wrapped: Vec<String> = [
        "systemd-run",
        "--user",
        "--scope",
        "--quiet",
        "--collect",
        "-p",
        "MemoryAccounting=yes",
        "--unit",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    wrapped.extend([
        unit,
        "--description".into(),
        format!("RimZ agent {kind}"),
        "--".into(),
    ]);
    wrapped.extend_from_slice(argv);
    wrapped
}

#[cfg(test)]
mod tests;
