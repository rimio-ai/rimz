//! Host-testable decision engine for the Zellij presence plugin.
//!
//! The wasm shell projects Zellij events into this module, then executes the
//! returned [`Effect`]s. This keeps room state, poke timing, focus correction,
//! permission gating, and topology publication testable on the host target.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use crate::policy::{
    self, CorrectionAction, FocusCorrection, FocusPatch, ForegroundCommandUpdate, PaneBaseline,
    PaneFields, Poke, PokePolicy, TimerGate,
};
use crate::wire::{self, PluginTelemetry};

/// Host lookups the engine needs mid-decision.
pub trait Host {
    fn baseline(&self, pane_id: u32) -> Option<PaneBaseline>;
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
pub enum ProjectedPaneId {
    Terminal(u32),
    Plugin(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectedClientFocus {
    pub client_id: u16,
    pub pane_id: u32,
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
    baseline: BTreeMap<u32, PaneBaseline>,
    baseline_attempts: BTreeMap<u32, u8>,
    next_baseline_probe_at: Option<u64>,
    active_tab: Option<usize>,
    active_focused_pane: Option<u32>,
    session_focused_pane: Option<u32>,
    focus_correction: FocusCorrection,
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
    contested_focused_by_tab: BTreeMap<usize, Vec<u32>>,
    prior_focused_by_tab: BTreeMap<usize, u32>,
    retired: bool,
    wake_fork_failures: u32,
    stale_writer_rejections: u32,
}

const WAKE_FORK_FALLBACK_THRESHOLD: u32 = 3;
const STALE_WRITER_RETIRE_THRESHOLD: u32 = 3;
const BASELINE_PROBE_BATCH: usize = 8;
const BASELINE_PROBE_MAX_ATTEMPTS: u8 = 3;
const BASELINE_PROBE_RETRY_MS: u64 = 2_000;

impl Engine {
    pub fn new(now: u64, config: EngineConfig) -> Self {
        Self {
            policy: PokePolicy::new(now),
            tabs: BTreeMap::new(),
            last_raw_stable_hash: None,
            tab_names: BTreeMap::new(),
            foreground: BTreeMap::new(),
            baseline: BTreeMap::new(),
            baseline_attempts: BTreeMap::new(),
            next_baseline_probe_at: None,
            active_tab: None,
            active_focused_pane: None,
            session_focused_pane: None,
            focus_correction: FocusCorrection::default(),
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
            contested_focused_by_tab: BTreeMap::new(),
            prior_focused_by_tab: BTreeMap::new(),
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
        let stable_unchanged = self.last_raw_stable_hash == Some(raw_hash) && !self.tabs.is_empty();
        self.last_raw_stable_hash = Some(raw_hash);
        if !stable_unchanged {
            let projected = project(&self.tab_names);
            self.record_contested_focus(&projected);
            // Zellij can deliver partial pane manifests; omitted tabs retain
            // their previous state instead of collapsing the room.
            let mut next_tabs = policy::merged_room(&self.tabs, &projected);
            self.prune_baseline_state(&next_tabs);
            policy::apply_foreground_commands(&mut next_tabs, &self.foreground, &self.baseline);
            self.repair_contested_focus(&mut next_tabs);
            let opened = policy::opened_card_panes(&self.tabs, &next_tabs);
            let focus_patch = policy::focus_shortcut_if_only_focus_changed(&self.tabs, &next_tabs);
            let shortcut_focused = focus_patch
                .as_ref()
                .and_then(|patch| focused_patch_id(patch));
            if let Some(focused) = shortcut_focused {
                self.session_focused_pane = Some(focused);
            }
            self.tabs = next_tabs;
            self.schedule_baseline_probe(now);
            let pending_correction_tab = self.focus_correction.pending_tab();
            let correction_will_resolve_active_tab =
                pending_correction_tab.is_some() && pending_correction_tab == self.active_tab;
            let reconciled_focus_patch = (!correction_will_resolve_active_tab
                && shortcut_focused.is_none())
            .then(|| self.reconcile_manifest_focus_patch())
            .flatten();
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
            let focus_patch = focus_patch.or(reconciled_focus_patch);
            let emitted_focus = focus_patch.is_some_and(|patch| {
                self.run_wake(wire::WakeRequest::FocusChanged { patch }, now, &mut effects)
            });
            if emitted_focus {
                let hash = policy::manifest_hash(&self.tabs);
                self.policy.accept_manifest(hash);
                self.policy.on_optimistic_signal(now);
            } else if emitted_open {
                let hash = policy::manifest_hash(&self.tabs);
                self.policy.accept_manifest(hash);
            } else {
                self.fold(now);
            }
            self.resolve_focus_correction(now, true, &mut effects);
            self.active_focused_pane = policy::resolved_focused_pane_id(
                &self.tabs,
                self.active_tab,
                self.session_focused_pane,
            );
        } else {
            self.schedule_baseline_probe(now);
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
        let previous_focused_pane = self.active_focused_pane.or_else(|| {
            policy::resolved_focused_pane_id(&self.tabs, previous_active, self.session_focused_pane)
        });
        self.tab_names = tab_names;
        self.active_tab = active;
        if previous_active != active {
            // A tab switch changes which panes attached clients view, but
            // Zellij sends no client-list event for it. Re-query so the fresh
            // sample wakes the newly viewed tab's sidebar before keepalive.
            queue_list_clients(&mut effects);
        }
        self.active_focused_pane = policy::resolved_focused_pane_id(
            &self.tabs,
            self.active_tab,
            self.session_focused_pane,
        );
        self.focus_correction.on_active_tab_change(
            previous_active,
            active,
            previous_focused_pane,
            now,
        );
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
                self.set_foreground_command(pane, Some(command_text), now, host);
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
            ForegroundCommandUpdate::Forget => {
                self.set_foreground_command(pane, None, now, host);
                self.signal_change(now);
            }
        }
        self.finish_update(now, host, effects)
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
        self.schedule_baseline_probe(now);
        if let ProjectedPaneId::Terminal(id) = pane
            && self.session_focused_pane == Some(id)
        {
            self.session_focused_pane = None;
        }
        self.active_focused_pane = policy::resolved_focused_pane_id(
            &self.tabs,
            self.active_tab,
            self.session_focused_pane,
        );
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
        self.dispatch_due_baseline_probe(now, host);
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
            queue_list_clients(&mut effects);
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
        let sample = client_sample(clients);
        let changed = self.client_sample.as_ref() != Some(&sample);
        self.client_sample = Some(sample);
        if changed {
            self.apply_client_focus_to_recorded_contests();
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
        self.schedule_baseline_probe(now);
        let mut tabs = std::mem::take(&mut self.tabs);
        self.repair_contested_focus(&mut tabs);
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
        let own = policy::TopologyWriter {
            plugin_id: self.plugin_id.unwrap_or_default(),
            loaded_at_ms: self.loaded_at_ms,
            build: self.plugin_build.clone(),
            config: self.plugin_config.clone(),
        };
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

    fn repair_contested_focus(&mut self, tabs: &mut BTreeMap<usize, Vec<PaneFields>>) {
        let repaired_active_tab = self.active_tab.filter(|active_tab| {
            tabs.get(active_tab).is_some_and(|panes| {
                panes
                    .iter()
                    .filter(|pane| pane.is_focused && !pane.is_floating)
                    .count()
                    > 1
            })
        });
        let viewed = self
            .client_sample
            .as_ref()
            .map(|sample| sample.viewed_panes.as_slice())
            .unwrap_or_default();
        policy::repair_contested_tab_focus(tabs, viewed, &self.prior_focused_by_tab);
        self.prior_focused_by_tab = tabs
            .iter()
            .filter_map(|(tab, panes)| {
                panes
                    .iter()
                    .find(|pane| pane.is_focused && !pane.is_floating)
                    .map(|pane| (*tab, pane.id))
            })
            .collect();
        if let Some(active_tab) = repaired_active_tab
            && let Some(focused) = self.prior_focused_by_tab.get(&active_tab).copied()
        {
            self.session_focused_pane = Some(focused);
        }
    }

    fn record_contested_focus(&mut self, projected: &BTreeMap<usize, Vec<PaneFields>>) {
        for (tab, panes) in projected.iter().filter(|(_, panes)| !panes.is_empty()) {
            let focused = panes
                .iter()
                .filter(|pane| pane.is_focused && !pane.is_floating)
                .map(|pane| pane.id)
                .collect::<Vec<_>>();
            if focused.len() > 1 {
                self.contested_focused_by_tab.insert(*tab, focused);
            } else {
                self.contested_focused_by_tab.remove(tab);
            }
        }
    }

    fn apply_client_focus_to_recorded_contests(&mut self) {
        let Some(sample) = self.client_sample.as_ref() else {
            return;
        };
        let viewed = sample.viewed_panes.iter().copied().collect::<BTreeSet<_>>();
        let mut repaired_active = None;
        for (tab, contenders) in &self.contested_focused_by_tab {
            let Some(keep) = contenders
                .iter()
                .copied()
                .filter(|pane_id| viewed.contains(pane_id))
                .filter(|pane_id| {
                    self.tabs.get(tab).is_some_and(|panes| {
                        panes
                            .iter()
                            .any(|pane| pane.id == *pane_id && !pane.is_floating)
                    })
                })
                .min()
            else {
                continue;
            };
            if let Some(panes) = self.tabs.get_mut(tab) {
                for pane in panes {
                    if !pane.is_floating {
                        pane.is_focused = pane.id == keep;
                    }
                }
            }
            self.prior_focused_by_tab.insert(*tab, keep);
            if self.active_tab == Some(*tab) {
                repaired_active = Some(keep);
            }
        }
        if let Some(focused) = repaired_active {
            self.session_focused_pane = Some(focused);
        }
    }

    fn finish_update(
        &mut self,
        now: u64,
        host: &impl Host,
        mut effects: Vec<Effect>,
    ) -> Vec<Effect> {
        self.dispatch_due(now, host, &mut effects);
        self.resolve_focus_correction(now, false, &mut effects);
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
        queue_list_clients(effects);
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

    fn reconcile_manifest_focus_patch(&mut self) -> Option<Vec<FocusPatch>> {
        let focused = policy::manifest_focused_tiled(&self.tabs, self.active_tab)?;
        let previous = self.session_focused_pane?;
        if previous == focused {
            return None;
        }
        self.session_focused_pane = Some(focused);
        policy::focus_tiled_pane(&mut self.tabs, focused);

        let mut patch = Vec::new();
        if policy::is_card_pane_id(&self.tabs, previous) {
            patch.push(FocusPatch {
                id: previous,
                is_focused: false,
            });
        }
        patch.push(FocusPatch {
            id: focused,
            is_focused: true,
        });
        Some(patch)
    }

    /// Resolve a switched-tab focus classification and publish the exact
    /// overlay the renderer needs before the next producer pull.
    fn resolve_focus_correction(
        &mut self,
        now: u64,
        manifest_fresh: bool,
        effects: &mut Vec<Effect>,
    ) {
        match self.focus_correction.resolve(
            &self.tabs,
            self.active_tab,
            self.session_focused_pane,
            manifest_fresh,
            now,
        ) {
            CorrectionAction::StrandedSidebar(pane_id) => {
                self.session_focused_pane = Some(pane_id);
                self.run_wake(wire::WakeRequest::FocusStranded { pane_id }, now, effects);
            }
            CorrectionAction::FocusWorkingPane { focused, unfocused } => {
                self.session_focused_pane = Some(focused);
                policy::focus_tiled_pane(&mut self.tabs, focused);
                let mut patch = vec![FocusPatch {
                    id: focused,
                    is_focused: true,
                }];
                if let Some(unfocused) = unfocused {
                    patch.push(FocusPatch {
                        id: unfocused,
                        is_focused: false,
                    });
                }
                if self.run_wake(wire::WakeRequest::FocusChanged { patch }, now, effects) {
                    let hash = policy::manifest_hash(&self.tabs);
                    self.policy.accept_manifest(hash);
                    self.policy.on_optimistic_signal(now);
                } else {
                    self.signal_change(now);
                }
            }
            CorrectionAction::Wait | CorrectionAction::Clear => {}
        }
    }

    fn dispatch_due(&mut self, now: u64, host: &impl Host, effects: &mut Vec<Effect>) {
        for poke in self.policy.due(now) {
            if poke == Poke::Alive {
                queue_list_clients(effects);
            }
            self.poke(poke, now, host, effects);
        }
    }

    /// Arm one host timer for the policy's next deadline, deduplicated by the
    /// [`TimerGate`]: event bursts arm one timer, an earlier deadline
    /// supersedes, and a superseded timer's fire is a harmless no-op.
    fn rearm(&mut self, now: u64, effects: &mut Vec<Effect>) {
        let policy_at = Some(self.policy.next_wake_at());
        let correction_at = self.focus_correction.next_deadline();
        let baseline_at = self.next_baseline_probe_at;
        let Some(at) = [policy_at, correction_at, baseline_at]
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
        let focused = policy::resolved_focused_pane_id(
            &self.tabs,
            self.active_tab,
            self.session_focused_pane,
        );
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
            focused,
            self.client_sample.as_ref(),
            &self.tabs,
        );
        let Some(argv) = wire::wake_argv(&self.wake_context(), request, topology.as_deref()) else {
            return false;
        };
        effects.push(Effect::RunCommand(argv));
        true
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

    fn baseline_probe_candidates(&self) -> Vec<u32> {
        policy::panes_needing_baseline(&self.tabs, &self.foreground, &self.baseline)
            .into_iter()
            .filter(|id| {
                self.baseline_attempts.get(id).copied().unwrap_or_default()
                    < BASELINE_PROBE_MAX_ATTEMPTS
            })
            .collect()
    }

    fn schedule_baseline_probe(&mut self, now: u64) {
        if self.baseline_probe_candidates().is_empty() {
            self.next_baseline_probe_at = None;
        } else if self.next_baseline_probe_at.is_none() {
            self.next_baseline_probe_at = Some(now);
        }
    }

    fn probe_baseline(&mut self, id: u32, host: &impl Host) -> bool {
        if self.baseline_attempts.get(&id).copied().unwrap_or_default()
            >= BASELINE_PROBE_MAX_ATTEMPTS
        {
            return false;
        }
        let Some(baseline) = host.baseline(id) else {
            let attempts = self.baseline_attempts.entry(id).or_default();
            *attempts = attempts.saturating_add(1);
            return false;
        };
        self.baseline.insert(id, baseline);
        self.baseline_attempts.remove(&id);
        true
    }

    fn dispatch_due_baseline_probe(&mut self, now: u64, host: &impl Host) {
        if self.next_baseline_probe_at.is_none_or(|at| now < at) {
            return;
        }
        self.next_baseline_probe_at = None;
        let candidates = self.baseline_probe_candidates();
        let mut changed = false;
        for id in candidates.into_iter().take(BASELINE_PROBE_BATCH) {
            changed = self.probe_baseline(id, host) || changed;
        }
        if changed {
            policy::apply_foreground_commands(&mut self.tabs, &self.foreground, &self.baseline);
            self.signal_change(now);
        }
        if !self.baseline_probe_candidates().is_empty() {
            self.next_baseline_probe_at = Some(now.saturating_add(BASELINE_PROBE_RETRY_MS));
        }
    }

    fn prune_baseline_state(&mut self, tabs: &BTreeMap<usize, Vec<PaneFields>>) {
        let pane_ids = tabs
            .values()
            .flatten()
            .filter(|pane| !pane.is_plugin)
            .map(|pane| pane.id)
            .collect::<BTreeSet<_>>();
        self.baseline.retain(|id, _| pane_ids.contains(id));
        self.baseline_attempts.retain(|id, _| pane_ids.contains(id));
    }

    fn set_foreground_command(
        &mut self,
        pane_id: ProjectedPaneId,
        command: Option<String>,
        now: u64,
        host: &impl Host,
    ) {
        let ProjectedPaneId::Terminal(id) = pane_id else {
            return;
        };
        match command.as_ref() {
            Some(command) => {
                self.foreground.insert(id, command.clone());
                self.baseline_attempts.remove(&id);
            }
            None => {
                self.foreground.remove(&id);
            }
        }
        if command.is_none() && self.granted {
            if self.baseline_probe_candidates().contains(&id) {
                let changed = self.probe_baseline(id, host);
                if changed
                    || self.baseline_attempts.get(&id).copied().unwrap_or_default()
                        >= BASELINE_PROBE_MAX_ATTEMPTS
                {
                    self.schedule_baseline_probe(now);
                } else {
                    self.next_baseline_probe_at = Some(now.saturating_add(BASELINE_PROBE_RETRY_MS));
                }
            }
        } else {
            self.schedule_baseline_probe(now);
        }
        let pane_command = self.foreground.get(&id).cloned().or_else(|| {
            self.baseline
                .get(&id)
                .map(|baseline| baseline.command.clone())
        });
        let pane_cwd = self
            .baseline
            .get(&id)
            .and_then(|baseline| baseline.cwd.clone());
        for pane in self.tabs.values_mut().flatten() {
            if !pane.is_plugin && pane.id == id {
                pane.pane_command = pane_command.clone();
                pane.pane_cwd = pane_cwd.clone();
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
            self.baseline.remove(&id);
            self.baseline_attempts.remove(&id);
            self.policy.forget_pane(id);
        }
    }
}

fn focused_patch_id(patch: &[FocusPatch]) -> Option<u32> {
    patch
        .iter()
        .find(|patch| patch.is_focused)
        .map(|patch| patch.id)
}

fn queue_list_clients(effects: &mut Vec<Effect>) {
    if !effects
        .iter()
        .any(|effect| matches!(effect, Effect::ListClients))
    {
        effects.push(Effect::ListClients);
    }
}

fn client_sample(clients: Vec<ProjectedClientFocus>) -> policy::ClientSample {
    let mut client_ids = BTreeSet::new();
    let mut viewed_panes = BTreeSet::new();
    for client in clients {
        client_ids.insert(client.client_id);
        viewed_panes.insert(client.pane_id);
    }
    policy::ClientSample {
        human_clients: client_ids.len().try_into().unwrap_or(u32::MAX),
        viewed_panes: viewed_panes.into_iter().collect(),
    }
}

#[cfg(test)]
mod tests;
