//! Host-testable decision engine for the Zellij presence plugin.
//!
//! The wasm shell projects Zellij events into this module, then executes the
//! returned [`Effect`]s. This keeps room state, poke timing, focus correction,
//! permission gating, and topology publication testable on the host target.

use std::collections::BTreeMap;

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
    /// Fire-and-forget host fork: wake pokes and focus-sidebar.
    RunCommand(Vec<String>),
    /// Hide the permission-prompt pane left by Zellij.
    HideSelf,
    /// Runtime `reconfigure(kdl, false)`.
    Reconfigure(String),
    /// Admit browser clients to the current Zellij session.
    ShareSession,
    /// Close this plugin instance.
    CloseSelf,
    /// Arm one host timer, in milliseconds from now.
    SetTimeout(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectedPaneId {
    Terminal(u32),
    Plugin(u32),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EngineConfig {
    pub workspace_id: Option<String>,
    pub session_name: Option<String>,
    pub rimz_bin: Option<String>,
    pub plugin_id: Option<u32>,
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
    active_tab: Option<usize>,
    active_focused_pane: Option<u32>,
    session_focused_pane: Option<u32>,
    focus_correction: FocusCorrection,
    workspace_id: Option<String>,
    session_name: Option<String>,
    rimz_bin: Option<String>,
    plugin_id: Option<u32>,
    focus_key: Option<String>,
    focus_follows_mouse: Option<bool>,
    mouse_click_through: Option<bool>,
    granted: bool,
    pending_pregrant_change: bool,
    share_requested: bool,
    timer_gate: TimerGate,
    loaded_at_ms: u64,
}

impl Engine {
    pub fn new(now: u64, config: EngineConfig) -> Self {
        Self {
            policy: PokePolicy::new(now),
            tabs: BTreeMap::new(),
            last_raw_stable_hash: None,
            tab_names: BTreeMap::new(),
            foreground: BTreeMap::new(),
            baseline: BTreeMap::new(),
            active_tab: None,
            active_focused_pane: None,
            session_focused_pane: None,
            focus_correction: FocusCorrection::default(),
            workspace_id: config.workspace_id,
            session_name: config.session_name,
            rimz_bin: config.rimz_bin,
            plugin_id: config.plugin_id,
            focus_key: config.focus_key,
            focus_follows_mouse: config.focus_follows_mouse,
            mouse_click_through: config.mouse_click_through,
            granted: false,
            pending_pregrant_change: false,
            share_requested: false,
            timer_gate: TimerGate::default(),
            loaded_at_ms: now,
        }
    }

    pub fn tab_names(&self) -> &BTreeMap<usize, String> {
        &self.tab_names
    }

    pub fn on_load(&mut self, now: u64, host: &impl Host) -> Vec<Effect> {
        self.finish_update(now, host, Vec::new())
    }

    pub fn on_permission_granted(&mut self, now: u64, host: &impl Host) -> Vec<Effect> {
        let mut effects = Vec::new();
        self.mark_granted(now, host, &mut effects);
        self.apply_runtime_reconfigure(&mut effects);
        if self.share_requested {
            effects.push(Effect::ShareSession);
        }
        self.finish_update(now, host, effects)
    }

    pub fn on_permission_denied(&mut self, now: u64, host: &impl Host) -> Vec<Effect> {
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
        let mut effects = Vec::new();
        // Application state flowing proves the (possibly cached) grant covers
        // us: Zellij sends no PermissionRequestResult when the grant comes from
        // the permission cache, so this path is load-bearing.
        self.mark_granted(now, host, &mut effects);
        let stable_unchanged = self.last_raw_stable_hash == Some(raw_hash) && !self.tabs.is_empty();
        self.last_raw_stable_hash = Some(raw_hash);
        if !stable_unchanged {
            let projected = project(&self.tab_names);
            // Zellij can deliver partial pane manifests; omitted tabs retain
            // their previous state instead of collapsing the room.
            let mut next_tabs = policy::merged_room(&self.tabs, &projected);
            let baseline_ids =
                policy::panes_needing_baseline(&next_tabs, &self.foreground, &self.baseline);
            self.probe_baselines(baseline_ids, host);
            policy::apply_foreground_commands(&mut next_tabs, &self.foreground, &self.baseline);
            let opened = policy::opened_card_panes(&self.tabs, &next_tabs);
            let focus_patch = policy::focus_shortcut_if_only_focus_changed(&self.tabs, &next_tabs);
            if let Some(focused) = focus_patch
                .as_ref()
                .and_then(|patch| focused_patch_id(patch))
            {
                self.session_focused_pane = Some(focused);
            }
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
            match focus_patch {
                Some(patch) => {
                    if self.run_wake(wire::WakeRequest::FocusChanged { patch }, now, &mut effects) {
                        let hash = policy::manifest_hash(&self.tabs);
                        self.policy.accept_manifest(hash);
                        self.policy.on_optimistic_signal(now);
                    } else if emitted_open {
                        let hash = policy::manifest_hash(&self.tabs);
                        self.policy.accept_manifest(hash);
                    } else {
                        self.fold(now);
                    }
                }
                None if emitted_open => {
                    let hash = policy::manifest_hash(&self.tabs);
                    self.policy.accept_manifest(hash);
                }
                None => self.fold(now),
            }
            self.resolve_focus_correction(now, true, &mut effects);
            self.active_focused_pane = policy::resolved_focused_pane_id(
                &self.tabs,
                self.active_tab,
                self.session_focused_pane,
            );
        } else {
            let baseline_ids =
                policy::panes_needing_baseline(&self.tabs, &self.foreground, &self.baseline);
            if self.probe_baselines(baseline_ids, host) {
                policy::apply_foreground_commands(&mut self.tabs, &self.foreground, &self.baseline);
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
        let mut effects = Vec::new();
        self.mark_granted(now, host, &mut effects);
        let previous_active = self.active_tab;
        let previous_focused_pane = self.active_focused_pane.or_else(|| {
            policy::resolved_focused_pane_id(&self.tabs, previous_active, self.session_focused_pane)
        });
        self.tab_names = tab_names;
        self.active_tab = active;
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
        let mut effects = Vec::new();
        match policy::foreground_command_update(&command, is_foreground) {
            ForegroundCommandUpdate::Remember(command_text) => {
                self.set_foreground_command(pane, Some(command_text), host);
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
                self.set_foreground_command(pane, None, host);
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
        self.timer_gate.on_fire(now);
        self.finish_update(now, host, Vec::new())
    }

    pub fn on_focus_sidebar_pipe(&mut self) -> Vec<Effect> {
        let mut effects = Vec::new();
        self.run_focus_sidebar(&mut effects);
        effects
    }

    pub fn on_share_session_pipe(&mut self) -> Vec<Effect> {
        self.share_requested = true;
        // This can be dropped while the upgraded StartWebServer grant is
        // pending; PermissionRequestResult(Granted) replays it.
        vec![Effect::ShareSession]
    }

    pub fn on_dump_topology_pipe(&mut self, now: u64, host: &impl Host) -> Vec<Effect> {
        let mut effects = Vec::new();
        if !self.granted {
            self.pending_pregrant_change = true;
            return effects;
        }
        let baseline_ids =
            policy::panes_needing_baseline(&self.tabs, &self.foreground, &self.baseline);
        if self.probe_baselines(baseline_ids, host) {
            policy::apply_foreground_commands(&mut self.tabs, &self.foreground, &self.baseline);
        }
        self.run_wake(wire::WakeRequest::Changed, now, &mut effects);
        effects
    }

    pub fn on_retire_pipe(&self, payload: Option<&str>) -> Vec<Effect> {
        if wire::should_retire(self.rimz_bin.as_deref(), payload) {
            vec![Effect::CloseSelf]
        } else {
            Vec::new()
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
            self.poke(poke, now, host, effects);
        }
    }

    /// Arm one host timer for the policy's next deadline, deduplicated by the
    /// [`TimerGate`]: event bursts arm one timer, an earlier deadline
    /// supersedes, and a superseded timer's fire is a harmless no-op.
    fn rearm(&mut self, now: u64, effects: &mut Vec<Effect>) {
        let policy_at = Some(self.policy.next_wake_at());
        let correction_at = self.focus_correction.next_deadline();
        let Some(at) = [policy_at, correction_at].into_iter().flatten().min() else {
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
        });
        let topology = wire::topology_json(
            self.session_name.as_deref(),
            now,
            writer,
            focused,
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

    fn probe_baselines(&mut self, ids: Vec<u32>, host: &impl Host) -> bool {
        let mut changed = false;
        for id in ids {
            let Some(baseline) = host.baseline(id) else {
                continue;
            };
            self.baseline.insert(id, baseline);
            changed = true;
        }
        changed
    }

    fn set_foreground_command(
        &mut self,
        pane_id: ProjectedPaneId,
        command: Option<String>,
        host: &impl Host,
    ) {
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
        if command.is_none() && self.granted {
            let baseline_ids =
                policy::panes_needing_baseline(&self.tabs, &self.foreground, &self.baseline);
            if baseline_ids.contains(&id) {
                self.probe_baselines(vec![id], host);
            }
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

#[cfg(test)]
mod tests;
