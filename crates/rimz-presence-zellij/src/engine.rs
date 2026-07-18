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
    SwitchObserve { generation: u64, tab: usize },
    SwitchConfirm { generation: u64, tab: usize },
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
    confirmation_requested: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientObservation {
    Detached,
    Unique(ProjectedPaneId),
    Ambiguous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SwitchObservation {
    Work(u32),
    Repairable,
    Pending,
    Abstain,
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
    policy: PokePolicy,
    tabs: BTreeMap<usize, Vec<PaneFields>>,
    last_raw_stable_hash: Option<u64>,
    tab_names: BTreeMap<usize, String>,
    foreground: BTreeMap<u32, String>,
    shell: BTreeMap<u32, String>,
    cwd: BTreeMap<u32, String>,
    pids: BTreeMap<u32, u32>,
    pid_probed: BTreeSet<u32>,
    active_tab: Option<usize>,
    session_focused_pane: Option<u32>,
    switch_generation: u64,
    pending_switch: Option<PendingSwitch>,
    in_flight_clients: Option<InFlightClientQuery>,
    queued_clients: Option<ClientQueryPurpose>,
    stale_client_replies: u32,
    workspace_id: Option<String>,
    session_name: Option<String>,
    rimz_bin: Option<String>,
    plugin_id: Option<u32>,
    plugin_build: Option<String>,
    plugin_config: Option<String>,
    focus_key: Option<String>,
    focus_follows_mouse: Option<bool>,
    mouse_click_through: Option<bool>,
    granted: bool,
    pending_pregrant_change: bool,
    share_requested: bool,
    timer_gate: TimerGate,
    loaded_at_ms: u64,
    connected_clients: Option<usize>,
    client_sample: Option<policy::ClientSample>,
    retired: bool,
    wake_fork_failures: u32,
    stale_writer_rejections: u32,
}

const WAKE_FORK_FALLBACK_THRESHOLD: u32 = 3;
const STALE_WRITER_RETIRE_THRESHOLD: u32 = 3;
impl Engine {
    pub fn new(now: u64, config: EngineConfig) -> Self {
        Self {
            policy: PokePolicy::new(now),
            tabs: BTreeMap::new(),
            last_raw_stable_hash: None,
            tab_names: BTreeMap::new(),
            foreground: BTreeMap::new(),
            shell: BTreeMap::new(),
            cwd: BTreeMap::new(),
            pids: BTreeMap::new(),
            pid_probed: BTreeSet::new(),
            active_tab: None,
            session_focused_pane: None,
            switch_generation: 0,
            pending_switch: None,
            in_flight_clients: None,
            queued_clients: None,
            stale_client_replies: 0,
            workspace_id: config.workspace_id,
            session_name: config.session_name,
            rimz_bin: config.rimz_bin,
            plugin_id: config.plugin_id,
            plugin_build: config.plugin_build,
            plugin_config: config.plugin_config,
            focus_key: config.focus_key,
            focus_follows_mouse: config.focus_follows_mouse,
            mouse_click_through: config.mouse_click_through,
            granted: false,
            pending_pregrant_change: false,
            share_requested: false,
            timer_gate: TimerGate::default(),
            loaded_at_ms: now,
            connected_clients: None,
            client_sample: None,
            retired: false,
            wake_fork_failures: 0,
            stale_writer_rejections: 0,
        }
    }

    pub fn tab_names(&self) -> &BTreeMap<usize, String> {
        &self.tab_names
    }

    pub fn session_name(&self) -> Option<&str> {
        self.session_name.as_deref()
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
        self.queue_client_query(ClientQueryPurpose::General);
        let stable_unchanged = self.last_raw_stable_hash == Some(raw_hash) && !self.tabs.is_empty();
        self.last_raw_stable_hash = Some(raw_hash);
        if !stable_unchanged {
            let projected = project(&self.tab_names);
            // Zellij can deliver partial pane manifests; omitted tabs retain
            // their previous state instead of collapsing the room.
            let mut next_tabs = policy::merged_room(&self.tabs, &projected);
            self.prune_pane_state(&next_tabs);
            self.probe_missing_pids(&mut next_tabs, host);
            let opened = policy::opened_card_panes(&self.tabs, &next_tabs);
            self.tabs = next_tabs;
            // Poke every opened pane — `fold`, not `any`, so a manifest carrying
            // two new panes emits both card-create events.
            let emitted_open = opened.iter().fold(false, |emitted, pane| {
                let command = pane
                    .pane_command
                    .as_ref()
                    .or(pane.terminal_command.as_ref())
                    .filter(|command| !command.is_empty())
                    .cloned();
                self.run_wake(
                    wire::WakeRequest::PaneOpened {
                        pane_id: pane.id,
                        command,
                    },
                    now,
                    &mut effects,
                ) || emitted
            });
            if emitted_open {
                let hash = policy::manifest_hash(&self.tabs);
                self.policy.accept_manifest(hash);
            } else {
                self.fold(now);
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
        let previous_active = self.active_tab;
        self.tab_names = tab_names;
        self.active_tab = active;
        match (previous_active, active) {
            (Some(previous), Some(next)) if previous != next => {
                self.switch_generation = self.switch_generation.wrapping_add(1);
                let generation = self.switch_generation;
                let previous = self
                    .pending_switch
                    .map_or(self.session_focused_pane, |pending| pending.previous);
                self.session_focused_pane = None;
                self.pending_switch = Some(PendingSwitch {
                    generation,
                    tab: next,
                    previous,
                    settle_at: now.saturating_add(policy::FOCUS_SETTLE_MS),
                    confirmation_requested: false,
                });
                self.queue_client_query(ClientQueryPurpose::SwitchObserve {
                    generation,
                    tab: next,
                });
            }
            (_, None) => {
                self.pending_switch = None;
                self.session_focused_pane = None;
                self.queue_client_query(ClientQueryPurpose::General);
            }
            (None, Some(_)) => self.queue_client_query(ClientQueryPurpose::General),
            _ => {}
        }
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
        let mut effects = Vec::new();
        match policy::foreground_command_update(&command, is_foreground) {
            ForegroundCommandUpdate::Remember(command_text) => {
                self.set_foreground_command(pane, Some(command_text));
                if let Some(id) = self.optimistic_command_poke_pane(pane, now)
                    && self.run_wake(
                        wire::WakeRequest::CommandChanged {
                            pane_id: id,
                            args: command,
                        },
                        now,
                        &mut effects,
                    )
                {
                    let hash = policy::manifest_hash(&self.tabs);
                    self.policy.accept_manifest(hash);
                    self.policy.accept_optimistic_pane_poke(id, now);
                } else {
                    self.signal_change(now);
                }
            }
            ForegroundCommandUpdate::Shell(command_text) => {
                let ProjectedPaneId::Terminal(id) = pane else {
                    return self.finish_update(now, host, effects);
                };
                self.foreground.remove(&id);
                self.shell.insert(id, command_text);
                self.refresh_pane(id);
                self.signal_change(now);
            }
            ForegroundCommandUpdate::Forget => {
                self.set_foreground_command(pane, None);
                self.signal_change(now);
            }
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
        let ProjectedPaneId::Terminal(id) = pane else {
            return self.finish_update(now, host, Vec::new());
        };
        let changed = match cwd {
            Some(cwd) if self.cwd.get(&id) != Some(&cwd) => {
                self.cwd.insert(id, cwd);
                true
            }
            Some(_) => false,
            None => self.cwd.remove(&id).is_some(),
        };
        if changed {
            self.refresh_pane(id);
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
        let closed_terminal = match pane {
            ProjectedPaneId::Terminal(id) => Some(id),
            ProjectedPaneId::Plugin(_) => None,
        };
        self.remove_pane(pane);
        if let ProjectedPaneId::Terminal(id) = pane
            && self.session_focused_pane == Some(id)
        {
            self.session_focused_pane = None;
        }
        if !closed_terminal.is_some_and(|pane_id| {
            self.run_wake(wire::WakeRequest::PaneClosed { pane_id }, now, &mut effects)
        }) {
            self.signal_change(now);
        } else {
            let hash = policy::manifest_hash(&self.tabs);
            self.policy.accept_manifest(hash);
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
        if let Some(connected_clients) = connected_clients
            && self.connected_clients != Some(connected_clients)
        {
            self.connected_clients = Some(connected_clients);
            self.queue_client_query(ClientQueryPurpose::General);
        }
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
        let purpose = self.consume_client_reply(now);
        let sample = client_sample(&clients);
        let changed = self.client_sample.as_ref() != Some(&sample);
        self.client_sample = Some(sample);
        let emitted_focus = self.apply_client_observation(purpose, &clients, now, &mut effects);
        if changed && !emitted_focus {
            self.run_wake(wire::WakeRequest::Changed, now, &mut effects);
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
        let mut tabs = std::mem::take(&mut self.tabs);
        self.probe_missing_pids(&mut tabs, host);
        self.tabs = tabs;
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
        if published_topology {
            self.stale_writer_rejections = 0;
        }
        self.wake_fork_failures = self.wake_fork_failures.saturating_add(1);
        if self.wake_fork_failures < WAKE_FORK_FALLBACK_THRESHOLD || self.rimz_bin.is_none() {
            return Vec::new();
        }
        self.rimz_bin = None;
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
        self.drive_client_queries(now, &mut effects);
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
        self.queue_client_query(ClientQueryPurpose::General);
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

    /// Fold the current projected shape into the policy.
    fn fold(&mut self, now: u64) {
        let hash = policy::manifest_hash(&self.tabs);
        self.policy.on_manifest(hash, now);
    }

    fn queue_client_query(&mut self, purpose: ClientQueryPurpose) {
        self.queued_clients = Some(match self.queued_clients {
            None => purpose,
            Some(existing) => latest_client_query(existing, purpose),
        });
    }

    fn drive_client_queries(&mut self, now: u64, effects: &mut Vec<Effect>) {
        if let Some(expired) = self.in_flight_clients.filter(|query| now >= query.deadline) {
            self.in_flight_clients = None;
            self.stale_client_replies = self.stale_client_replies.saturating_add(1);
            self.queue_client_query(expired.purpose);
        }

        if let Some(mut pending) = self.pending_switch
            && self.active_tab == Some(pending.tab)
            && now >= pending.settle_at
            && !pending.confirmation_requested
        {
            pending.confirmation_requested = true;
            self.pending_switch = Some(pending);
            self.queue_client_query(ClientQueryPurpose::SwitchConfirm {
                generation: pending.generation,
                tab: pending.tab,
            });
        }

        if self.in_flight_clients.is_none()
            && let Some(purpose) = self.queued_clients.take()
        {
            self.in_flight_clients = Some(InFlightClientQuery {
                purpose,
                deadline: now.saturating_add(policy::KEEPALIVE_MS),
            });
            effects.push(Effect::ListClients);
        }
    }

    fn consume_client_reply(&mut self, now: u64) -> ClientQueryPurpose {
        if let Some(expired) = self.in_flight_clients.filter(|query| now >= query.deadline) {
            self.in_flight_clients = None;
            self.stale_client_replies = self.stale_client_replies.saturating_add(1);
            self.queue_client_query(expired.purpose);
        }
        if self.stale_client_replies > 0 {
            self.stale_client_replies -= 1;
            if let Some(query) = self.in_flight_clients.take() {
                // The untagged reply may be the expired response or the first
                // response to its replacement. Treat the sample as general,
                // then re-issue the replacement so either ordering converges.
                self.queue_client_query(query.purpose);
            }
            return ClientQueryPurpose::General;
        }
        self.in_flight_clients
            .take()
            .map_or(ClientQueryPurpose::General, |query| query.purpose)
    }

    fn apply_client_observation(
        &mut self,
        purpose: ClientQueryPurpose,
        clients: &[ProjectedClientFocus],
        now: u64,
        effects: &mut Vec<Effect>,
    ) -> bool {
        match purpose {
            ClientQueryPurpose::General => {
                if self.pending_switch.is_some() {
                    return false;
                }
                let current = match unique_client_observation(clients) {
                    ClientObservation::Unique(ProjectedPaneId::Terminal(id))
                        if self.live_terminal(id).is_some() =>
                    {
                        Some(id)
                    }
                    ClientObservation::Detached
                    | ClientObservation::Unique(_)
                    | ClientObservation::Ambiguous => None,
                };
                self.transition_session_focus(current, now, effects)
            }
            ClientQueryPurpose::SwitchObserve { generation, tab }
            | ClientQueryPurpose::SwitchConfirm { generation, tab } => {
                let Some(pending) = self.pending_switch else {
                    return false;
                };
                if pending.generation != generation
                    || pending.tab != tab
                    || self.active_tab != Some(tab)
                {
                    return false;
                }
                let observation = self.classify_switch_observation(tab, clients);
                match (purpose, observation) {
                    (_, SwitchObservation::Work(pane_id)) => {
                        self.pending_switch = None;
                        self.session_focused_pane = Some(pane_id);
                        self.publish_focus_transition(pending.previous, Some(pane_id), now, effects)
                    }
                    (ClientQueryPurpose::SwitchObserve { .. }, SwitchObservation::Repairable) => {
                        false
                    }
                    (ClientQueryPurpose::SwitchObserve { .. }, SwitchObservation::Pending) => false,
                    (ClientQueryPurpose::SwitchObserve { .. }, SwitchObservation::Abstain) => {
                        self.pending_switch = None;
                        false
                    }
                    (ClientQueryPurpose::SwitchConfirm { .. }, SwitchObservation::Repairable) => {
                        self.pending_switch = None;
                        let Some(pane_id) = self.repair_owner(tab) else {
                            return false;
                        };
                        self.run_wake(
                            wire::WakeRequest::FocusStranded {
                                pane_id,
                                generation,
                                clients: clients.to_vec(),
                            },
                            now,
                            effects,
                        )
                    }
                    (ClientQueryPurpose::SwitchConfirm { .. }, SwitchObservation::Pending) => {
                        self.pending_switch = None;
                        false
                    }
                    (ClientQueryPurpose::SwitchConfirm { .. }, SwitchObservation::Abstain) => {
                        self.pending_switch = None;
                        false
                    }
                    (ClientQueryPurpose::General, _) => false,
                }
            }
        }
    }

    fn classify_switch_observation(
        &self,
        tab: usize,
        clients: &[ProjectedClientFocus],
    ) -> SwitchObservation {
        match unique_client_observation(clients) {
            ClientObservation::Detached => SwitchObservation::Pending,
            ClientObservation::Ambiguous => SwitchObservation::Abstain,
            ClientObservation::Unique(ProjectedPaneId::Plugin(_)) => SwitchObservation::Repairable,
            ClientObservation::Unique(ProjectedPaneId::Terminal(id)) => {
                match self.pane_location(id) {
                    Some((pane_tab, pane)) if pane_tab == tab && pane.is_card_pane() => {
                        SwitchObservation::Work(id)
                    }
                    Some((pane_tab, pane)) if pane_tab != tab && pane.is_live_terminal() => {
                        SwitchObservation::Repairable
                    }
                    Some((pane_tab, pane)) if pane_tab == tab && pane.is_sidebar() => {
                        SwitchObservation::Repairable
                    }
                    Some(_) | None => SwitchObservation::Pending,
                }
            }
        }
    }

    fn pane_location(&self, id: u32) -> Option<(usize, &PaneFields)> {
        self.tabs.iter().find_map(|(tab, panes)| {
            panes
                .iter()
                .find(|pane| !pane.is_plugin && pane.id == id)
                .map(|pane| (*tab, pane))
        })
    }

    fn live_terminal(&self, id: u32) -> Option<&PaneFields> {
        self.pane_location(id)
            .map(|(_, pane)| pane)
            .filter(|pane| pane.is_live_terminal())
    }

    fn repair_owner(&self, tab: usize) -> Option<u32> {
        let panes = self.tabs.get(&tab)?;
        let sidebars = panes
            .iter()
            .filter(|pane| pane.is_sidebar())
            .map(|pane| pane.id)
            .collect::<Vec<_>>();
        let has_work = panes.iter().any(PaneFields::is_card_pane);
        matches!(sidebars.as_slice(), [sidebar] if has_work).then(|| sidebars[0])
    }

    fn transition_session_focus(
        &mut self,
        current: Option<u32>,
        now: u64,
        effects: &mut Vec<Effect>,
    ) -> bool {
        let previous = self.session_focused_pane;
        self.session_focused_pane = current;
        self.publish_focus_transition(previous, current, now, effects)
    }

    fn publish_focus_transition(
        &self,
        previous: Option<u32>,
        current: Option<u32>,
        now: u64,
        effects: &mut Vec<Effect>,
    ) -> bool {
        if previous == current {
            return false;
        }
        self.run_wake(
            wire::WakeRequest::FocusChanged { previous, current },
            now,
            effects,
        )
    }

    fn dispatch_due(&mut self, now: u64, host: &impl Host, effects: &mut Vec<Effect>) {
        for poke in self.policy.due(now) {
            if poke == Poke::Alive {
                self.queue_client_query(ClientQueryPurpose::General);
            }
            self.poke(poke, now, host, effects);
        }
    }

    /// Arm one host timer for the policy's next deadline, deduplicated by the
    /// [`TimerGate`]: event bursts arm one timer, an earlier deadline
    /// supersedes, and a superseded timer's fire is a harmless no-op.
    fn rearm(&mut self, now: u64, effects: &mut Vec<Effect>) {
        let policy_at = Some(self.policy.next_wake_at());
        let switch_at = self.pending_switch.map(|pending| pending.settle_at);
        let query_at = self.in_flight_clients.map(|query| query.deadline);
        let Some(at) = [policy_at, switch_at, query_at].into_iter().flatten().min() else {
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
                telemetry.plugin_id = self.plugin_id;
                telemetry.loaded_at_ms = self.loaded_at_ms;
                telemetry.uptime_ms = now.saturating_sub(self.loaded_at_ms);
                wire::WakeRequest::Alive(telemetry)
            }
        };
        self.run_wake(request, now, effects);
    }

    fn wake_context(&self) -> wire::WakeContext<'_> {
        wire::WakeContext {
            rimz_bin: self.rimz_bin.as_deref(),
            workspace_id: self.workspace_id.as_deref(),
            session_name: self.session_name.as_deref(),
        }
    }

    fn run_wake(&self, request: wire::WakeRequest, now: u64, effects: &mut Vec<Effect>) -> bool {
        if !self.granted {
            return false;
        }
        let writer = self.plugin_id.map(|plugin_id| policy::TopologyWriter {
            plugin_id,
            loaded_at_ms: self.loaded_at_ms,
            build: self.plugin_build.clone(),
            config: self.plugin_config.clone(),
        });
        let topology = wire::topology_json(
            self.session_name.as_deref(),
            now,
            writer,
            self.session_focused_pane,
            self.client_sample.as_ref(),
            &self.tabs,
        );
        let Some(argv) = wire::wake_argv(&self.wake_context(), request, topology.as_deref()) else {
            return false;
        };
        effects.push(Effect::RunCommand(argv));
        true
    }

    fn writer(&self) -> policy::TopologyWriter {
        policy::TopologyWriter {
            plugin_id: self.plugin_id.unwrap_or_default(),
            loaded_at_ms: self.loaded_at_ms,
            build: self.plugin_build.clone(),
            config: self.plugin_config.clone(),
        }
    }

    /// Apply rimz-owned runtime config in one `reconfigure(..., false)`.
    fn apply_runtime_reconfigure(&self, effects: &mut Vec<Effect>) {
        let config = wire::RuntimeReconfigure {
            plugin_id: self.plugin_id,
            focus_key: self.focus_key.as_deref(),
            focus_follows_mouse: self.focus_follows_mouse,
            mouse_click_through: self.mouse_click_through,
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

    fn probe_missing_pids(
        &mut self,
        tabs: &mut BTreeMap<usize, Vec<PaneFields>>,
        host: &impl Host,
    ) {
        for id in policy::panes_needing_pid(tabs, &self.pids, &self.pid_probed) {
            self.pid_probed.insert(id);
            if let Some(pid) = host.pane_pid(id) {
                self.pids.insert(id, pid);
            }
        }
        policy::apply_foreground_commands(
            tabs,
            &self.foreground,
            &self.shell,
            &self.cwd,
            &self.pids,
        );
    }

    fn prune_pane_state(&mut self, tabs: &BTreeMap<usize, Vec<PaneFields>>) {
        let pane_ids = tabs
            .values()
            .flatten()
            .filter(|pane| !pane.is_plugin)
            .map(|pane| pane.id)
            .collect::<BTreeSet<_>>();
        self.foreground.retain(|id, _| pane_ids.contains(id));
        self.shell.retain(|id, _| pane_ids.contains(id));
        self.cwd.retain(|id, _| pane_ids.contains(id));
        self.pids.retain(|id, _| pane_ids.contains(id));
        self.pid_probed.retain(|id| pane_ids.contains(id));
    }

    fn set_foreground_command(&mut self, pane_id: ProjectedPaneId, command: Option<String>) {
        let ProjectedPaneId::Terminal(id) = pane_id else {
            return;
        };
        match command.as_ref() {
            Some(command) => {
                self.foreground.insert(id, command.clone());
            }
            None => {
                self.foreground.remove(&id);
            }
        }
        self.refresh_pane(id);
    }

    fn refresh_pane(&mut self, id: u32) {
        let pane_command = self
            .foreground
            .get(&id)
            .or_else(|| self.shell.get(&id))
            .cloned();
        let pane_cwd = self.cwd.get(&id).cloned();
        let pane_pid = self.pids.get(&id).copied();
        for pane in self.tabs.values_mut().flatten() {
            if !pane.is_plugin && pane.id == id {
                pane.pane_command = pane_command.clone();
                pane.pane_cwd = pane_cwd.clone();
                pane.pane_pid = pane_pid;
                return;
            }
        }
    }

    fn optimistic_command_poke_pane(&self, pane_id: ProjectedPaneId, now: u64) -> Option<u32> {
        let ProjectedPaneId::Terminal(id) = pane_id else {
            return None;
        };
        self.policy
            .optimistic_pane_poke_allowed(id, now)
            .then_some(id)
    }

    fn remove_pane(&mut self, pane_id: ProjectedPaneId) {
        let (is_plugin, id) = match pane_id {
            ProjectedPaneId::Terminal(id) => (false, id),
            ProjectedPaneId::Plugin(id) => (true, id),
        };
        policy::remove_pane_from_tabs(&mut self.tabs, is_plugin, id);
        if !is_plugin {
            self.foreground.remove(&id);
            self.shell.remove(&id);
            self.cwd.remove(&id);
            self.pids.remove(&id);
            self.pid_probed.remove(&id);
            self.policy.forget_pane(id);
        }
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
            current @ (ClientQueryPurpose::SwitchObserve {
                generation: current_generation,
                ..
            }
            | ClientQueryPurpose::SwitchConfirm {
                generation: current_generation,
                ..
            }),
            ClientQueryPurpose::SwitchObserve {
                generation: next_generation,
                ..
            }
            | ClientQueryPurpose::SwitchConfirm {
                generation: next_generation,
                ..
            },
        ) if current_generation > next_generation => current,
        (
            current @ ClientQueryPurpose::SwitchConfirm {
                generation: current_generation,
                ..
            },
            ClientQueryPurpose::SwitchObserve {
                generation: next_generation,
                ..
            },
        ) if current_generation == next_generation => current,
        (_, next) => next,
    }
}

fn client_sample(clients: &[ProjectedClientFocus]) -> policy::ClientSample {
    let mut client_ids = BTreeSet::new();
    let mut viewed_panes = BTreeSet::new();
    for client in clients {
        client_ids.insert(client.client_id);
        if let ProjectedPaneId::Terminal(pane_id) = client.pane_id {
            viewed_panes.insert(pane_id);
        }
    }
    policy::ClientSample {
        human_clients: client_ids.len().try_into().unwrap_or(u32::MAX),
        viewed_panes: viewed_panes.into_iter().collect(),
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
