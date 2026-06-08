//! The Zellij plugin shell: projects host events into the pure policy core,
//! runs the pokes it returns, and broadcasts when a switched-to tab restored
//! focus to Rimz's sidebar. Headless — it renders nothing and holds no pane.
//! Compiled only for wasm (`register_plugin!` defines the wasip1 `main`); host
//! targets build a stub so `--workspace` builds, lints, and the policy unit
//! tests run without the wasm toolchain.

#[cfg(target_family = "wasm")]
mod shell {
    use std::collections::BTreeMap;
    use std::time::{SystemTime, UNIX_EPOCH};

    use rimz_presence_zellij::policy::{
        self, CorrectionAction, FocusCorrection, FocusPatch, FocusShortcut, PaneFields, Poke,
        PokePolicy, TimerGate, TopologyPayload,
    };
    use zellij_tile::prelude::*;

    #[derive(Default)]
    pub struct State {
        /// `None` until `load` runs (the `Default` the macro requires).
        policy: Option<PokePolicy>,
        /// The projected room shape, refreshed per manifest event.
        tabs: BTreeMap<usize, Vec<PaneFields>>,
        tab_names: BTreeMap<usize, String>,
        foreground: BTreeMap<u32, String>,
        active_tab: Option<usize>,
        active_focused_pane: Option<u32>,
        /// Classifies active-tab changes after Zellij's focus marks settle.
        focus_correction: FocusCorrection,
        /// Configuration written by rimz at load time (never user config):
        /// the workspace to poke and the absolute rimz binary, insulating the
        /// poke from the host PATH. Absent (a hand-loaded plugin), the wake
        /// CLI resolves the workspace from the host cwd ladder.
        workspace_id: Option<String>,
        session_name: Option<String>,
        rimz_bin: Option<String>,
        /// Pokes are gated until a grant is observed — either the explicit
        /// permission result or any application-state event arriving, which
        /// proves the cached grant already covers us.
        granted: bool,
        /// `CommandChanged` requires no permission, so it can arrive before the
        /// cached `RunCommands` grant is proven. Hold one topology signal until
        /// a permission-bearing event or grant result lets us run the wake CLI.
        pending_pregrant_change: bool,
        /// Deduplicates host timers over the policy's deadlines, so a burst
        /// of events arms one timer, not one per event, and a superseded
        /// timer's late fire never arms a duplicate chain.
        timer_gate: TimerGate,
    }

    impl ZellijPlugin for State {
        fn load(&mut self, configuration: BTreeMap<String, String>) {
            self.workspace_id = configuration.get("workspace_id").cloned();
            self.session_name = configuration.get("session_name").cloned();
            self.rimz_bin = configuration.get("rimz_bin").cloned();
            request_permission(&[
                PermissionType::ReadApplicationState,
                PermissionType::RunCommands,
            ]);
            subscribe(&[
                EventType::PaneUpdate,
                EventType::TabUpdate,
                EventType::CommandChanged,
                EventType::PaneClosed,
                EventType::Timer,
                EventType::PermissionRequestResult,
            ]);
            let now = now_ms();
            self.policy = Some(PokePolicy::new(now));
            self.rearm(now);
        }

