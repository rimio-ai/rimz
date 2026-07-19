//! Host-testable decision engine for the Zellij presence plugin.
//!
//! The wasm shell projects Zellij events into this module, then executes the
//! returned [`Effect`]s. This keeps room state, poke timing, focus correction,
//! permission gating, and topology publication testable on the host target.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use crate::policy::{self, ForegroundCommandUpdate, PaneFields, Poke, PokePolicy, TimerGate};
use crate::wire::{self, PluginTelemetry};

pub use crate::policy::{ClientPaneId as ProjectedPaneId, ClientViewEntry as ProjectedClientFocus};

/// Host lookups the engine needs mid-decision.
pub trait Host {
    fn pane_pid(&self, pane_id: u32) -> Option<u32>;
    fn telemetry(&self) -> PluginTelemetry;
}

/// What the shell must execute, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Fire-and-forget host fork: wake pokes and focus-sidebar. The shell pins
    /// its cwd to `/` so a deleted launch worktree cannot strand the plugin.
    RunCommand(Vec<String>),
    /// Hide the permission-prompt pane left by Zellij.
    HideSelf,
    /// Runtime `reconfigure(kdl, false)`.
    Reconfigure(String),
    /// Admit browser clients to the current Zellij session.
    ShareSession,
    /// Close this plugin instance.
    CloseSelf,
    /// Drop every subscription until a topology dump revives the same-id clone.
    Unsubscribe,
    /// Restore every subscription after a topology dump revives a clone.
    Resubscribe,
    /// Arm one host timer, in milliseconds from now.
    SetTimeout(u64),
    /// Ask Zellij to deliver focused client panes via `Event::ListClients`.
    ListClients,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientQueryPurpose {
    General,
    SwitchSettled { generation: u64, tab: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InFlightClientQuery {
    purpose: ClientQueryPurpose,
    deadline: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingSwitch {
    generation: u64,
    tab: usize,
    previous: Option<u32>,
    settle_at: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum FocusMode {
    #[default]
    Stable,
    Switching(PendingSwitch),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientObservation {
    Detached,
    Unique(ProjectedPaneId),
    Ambiguous,
}

struct SettledSwitch {
    tab: usize,
    generation: u64,
    clients: Vec<ProjectedClientFocus>,
}

#[derive(Default)]
struct FocusUpdate {
    transition: Option<(Option<u32>, Option<u32>)>,
    settled: Option<SettledSwitch>,
    sample_changed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusWork {
    ListClients,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum PanePid {
    #[default]
    Unprobed,
    Missing,
    Known(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct PaneKey {
    is_plugin: bool,
    id: u32,
}

impl From<ProjectedPaneId> for PaneKey {
    fn from(pane: ProjectedPaneId) -> Self {
        match pane {
            ProjectedPaneId::Terminal(id) => Self {
                is_plugin: false,
                id,
            },
            ProjectedPaneId::Plugin(id) => Self {
                is_plugin: true,
                id,
            },
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct PaneState {
    manifest: Option<PaneFields>,
    foreground: Option<String>,
    shell: Option<String>,
    cwd: Option<String>,
    pid: PanePid,
}

#[derive(Debug, Default)]
struct RoomState {
    panes: BTreeMap<PaneKey, PaneState>,
    manifest_applied: bool,
}

impl RoomState {
    fn has_manifest(&self) -> bool {
        self.manifest_applied
    }

    fn apply_manifest(
        &mut self,
        projected: BTreeMap<usize, Vec<PaneFields>>,
        host: &impl Host,
    ) -> bool {
        let before = self.panes.clone();
        self.manifest_applied = true;
        for (tab, panes) in projected {
            if panes.is_empty() {
                continue;
            }
            let reported = panes
                .iter()
                .map(|pane| PaneKey {
                    is_plugin: pane.is_plugin,
                    id: pane.id,
                })
                .collect::<BTreeSet<_>>();
            self.panes
                .retain(|key, state| state.tab() != Some(tab) || reported.contains(key));
            for pane in panes {
                let key = PaneKey {
                    is_plugin: pane.is_plugin,
                    id: pane.id,
                };
                match self.panes.get_mut(&key) {
                    Some(state) => state.apply_manifest(pane),
                    None => {
                        self.panes.insert(key, PaneState::from_manifest(pane));
                    }
                }
            }
        }
        self.probe_missing_pids(host);
        self.panes != before
    }

    fn probe_missing_pids(&mut self, host: &impl Host) {
        let live = self
            .panes
            .iter()
            .filter(|(key, state)| state.is_live_terminal(**key))
            .map(|(key, _)| key.id)
            .collect::<Vec<_>>();
        for id in live {
            let key = PaneKey {
                is_plugin: false,
                id,
            };
            if let Some(state) = self.panes.get_mut(&key)
                && state.pid == PanePid::Unprobed
            {
                state.pid = host.pane_pid(id).map_or(PanePid::Missing, PanePid::Known);
            }
        }
    }

    fn update_foreground(&mut self, pane: ProjectedPaneId, command: Option<String>) -> bool {
        let ProjectedPaneId::Terminal(id) = pane else {
            return false;
        };
        let state = self
            .panes
            .entry(PaneKey::terminal(id))
            .or_insert_with(PaneState::pending);
        if state.foreground == command {
            return false;
        }
        state.foreground = command;
        true
    }

    fn update_shell(&mut self, pane: ProjectedPaneId, command: String) -> bool {
        let ProjectedPaneId::Terminal(id) = pane else {
            return false;
        };
        let state = self
            .panes
            .entry(PaneKey::terminal(id))
            .or_insert_with(PaneState::pending);
        if state.foreground.is_none() && state.shell.as_deref() == Some(&command) {
            return false;
        }
        state.foreground = None;
        state.shell = Some(command);
        true
    }

    fn update_cwd(&mut self, pane: ProjectedPaneId, cwd: Option<String>) -> bool {
        let ProjectedPaneId::Terminal(id) = pane else {
            return false;
        };
        let state = self
            .panes
            .entry(PaneKey::terminal(id))
            .or_insert_with(PaneState::pending);
        if state.cwd == cwd {
            return false;
        }
        state.cwd = cwd;
        true
    }

    fn close_pane(&mut self, pane: ProjectedPaneId) -> bool {
        self.panes.remove(&pane.into()).is_some()
    }

    fn pane_location(&self, id: u32) -> Option<(usize, &PaneState)> {
        self.panes
            .get(&PaneKey::terminal(id))
            .and_then(|pane| pane.tab().map(|tab| (tab, pane)))
    }

    fn live_terminal(&self, id: u32) -> Option<&PaneState> {
        self.pane_location(id).map(|(_, pane)| pane).filter(|pane| {
            pane.is_live_terminal(PaneKey {
                is_plugin: false,
                id,
            })
        })
    }

    fn published_panes(&self) -> Vec<PaneFields> {
        let mut panes = self
            .panes
            .iter()
            .filter_map(|(key, state)| state.tab().map(|tab| (tab, key, state)))
            .collect::<Vec<_>>();
        panes.sort_unstable_by_key(|(tab, key, _)| (*tab, **key));
        panes
            .into_iter()
            .filter_map(|(_, _, state)| state.published())
            .collect()
    }
}

impl PaneKey {
    fn terminal(id: u32) -> Self {
        Self {
            is_plugin: false,
            id,
        }
    }
}

impl PaneState {
    fn from_manifest(mut pane: PaneFields) -> Self {
        let foreground = pane.pane_command.take();
        let cwd = pane.pane_cwd.take();
        let pid = pane
            .pane_pid
            .take()
            .map_or(PanePid::Unprobed, PanePid::Known);
        Self {
            manifest: Some(pane),
            foreground,
            shell: None,
            cwd,
            pid,
        }
    }

    fn pending() -> Self {
        Self::default()
    }

    fn apply_manifest(&mut self, pane: PaneFields) {
        self.manifest = Some(pane);
    }

    fn tab(&self) -> Option<usize> {
        self.manifest
            .as_ref()
            .and_then(|pane| pane.tab_position.try_into().ok())
    }

    fn is_live_terminal(&self, key: PaneKey) -> bool {
        !key.is_plugin
            && self.manifest.as_ref().is_some_and(|pane| {
                !pane.is_suppressed && !pane.is_floating && !pane.exited && !pane.is_held
            })
    }

    fn published(&self) -> Option<PaneFields> {
        let mut pane = self.manifest.clone()?;
        pane.pane_command = self.foreground.as_ref().or(self.shell.as_ref()).cloned();
        pane.pane_cwd.clone_from(&self.cwd);
        pane.pane_pid = match self.pid {
            PanePid::Known(pid) => Some(pid),
            PanePid::Unprobed | PanePid::Missing => None,
        };
        Some(pane)
    }
}

#[derive(Debug, Default)]
struct FocusSync {
    active_tab: Option<usize>,
    session_focused_pane: Option<u32>,
    generation: u64,
    mode: FocusMode,
    in_flight: Option<InFlightClientQuery>,
    queued: Option<ClientQueryPurpose>,
    stale_replies: u32,
    connected_clients: Option<usize>,
    client_sample: Option<policy::ClientSample>,
}

impl FocusSync {
    fn request_general_observation(&mut self) {
        self.queue(ClientQueryPurpose::General);
    }

    fn accept_tab_update(&mut self, active: Option<usize>, now: u64) {
        let previous_active = self.active_tab;
        self.active_tab = active;
        match (previous_active, active) {
            (Some(previous), Some(next)) if previous != next => {
                self.generation = self.generation.wrapping_add(1);
                let previous = match self.mode {
                    FocusMode::Stable => self.session_focused_pane,
                    FocusMode::Switching(pending) => pending.previous,
                };
                self.session_focused_pane = None;
                let pending = PendingSwitch {
                    generation: self.generation,
                    tab: next,
                    previous,
                    settle_at: now.saturating_add(policy::FOCUS_SETTLE_MS),
                };
                self.mode = FocusMode::Switching(pending);
            }
            (_, None) => {
                self.mode = FocusMode::Stable;
                self.session_focused_pane = None;
                self.request_general_observation();
            }
            (None, Some(_)) => self.request_general_observation(),
            _ => {}
        }
    }

    fn accept_connected_clients(&mut self, connected_clients: Option<usize>) {
        if let Some(connected_clients) = connected_clients
            && self.connected_clients != Some(connected_clients)
        {
            self.connected_clients = Some(connected_clients);
            self.request_general_observation();
        }
    }

    fn close_pane(&mut self, pane: ProjectedPaneId) {
        if let ProjectedPaneId::Terminal(id) = pane
            && self.session_focused_pane == Some(id)
        {
            self.session_focused_pane = None;
        }
    }

    fn accept_client_sample(
        &mut self,
        room: &RoomState,
        clients: Vec<ProjectedClientFocus>,
        now: u64,
    ) -> FocusUpdate {
        let purpose = self.consume_reply(now);
        let sample = client_sample(&clients);
        let sample_changed = self.client_sample.as_ref() != Some(&sample);
        self.client_sample = Some(sample);
        let mut update = self.apply_observation(room, purpose, clients);
        update.sample_changed = sample_changed;
        update
    }

    fn drive_due_query(&mut self, now: u64) -> Option<FocusWork> {
        self.expire_query(now);
        if let FocusMode::Switching(pending) = self.mode
            && self.active_tab == Some(pending.tab)
            && now >= pending.settle_at
        {
            let purpose = ClientQueryPurpose::SwitchSettled {
                generation: pending.generation,
                tab: pending.tab,
            };
            if !self.has_query(purpose) {
                self.queue(purpose);
            }
        }
        if self.in_flight.is_some() {
            return None;
        }
        let purpose = self.queued.take()?;
        self.in_flight = Some(InFlightClientQuery {
            purpose,
            deadline: now.saturating_add(policy::KEEPALIVE_MS),
        });
        Some(FocusWork::ListClients)
    }

    fn next_deadline(&self) -> Option<u64> {
        [
            match self.mode {
                FocusMode::Stable => None,
                FocusMode::Switching(pending)
                    if !self.has_query(ClientQueryPurpose::SwitchSettled {
                        generation: pending.generation,
                        tab: pending.tab,
                    }) =>
                {
                    Some(pending.settle_at)
                }
                FocusMode::Switching(_) => None,
            },
            self.in_flight.map(|query| query.deadline),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn session_focus(&self) -> Option<u32> {
        self.session_focused_pane
    }

    fn client_sample(&self) -> Option<&policy::ClientSample> {
        self.client_sample.as_ref()
    }

    fn queue(&mut self, purpose: ClientQueryPurpose) {
        self.queued = Some(match self.queued {
            None => purpose,
            Some(existing) => latest_client_query(existing, purpose),
        });
    }

    fn has_query(&self, purpose: ClientQueryPurpose) -> bool {
        self.queued == Some(purpose) || self.in_flight.is_some_and(|query| query.purpose == purpose)
    }

    fn expire_query(&mut self, now: u64) {
        if let Some(expired) = self.in_flight.filter(|query| now >= query.deadline) {
            self.in_flight = None;
            self.stale_replies = self.stale_replies.saturating_add(1);
            self.queue(expired.purpose);
        }
    }

    fn consume_reply(&mut self, now: u64) -> ClientQueryPurpose {
        self.expire_query(now);
        if self.stale_replies > 0 {
            self.stale_replies -= 1;
            if let Some(query) = self.in_flight.take() {
                // Untagged replies are general evidence; reissue the current
                // query so either response ordering converges.
                self.queue(query.purpose);
            }
            return ClientQueryPurpose::General;
        }
        self.in_flight
            .take()
            .map_or(ClientQueryPurpose::General, |query| query.purpose)
    }

    fn apply_observation(
        &mut self,
        room: &RoomState,
        purpose: ClientQueryPurpose,
        clients: Vec<ProjectedClientFocus>,
    ) -> FocusUpdate {
        match purpose {
            ClientQueryPurpose::General => {
                if matches!(self.mode, FocusMode::Switching(_)) {
                    return FocusUpdate::default();
                }
                FocusUpdate {
                    transition: self.transition(observed_focus(room, &clients)),
                    ..FocusUpdate::default()
                }
            }
            ClientQueryPurpose::SwitchSettled { generation, tab } => {
                let FocusMode::Switching(pending) = self.mode else {
                    return FocusUpdate::default();
                };
                if pending.generation != generation
                    || pending.tab != tab
                    || self.active_tab != Some(tab)
                {
                    return FocusUpdate::default();
                }
                let current = observed_focus(room, &clients);
                self.mode = FocusMode::Stable;
                self.session_focused_pane = current;
                FocusUpdate {
                    transition: transition_outcome(pending.previous, current),
                    settled: Some(SettledSwitch {
                        tab,
                        generation,
                        clients,
                    }),
                    sample_changed: false,
                }
            }
        }
    }

    fn transition(&mut self, current: Option<u32>) -> Option<(Option<u32>, Option<u32>)> {
        let previous = self.session_focused_pane;
        self.session_focused_pane = current;
        transition_outcome(previous, current)
    }
}

fn transition_outcome(
    previous: Option<u32>,
    current: Option<u32>,
) -> Option<(Option<u32>, Option<u32>)> {
    (previous != current).then_some((previous, current))
}

fn observed_focus(room: &RoomState, clients: &[ProjectedClientFocus]) -> Option<u32> {
    match unique_client_observation(clients) {
        ClientObservation::Unique(ProjectedPaneId::Terminal(id))
            if room.live_terminal(id).is_some() =>
        {
            Some(id)
        }
        ClientObservation::Detached
        | ClientObservation::Unique(_)
        | ClientObservation::Ambiguous => None,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EngineConfig {
    pub workspace_id: Option<String>,
    pub session_name: Option<String>,
    pub rimz_bin: Option<String>,
    pub plugin_id: Option<u32>,
    pub plugin_build: Option<String>,
    pub plugin_config: Option<String>,
    pub focus_key: Option<String>,
    pub focus_follows_mouse: Option<bool>,
    pub mouse_click_through: Option<bool>,
}

pub struct Engine {
    config: EngineConfig,
    policy: PokePolicy,
    room: RoomState,
    last_raw_stable_hash: Option<u64>,
    tab_names: BTreeMap<usize, String>,
    focus: FocusSync,
    granted: bool,
    pending_pregrant_change: bool,
    share_requested: bool,
    timer_gate: TimerGate,
    loaded_at_ms: u64,
    retired: bool,
    wake_fork_failures: u32,
    stale_writer_rejections: u32,
}

const WAKE_FORK_FALLBACK_THRESHOLD: u32 = 3;
const STALE_WRITER_RETIRE_THRESHOLD: u32 = 3;
impl Engine {
    pub fn new(now: u64, config: EngineConfig) -> Self {
        Self {
            config,
            policy: PokePolicy::new(now),
            room: RoomState::default(),
            last_raw_stable_hash: None,
            tab_names: BTreeMap::new(),
            focus: FocusSync::default(),
            granted: false,
            pending_pregrant_change: false,
            share_requested: false,
            timer_gate: TimerGate::default(),
            loaded_at_ms: now,
            retired: false,
            wake_fork_failures: 0,
            stale_writer_rejections: 0,
        }
    }

    pub fn tab_names(&self) -> &BTreeMap<usize, String> {
        &self.tab_names
    }

    pub fn session_name(&self) -> Option<&str> {
        self.config.session_name.as_deref()
    }

    pub fn on_load(&mut self, now: u64, host: &impl Host) -> Vec<Effect> {
        if self.retired {
            return Vec::new();
        }
        self.finish_update(now, host, Vec::new())
    }

    pub fn on_permission_granted(&mut self, now: u64, host: &impl Host) -> Vec<Effect> {
        if self.retired {
            return Vec::new();
        }
        let mut effects = Vec::new();
        self.mark_granted(now, host, &mut effects);
        self.apply_runtime_reconfigure(&mut effects);
        if self.share_requested {
            effects.push(Effect::ShareSession);
        }
        self.finish_update(now, host, effects)
    }

    pub fn on_permission_denied(&mut self, now: u64, host: &impl Host) -> Vec<Effect> {
        if self.retired {
            return Vec::new();
        }
        self.granted = false;
        self.finish_update(now, host, Vec::new())
    }

    pub fn on_pane_manifest(
        &mut self,
        raw_hash: u64,
        project: impl FnOnce(&BTreeMap<usize, String>) -> BTreeMap<usize, Vec<PaneFields>>,
        now: u64,
        host: &impl Host,
    ) -> Vec<Effect> {
        if self.retired {
            return Vec::new();
        }
        let mut effects = Vec::new();
        // Application state flowing proves the (possibly cached) grant covers
        // us: Zellij sends no PermissionRequestResult when the grant comes from
        // the permission cache, so this path is load-bearing.
        self.mark_granted(now, host, &mut effects);
        // PaneUpdate is Zellij's only prompt signal for some view-local focus
        // changes. The raw focus bit is deliberately absent from the topology
        // hash; refresh attached-client truth even when the roster is stable.
        self.focus.request_general_observation();
        let stable_unchanged =
            self.last_raw_stable_hash == Some(raw_hash) && self.room.has_manifest();
        self.last_raw_stable_hash = Some(raw_hash);
        if !stable_unchanged {
            let projected = project(&self.tab_names);
            let baseline = !self.room.has_manifest();
            if self.room.apply_manifest(projected, host) && !baseline {
                self.signal_change(now);
            }
        }
        self.finish_update(now, host, effects)
    }

    pub fn on_tab_update(
        &mut self,
        active: Option<usize>,
        tab_names: BTreeMap<usize, String>,
        now: u64,
        host: &impl Host,
    ) -> Vec<Effect> {
        if self.retired {
            return Vec::new();
        }
        let mut effects = Vec::new();
        self.mark_granted(now, host, &mut effects);
        self.tab_names = tab_names;
        self.focus.accept_tab_update(active, now);
        self.finish_update(now, host, effects)
    }

    pub fn on_command_changed(
        &mut self,
        pane: ProjectedPaneId,
        command: Vec<String>,
        is_foreground: bool,
        now: u64,
        host: &impl Host,
    ) -> Vec<Effect> {
        if self.retired {
            return Vec::new();
        }
        let effects = Vec::new();
        let changed = match policy::foreground_command_update(&command, is_foreground) {
            ForegroundCommandUpdate::Remember(command_text) => {
                self.room.update_foreground(pane, Some(command_text))
            }
            ForegroundCommandUpdate::Shell(command_text) => {
                self.room.update_shell(pane, command_text)
            }
            ForegroundCommandUpdate::Forget => self.room.update_foreground(pane, None),
        };
        if changed {
            self.signal_change(now);
        }
        self.finish_update(now, host, effects)
    }

    pub fn on_cwd_changed(
        &mut self,
        pane: ProjectedPaneId,
        cwd: Option<String>,
        now: u64,
        host: &impl Host,
    ) -> Vec<Effect> {
        if self.retired {
            return Vec::new();
        }
        let ProjectedPaneId::Terminal(_) = pane else {
            return self.finish_update(now, host, Vec::new());
        };
        let changed = self.room.update_cwd(pane, cwd);
        if changed {
            self.signal_change(now);
        }
        self.finish_update(now, host, Vec::new())
    }

    pub fn on_pane_closed(
        &mut self,
        pane: ProjectedPaneId,
        now: u64,
        host: &impl Host,
    ) -> Vec<Effect> {
        if self.retired {
            return Vec::new();
        }
        let mut effects = Vec::new();
        self.mark_granted(now, host, &mut effects);
        let changed = self.room.close_pane(pane);
        self.focus.close_pane(pane);
        if changed {
            // A close followed by same-shaped pane ID reuse has the same raw hash;
            // force the replacement manifest through the canonical reducer.
            self.last_raw_stable_hash = None;
            self.signal_change(now);
        }
        self.finish_update(now, host, effects)
    }

    pub fn on_timer(&mut self, now: u64, host: &impl Host) -> Vec<Effect> {
        if self.retired {
            return Vec::new();
        }
        self.timer_gate.on_fire(now);
        self.finish_update(now, host, Vec::new())
    }

    pub fn on_session_update(
        &mut self,
        connected_clients: Option<usize>,
        now: u64,
        host: &impl Host,
    ) -> Vec<Effect> {
        if self.retired {
            return Vec::new();
        }
        let mut effects = Vec::new();
        self.mark_granted(now, host, &mut effects);
        self.focus.accept_connected_clients(connected_clients);
        self.finish_update(now, host, effects)
    }

    pub fn on_list_clients(
        &mut self,
        clients: Vec<ProjectedClientFocus>,
        now: u64,
        host: &impl Host,
    ) -> Vec<Effect> {
        if self.retired {
            return Vec::new();
        }
        let mut effects = Vec::new();
        let update = self.focus.accept_client_sample(&self.room, clients, now);
        let changed = update.sample_changed || update.transition.is_some();
        if let Some(settled) = update.settled {
            self.run_wake(
                wire::WakeRequest::SwitchSettled {
                    tab: settled.tab as u64,
                    generation: settled.generation,
                    clients: settled.clients,
                },
                now,
                &mut effects,
            );
        }
        if changed {
            self.signal_change(now);
        }
        self.finish_update(now, host, effects)
    }

    pub fn on_focus_sidebar_pipe(&mut self) -> Vec<Effect> {
        if self.retired {
            return Vec::new();
        }
        let mut effects = Vec::new();
        self.run_focus_sidebar(&mut effects);
        effects
    }

    pub fn on_share_session_pipe(&mut self) -> Vec<Effect> {
        if self.retired {
            return Vec::new();
        }
        self.share_requested = true;
        // This can be dropped while the upgraded StartWebServer grant is
        // pending; PermissionRequestResult(Granted) replays it.
        vec![Effect::ShareSession]
    }

    pub fn on_dump_topology_pipe(&mut self, now: u64, host: &impl Host) -> Vec<Effect> {
        let mut effects = Vec::new();
        if self.retired {
            self.retired = false;
            self.last_raw_stable_hash = None;
            effects.push(Effect::Resubscribe);
        }
        if !self.granted {
            self.pending_pregrant_change = true;
            return effects;
        }
        self.room.probe_missing_pids(host);
        self.poke(Poke::Alive, now, host, &mut effects);
        self.rearm(now, &mut effects);
        effects
    }

    pub fn on_retire_pipe(&mut self, payload: Option<&str>) -> Vec<Effect> {
        if self.retired {
            return Vec::new();
        }
        let Some(generation) = wire::retire_generation(payload) else {
            return Vec::new();
        };
        let own = self.writer();
        if let (Some(build), Some(config)) = (&generation.build, &generation.config)
            && (own.build.as_ref() != Some(build) || own.config.as_ref() != Some(config))
        {
            return vec![Effect::CloseSelf];
        }
        if (own.loaded_at_ms, own.plugin_id) >= (generation.loaded_at_ms, generation.plugin_id) {
            return Vec::new();
        }
        if own.plugin_id == generation.plugin_id {
            self.retired = true;
            vec![Effect::Unsubscribe]
        } else {
            vec![Effect::CloseSelf]
        }
    }

    pub fn on_run_command_result(
        &mut self,
        exit_code: Option<i32>,
        published_topology: bool,
        now: u64,
        host: &impl Host,
    ) -> Vec<Effect> {
        if self.retired {
            return Vec::new();
        }
        if published_topology && exit_code == Some(wire::STALE_WRITER_EXIT_CODE) {
            self.stale_writer_rejections = self.stale_writer_rejections.saturating_add(1);
            if self.stale_writer_rejections >= STALE_WRITER_RETIRE_THRESHOLD {
                self.retired = true;
                // `close_self()` unloads the accepted same-id clone too; this
                // retired clone stays revivable through `rimz:dump_topology`.
                return vec![Effect::Unsubscribe];
            }
            return Vec::new();
        }
        if exit_code == Some(0) {
            self.wake_fork_failures = 0;
            if published_topology {
                self.stale_writer_rejections = 0;
            }
            return Vec::new();
        }
        self.wake_fork_failures = self.wake_fork_failures.saturating_add(1);
        if self.wake_fork_failures < WAKE_FORK_FALLBACK_THRESHOLD || self.config.rimz_bin.is_none()
        {
            return Vec::new();
        }
        self.config.rimz_bin = None;
        self.wake_fork_failures = 0;
        let mut effects = Vec::new();
        self.poke(Poke::Alive, now, host, &mut effects);
        effects
    }

    fn finish_update(
        &mut self,
        now: u64,
        host: &impl Host,
        mut effects: Vec<Effect>,
    ) -> Vec<Effect> {
        self.dispatch_due(now, host, &mut effects);
        if self.focus.drive_due_query(now) == Some(FocusWork::ListClients) {
            effects.push(Effect::ListClients);
        }
        self.rearm(now, &mut effects);
        effects
    }

    /// Flip into granted mode once: poke an immediate keepalive so the producer
    /// enters event mode now rather than after the first cadence, unless a
    /// pre-grant topology signal is already waiting. Hide the pane Zellij
    /// surfaced for the permission prompt; the plugin is headless, so a visible
    /// pane is only ever that prompt's leftover.
    fn mark_granted(&mut self, now: u64, host: &impl Host, effects: &mut Vec<Effect>) {
        if self.granted {
            self.flush_pregrant_change(now);
            return;
        }
        self.granted = true;
        effects.push(Effect::HideSelf);
        self.focus.request_general_observation();
        self.apply_runtime_reconfigure(effects);
        if self.pending_pregrant_change {
            self.flush_pregrant_change(now);
        } else {
            self.poke(Poke::Alive, now, host, effects);
        }
    }

    fn signal_change(&mut self, now: u64) {
        if !self.granted {
            self.pending_pregrant_change = true;
            return;
        }
        self.policy.on_signal(now);
    }

    fn flush_pregrant_change(&mut self, now: u64) {
        if self.pending_pregrant_change {
            self.pending_pregrant_change = false;
            self.policy.on_signal(now);
        }
    }

    fn dispatch_due(&mut self, now: u64, host: &impl Host, effects: &mut Vec<Effect>) {
        for poke in self.policy.due(now) {
            if poke == Poke::Alive {
                self.focus.request_general_observation();
            }
            self.poke(poke, now, host, effects);
        }
    }

    /// Arm one host timer for the policy's next deadline, deduplicated by the
    /// [`TimerGate`]: event bursts arm one timer, an earlier deadline
    /// supersedes, and a superseded timer's fire is a harmless no-op.
    fn rearm(&mut self, now: u64, effects: &mut Vec<Effect>) {
        let policy_at = Some(self.policy.next_wake_at());
        let Some(at) = [policy_at, self.focus.next_deadline()]
            .into_iter()
            .flatten()
            .min()
        else {
            return;
        };
        if self.timer_gate.should_arm(at) {
            effects.push(Effect::SetTimeout(at.saturating_sub(now)));
        }
    }

    /// Dispatch a policy poke through the wire argv builder. Fire-and-forget:
    /// a failed wake means no stamp, and the producer degrades to poll mode on
    /// its own.
    fn poke(&self, poke: Poke, now: u64, host: &impl Host, effects: &mut Vec<Effect>) {
        if !self.granted {
            return;
        }
        let request = match poke {
            Poke::Changed => wire::WakeRequest::Changed,
            Poke::Alive => {
                let mut telemetry = host.telemetry();
                telemetry.plugin_id = self.config.plugin_id;
                telemetry.plugin_build = self.config.plugin_build.clone();
                telemetry.loaded_at_ms = self.loaded_at_ms;
                telemetry.uptime_ms = now.saturating_sub(self.loaded_at_ms);
                wire::WakeRequest::Alive(telemetry)
            }
        };
        self.run_wake(request, now, effects);
    }

    fn wake_context(&self) -> wire::WakeContext<'_> {
        wire::WakeContext {
            rimz_bin: self.config.rimz_bin.as_deref(),
            workspace_id: self.config.workspace_id.as_deref(),
            session_name: self.config.session_name.as_deref(),
        }
    }

    fn run_wake(&self, request: wire::WakeRequest, now: u64, effects: &mut Vec<Effect>) -> bool {
        if !self.granted {
            return false;
        }
        let writer = self
            .config
            .plugin_id
            .map(|plugin_id| policy::TopologyWriter {
                plugin_id,
                loaded_at_ms: self.loaded_at_ms,
                build: self.config.plugin_build.clone(),
                config: self.config.plugin_config.clone(),
            });
        let panes = self.room.published_panes();
        let topology = wire::topology_json(
            self.config.session_name.as_deref(),
            now,
            writer,
            self.focus.session_focus(),
            self.focus.client_sample(),
            &panes,
        );
        let Some(argv) = wire::wake_argv(&self.wake_context(), request, topology.as_deref()) else {
            return false;
        };
        effects.push(Effect::RunCommand(argv));
        true
    }

    fn writer(&self) -> policy::TopologyWriter {
        policy::TopologyWriter {
            plugin_id: self.config.plugin_id.unwrap_or_default(),
            loaded_at_ms: self.loaded_at_ms,
            build: self.config.plugin_build.clone(),
            config: self.config.plugin_config.clone(),
        }
    }

    /// Apply rimz-owned runtime config in one `reconfigure(..., false)`.
    fn apply_runtime_reconfigure(&self, effects: &mut Vec<Effect>) {
        let config = wire::RuntimeReconfigure {
            plugin_id: self.config.plugin_id,
            focus_key: self.config.focus_key.as_deref(),
            focus_follows_mouse: self.config.focus_follows_mouse,
            mouse_click_through: self.config.mouse_click_through,
        };
        if let Some(kdl) = wire::runtime_reconfigure_kdl(&config) {
            effects.push(Effect::Reconfigure(kdl));
        }
    }

    /// Run `rimz sidebar focus --toggle` for the focus-key pipe.
    fn run_focus_sidebar(&self, effects: &mut Vec<Effect>) {
        if !self.granted {
            return;
        }
        effects.push(Effect::RunCommand(wire::focus_sidebar_argv(
            &self.wake_context(),
        )));
    }
}

fn latest_client_query(
    current: ClientQueryPurpose,
    next: ClientQueryPurpose,
) -> ClientQueryPurpose {
    match (current, next) {
        (current, ClientQueryPurpose::General) => current,
        (ClientQueryPurpose::General, next) => next,
        (
            current @ ClientQueryPurpose::SwitchSettled {
                generation: current_generation,
                ..
            },
            ClientQueryPurpose::SwitchSettled {
                generation: next_generation,
                ..
            },
        ) if current_generation > next_generation => current,
        (_, next) => next,
    }
}

fn client_sample(clients: &[ProjectedClientFocus]) -> policy::ClientSample {
    policy::ClientSample {
        views: clients
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
    }
}

fn unique_client_observation(clients: &[ProjectedClientFocus]) -> ClientObservation {
    let panes = clients
        .iter()
        .map(|client| client.pane_id)
        .collect::<BTreeSet<_>>();
    match panes.iter().copied().collect::<Vec<_>>().as_slice() {
        [] => ClientObservation::Detached,
        [pane] => ClientObservation::Unique(*pane),
        _ => ClientObservation::Ambiguous,
    }
}

#[cfg(test)]
mod tests;