        fn update(&mut self, event: Event) -> bool {
            let now = now_ms();
            match event {
                Event::PermissionRequestResult(PermissionStatus::Granted) => {
                    self.mark_granted(now);
                }
                Event::PermissionRequestResult(PermissionStatus::Denied) => {
                    self.granted = false;
                }
                Event::PaneUpdate(manifest) => {
                    // Application state flowing proves the (possibly cached)
                    // grant covers us: Zellij sends no PermissionRequestResult
                    // when the grant comes from the permission cache (verified
                    // live on 0.44.3), so this path is load-bearing.
                    self.mark_granted(now);
                    let projected = project(&manifest, &self.tab_names);
                    // Zellij can deliver partial pane manifests; omitted tabs
                    // retain their previous state instead of collapsing the room.
                    let mut next_tabs = policy::merged_room(&self.tabs, &projected);
                    policy::apply_foreground_commands(&mut next_tabs, &self.foreground);
                    let opened = policy::opened_card_panes(&self.tabs, &next_tabs);
                    let focus_patch =
                        policy::focus_shortcut_if_only_focus_changed(&self.tabs, &next_tabs);
                    self.tabs = next_tabs;
                    // Poke every opened pane — `fold`, not `any`, so a manifest
                    // carrying two new panes emits both card-create events.
                    let emitted_open = opened.iter().fold(false, |emitted, pane| {
                        self.poke_pane_opened(pane, now) || emitted
                    });
                    match focus_patch {
                        Some(FocusShortcut::Patch(patch))
                            if self.poke_focus_changed(&patch, now) =>
                        {
                            let hash = policy::manifest_hash(&self.tabs, self.active_tab);
                            if let Some(policy) = self.policy.as_mut() {
                                policy.accept_manifest(hash);
                                policy.on_optimistic_signal(now);
                            }
                        }
                        Some(FocusShortcut::Ignore) => {
                            let hash = policy::manifest_hash(&self.tabs, self.active_tab);
                            if let Some(policy) = self.policy.as_mut() {
                                policy.accept_manifest(hash);
                            }
                        }
                        _ if emitted_open => {
                            let hash = policy::manifest_hash(&self.tabs, self.active_tab);
                            if let Some(policy) = self.policy.as_mut() {
                                policy.accept_manifest(hash);
                            }
                        }
                        _ => self.fold(now),
                    }
                    self.resolve_focus_correction(now, true);
                    self.active_focused_pane = policy::focused_pane_id(&self.tabs, self.active_tab);
                }
                Event::TabUpdate(tabs) => {
                    self.mark_granted(now);
                    let previous_active = self.active_tab;
                    let previous_focused_pane = self
                        .active_focused_pane
                        .or_else(|| policy::focused_pane_id(&self.tabs, previous_active));
                    let next_active = tabs.iter().find(|tab| tab.active).map(|tab| tab.position);
                    self.tab_names = tabs
                        .iter()
                        .map(|tab| (tab.position, tab.name.clone()))
                        .collect();
                    self.active_tab = next_active;
                    self.active_focused_pane = policy::focused_pane_id(&self.tabs, self.active_tab);
                    self.focus_correction.on_active_tab_change_with_focus(
                        previous_active,
                        next_active,
                        previous_focused_pane,
                        now,
                    );
                }
                Event::CommandChanged(pane_id, command, is_foreground, _) => {
                    if is_foreground {
                        if let Some(command_text) = policy::joined_foreground_command(&command) {
                            self.remember_foreground_command(&pane_id, command_text);
                            if self.poke_command_changed(&pane_id, &command, now) {
                                if let Some(policy) = self.policy.as_mut() {
                                    let hash = policy::manifest_hash(&self.tabs, self.active_tab);
                                    policy.accept_manifest(hash);
                                    policy.on_optimistic_signal(now);
                                }
                            } else {
                                self.signal_change(now);
                            }
                        }
                    }
                }
                Event::PaneClosed(pane_id) => {
                    self.mark_granted(now);
                    self.remove_pane(&pane_id);
                    if !self.poke_pane_closed(&pane_id, now) {
                        self.signal_change(now);
                    } else {
                        let hash = policy::manifest_hash(&self.tabs, self.active_tab);
                        if let Some(policy) = self.policy.as_mut() {
                            policy.accept_manifest(hash);
                        }
                    }
                }
                Event::Timer(_) => {
                    self.timer_gate.on_fire(now);
                }
                _ => {}
            }
            self.dispatch_due(now);
            self.resolve_focus_correction(now, false);
            self.rearm(now);
            false // headless: never render
        }

        fn pipe(&mut self, _pipe_message: PipeMessage) -> bool {
            // The launch channel: rimz loads this plugin via `zellij pipe
            // --plugin`, the one load verb that works on a clientless session.
            // The message carries nothing — loading was the point — and
            // delivery itself releases the CLI (an explicit
            // `unblock_cli_pipe_input` would need a third permission,
            // `ReadCliPipes`, for nothing). The CLI blocks only while the
            // launch is permission-pending, which rimz bounds with its
            // command deadline.
            false // headless: never render
        }
    }

    impl State {
        /// Flip into granted mode once: poke an immediate keepalive so the
        /// producer enters event mode now rather than after the first cadence,
        /// unless a pre-grant topology signal is already waiting — that
        /// `panes-changed` wake writes the same stamp and also refreshes the
        /// pane frame. Hide the pane Zellij surfaced for the permission prompt;
        /// the plugin is headless, so a visible pane is only ever that prompt's
        /// leftover. Idempotent; already-hidden panes no-op.
        fn mark_granted(&mut self, now: u64) {
            if self.granted {
                self.flush_pregrant_change(now);
                return;
            }
            self.granted = true;
            hide_self();
            if self.pending_pregrant_change {
                self.flush_pregrant_change(now);
            } else {
                self.poke(Poke::Alive, now);
            }
        }

        fn signal_change(&mut self, now: u64) {
            if !self.granted {
                self.pending_pregrant_change = true;
                return;
            }
            if let Some(policy) = self.policy.as_mut() {
                policy.on_signal(now);
            }
        }

        fn flush_pregrant_change(&mut self, now: u64) {
            if self.pending_pregrant_change {
                self.pending_pregrant_change = false;
                if let Some(policy) = self.policy.as_mut() {
                    policy.on_signal(now);
                }
            }
        }

        /// Fold the current projected shape into the policy.
        fn fold(&mut self, now: u64) {
            let hash = policy::manifest_hash(&self.tabs, self.active_tab);
            if let Some(policy) = self.policy.as_mut() {
                policy.on_manifest(hash, now);
            }
        }

        /// Resolve a switched-tab focus classification. The plugin broadcasts
        /// only the stranded sidebar pane; the renderer owning that pane
        /// chooses the remembered working target.
        fn resolve_focus_correction(&mut self, now: u64, manifest_fresh: bool) {
            match self
                .focus_correction
                .resolve(&self.tabs, self.active_tab, manifest_fresh, now)
            {
                CorrectionAction::Broadcast(pane_id) => {
                    self.poke_focus_stranded(pane_id, now);
                }
                CorrectionAction::Wait | CorrectionAction::Clear => {}
            }
        }

        fn dispatch_due(&mut self, now: u64) {
            let Some(policy) = self.policy.as_mut() else {
                return;
            };
            for poke in policy.due(now) {
                self.poke(poke, now);
            }
        }

        /// Arm one host timer for the policy's next deadline, deduplicated by
        /// the [`TimerGate`]: event bursts arm one timer, an earlier deadline
        /// supersedes, and a superseded timer's fire is a harmless no-op.
        fn rearm(&mut self, now: u64) {
            let policy_at = self.policy.as_ref().map(PokePolicy::next_wake_at);
            let correction_at = self.focus_correction.next_deadline();
            let Some(at) = [policy_at, correction_at].into_iter().flatten().min() else {
                return;
            };
            if self.timer_gate.should_arm(at) {
                set_timeout(at.saturating_sub(now) as f64 / 1_000.0);
            }
        }

        /// One fixed argv per poke — the whole host-side surface of this
        /// plugin. Fire-and-forget: a failed wake means no stamp, and the
        /// producer degrades to poll mode on its own.
        fn poke(&self, poke: Poke, now: u64) {
            if !self.granted {
                return;
            }
            let reason = match poke {
                Poke::Changed => "panes-changed",
                Poke::Alive => "alive",
            };
            let program = self.rimz_bin.as_deref().unwrap_or("rimz");
            let mut argv = vec![
                program.to_owned(),
                "sidebar".to_owned(),
                "wake".to_owned(),
                "--reason".to_owned(),
                reason.to_owned(),
            ];
            if let Some(workspace_id) = self.workspace_id.as_deref() {
                argv.push("--workspace-id".to_owned());
                argv.push(workspace_id.to_owned());
            }
            self.append_topology_arg(&mut argv, now);
            let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
            run_command(&refs, BTreeMap::new());
        }

        fn append_topology_arg(&self, argv: &mut Vec<String>, now: u64) {
            let Some(session_name) = self.session_name.as_deref() else {
                return;
            };
            if self.tabs.is_empty() {
                return;
            }
            let payload = TopologyPayload::from_tabs(session_name, now, &self.tabs);
            let Ok(json) = serde_json::to_string(&payload) else {
                return;
            };
            argv.push("--topology".to_owned());
            argv.push(json);
        }

        fn remember_foreground_command(&mut self, pane_id: &PaneId, command: String) {
            let PaneId::Terminal(id) = pane_id else {
                return;
            };
            self.foreground.insert(*id, command.clone());
            for pane in self.tabs.values_mut().flatten() {
                if !pane.is_plugin && pane.id == *id {
                    pane.pane_command = Some(command);
                    return;
                }
            }
        }

        fn remove_pane(&mut self, pane_id: &PaneId) {
            let (is_plugin, id) = match pane_id {
                PaneId::Terminal(id) => (false, *id),
                PaneId::Plugin(id) => (true, *id),
            };
            for panes in self.tabs.values_mut() {
                panes.retain(|pane| pane.is_plugin != is_plugin || pane.id != id);
            }
            self.tabs.retain(|_, panes| !panes.is_empty());
            if !is_plugin {
                self.foreground.remove(&id);
            }
        }

        /// Publish an optimistic command patch for a terminal pane already
        /// known to the native sidebar cache. Returns false when the exact-cache
        /// shortcut is unavailable, so the caller can fall back to a normal
        /// `panes-changed` poke.
        fn poke_command_changed(&self, pane_id: &PaneId, command: &[String], now: u64) -> bool {
            if !self.granted || command.is_empty() {
                return false;
            }
            let PaneId::Terminal(_) = pane_id else {
                return false;
            };
            let Some(session_name) = self.session_name.as_deref() else {
                return false;
            };
            let program = self.rimz_bin.as_deref().unwrap_or("rimz");
            let mut argv = vec![
                program.to_owned(),
                "sidebar".to_owned(),
                "wake".to_owned(),
                "--reason".to_owned(),
                "command-changed".to_owned(),
                "--session-name".to_owned(),
                session_name.to_owned(),
                "--pane-id".to_owned(),
                pane_id.to_string(),
            ];
            if let Some(workspace_id) = self.workspace_id.as_deref() {
                argv.push("--workspace-id".to_owned());
                argv.push(workspace_id.to_owned());
            }
            let mut pushed_command_arg = false;
            for arg in command {
                if !arg.is_empty() {
                    argv.push("--command-arg".to_owned());
                    argv.push(arg.clone());
                    pushed_command_arg = true;
                }
            }
            if !pushed_command_arg {
                return false;
            }
            self.append_topology_arg(&mut argv, now);
            let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
            run_command(&refs, BTreeMap::new());
            true
        }

        fn poke_pane_opened(&self, pane: &PaneFields, now: u64) -> bool {
            if !self.granted || !pane.is_card_pane() {
                return false;
            }
            let Some(session_name) = self.session_name.as_deref() else {
                return false;
            };
            let program = self.rimz_bin.as_deref().unwrap_or("rimz");
            let mut argv = vec![
                program.to_owned(),
                "sidebar".to_owned(),
                "wake".to_owned(),
                "--reason".to_owned(),
                "pane-opened".to_owned(),
                "--session-name".to_owned(),
                session_name.to_owned(),
                "--pane-id".to_owned(),
                format!("terminal_{}", pane.id),
            ];
            if let Some(workspace_id) = self.workspace_id.as_deref() {
                argv.push("--workspace-id".to_owned());
                argv.push(workspace_id.to_owned());
            }
            if let Some(command) = pane
                .pane_command
                .as_ref()
                .or(pane.terminal_command.as_ref())
                .filter(|command| !command.is_empty())
            {
                argv.push("--command-arg".to_owned());
                argv.push(command.clone());
            }
            self.append_topology_arg(&mut argv, now);
            let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
            run_command(&refs, BTreeMap::new());
            true
        }

        fn poke_pane_closed(&self, pane_id: &PaneId, now: u64) -> bool {
            if !self.granted {
                return false;
            }
            let PaneId::Terminal(_) = pane_id else {
                return false;
            };
            let Some(session_name) = self.session_name.as_deref() else {
                return false;
            };
            let program = self.rimz_bin.as_deref().unwrap_or("rimz");
            let mut argv = vec![
                program.to_owned(),
                "sidebar".to_owned(),
                "wake".to_owned(),
                "--reason".to_owned(),
                "pane-closed".to_owned(),
                "--session-name".to_owned(),
                session_name.to_owned(),
                "--pane-id".to_owned(),
                pane_id.to_string(),
            ];
            if let Some(workspace_id) = self.workspace_id.as_deref() {
                argv.push("--workspace-id".to_owned());
                argv.push(workspace_id.to_owned());
            }
            self.append_topology_arg(&mut argv, now);
            let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
            run_command(&refs, BTreeMap::new());
            true
        }

        fn poke_focus_stranded(&self, pane_id: u32, now: u64) {
            if !self.granted {
                return;
            }
            let Some(session_name) = self.session_name.as_deref() else {
                return;
            };
            let program = self.rimz_bin.as_deref().unwrap_or("rimz");
            let mut argv = vec![
                program.to_owned(),
                "sidebar".to_owned(),
                "wake".to_owned(),
                "--reason".to_owned(),
                "focus-stranded".to_owned(),
                "--session-name".to_owned(),
                session_name.to_owned(),
                "--pane-id".to_owned(),
                format!("terminal_{pane_id}"),
            ];
            if let Some(workspace_id) = self.workspace_id.as_deref() {
                argv.push("--workspace-id".to_owned());
                argv.push(workspace_id.to_owned());
            }
            self.append_topology_arg(&mut argv, now);
            let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
            run_command(&refs, BTreeMap::new());
        }

        /// Publish an optimistic focus patch for panes already known to the
        /// native sidebar cache. Returns false when the exact-cache shortcut is
        /// unavailable, so the caller can fall back to a normal `panes-changed`
        /// poke.
        fn poke_focus_changed(&self, patch: &[FocusPatch], now: u64) -> bool {
            if !self.granted || patch.is_empty() {
                return false;
            }
            let Some(session_name) = self.session_name.as_deref() else {
                return false;
            };
            let program = self.rimz_bin.as_deref().unwrap_or("rimz");
            let mut argv = vec![
                program.to_owned(),
                "sidebar".to_owned(),
                "wake".to_owned(),
                "--reason".to_owned(),
                "focus-changed".to_owned(),
                "--session-name".to_owned(),
                session_name.to_owned(),
            ];
            if let Some(workspace_id) = self.workspace_id.as_deref() {
                argv.push("--workspace-id".to_owned());
                argv.push(workspace_id.to_owned());
            }
            for pane in patch {
                argv.push(
                    if pane.is_focused {
                        "--focused-pane-id"
                    } else {
                        "--unfocused-pane-id"
                    }
                    .to_owned(),
                );
                argv.push(format!("terminal_{}", pane.id));
            }
            self.append_topology_arg(&mut argv, now);
            let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
            run_command(&refs, BTreeMap::new());
            true
        }
    }

    fn project(
        manifest: &PaneManifest,
        tab_names: &BTreeMap<usize, String>,
    ) -> BTreeMap<usize, Vec<PaneFields>> {
        manifest
            .panes
            .iter()
            .map(|(tab, panes)| {
                let mut fields: Vec<PaneFields> = panes
                    .iter()
                    .map(|pane| PaneFields {
                        id: pane.id,
                        is_plugin: pane.is_plugin,
                        is_focused: pane.is_focused,
                        is_suppressed: pane.is_suppressed,
                        exited: pane.exited,
                        is_held: pane.is_held,
                        tab_position: *tab as u64,
                        tab_name: tab_names.get(tab).cloned(),
                        pane_x: Some(pane.pane_x as u64),
                        pane_columns: Some(pane.pane_columns as u64),
                        title: pane.title.clone(),
                        pane_command: None,
                        terminal_command: pane.terminal_command.clone(),
                    })
                    .collect();
                // Deterministic order regardless of the host map's iteration;
                // terminal and plugin panes have separate id spaces.
                fields.sort_unstable_by_key(|pane| (pane.is_plugin, pane.id));
                (*tab, fields)
            })
            .collect()
    }

    /// Unix milliseconds via the WASI clock. The policy only compares
    /// relative instants, so coarse wall-clock is plenty.
    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64
    }
}

// The macro expansion references prelude names (`ZellijPlugin`,
// `report_panic`) unqualified at its invocation site, so the prelude must be
// in scope at the crate root.
#[cfg(target_family = "wasm")]
use zellij_tile::prelude::*;

#[cfg(target_family = "wasm")]
zellij_tile::register_plugin!(shell::State);

/// Host-target stub: the plugin entrypoint exists only on wasm.
#[cfg(not(target_family = "wasm"))]
fn main() {}
