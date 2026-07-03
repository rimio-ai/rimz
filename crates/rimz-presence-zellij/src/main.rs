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
        self, CorrectionAction, FocusCorrection, FocusPatch, ForegroundCommandUpdate, PaneFields,
        Poke, PokePolicy, RawStablePaneFields, TimerGate,
    };
    use rimz_presence_zellij::wire::{self, FOCUS_SIDEBAR_PIPE, SHARE_SESSION_PIPE};
    use zellij_tile::prelude::*;

    #[derive(Default)]
    pub struct State {
        /// `None` until `load` runs (the `Default` the macro requires).
        policy: Option<PokePolicy>,
        /// The projected room shape, refreshed per manifest event.
        tabs: BTreeMap<usize, Vec<PaneFields>>,
        last_raw_stable_hash: Option<u64>,
        tab_names: BTreeMap<usize, String>,
        foreground: BTreeMap<u32, String>,
        active_tab: Option<usize>,
        active_focused_pane: Option<u32>,
        session_focused_pane: Option<u32>,
        /// Classifies active-tab changes after Zellij's focus marks settle.
        focus_correction: FocusCorrection,
        /// Configuration written by rimz at load time (never user config):
        /// the workspace to poke and the absolute rimz binary, insulating the
        /// poke from the host PATH. Absent (a hand-loaded plugin), the wake
        /// CLI resolves the workspace from the host cwd ladder.
        workspace_id: Option<String>,
        session_name: Option<String>,
        rimz_bin: Option<String>,
        plugin_id: Option<u32>,
        /// The focus-key chord rimz injected at load (e.g. `Alt+p`), bound once
        /// the Reconfigure grant lands so the key reaches the sidebar from any
        /// pane. Absent when the user disabled it or runs a hand-loaded plugin.
        focus_key: Option<String>,
        /// Runtime mouse options rimz injects at load, applied through
        /// `reconfigure()` so they override the user's config.kdl absolutely.
        focus_follows_mouse: Option<bool>,
        mouse_click_through: Option<bool>,
        /// Pokes are gated until a grant is observed — either the explicit
        /// permission result or any application-state event arriving, which
        /// proves the cached grant already covers us.
        granted: bool,
        /// `CommandChanged` requires no permission, so it can arrive before the
        /// cached `RunCommands` grant is proven. Hold one topology signal until
        /// a permission-bearing event or grant result lets us run the wake CLI.
        pending_pregrant_change: bool,
        /// `rimz web open` asked this plugin to share the session for browser
        /// clients. The first `share_current_session()` call may arrive before
        /// the new `StartWebServer` grant is live, so the explicit grant event
        /// replays it.
        share_requested: bool,
        /// Deduplicates host timers over the policy's deadlines, so a burst
        /// of events arms one timer, not one per event, and a superseded
        /// timer's late fire never arms a duplicate chain.
        timer_gate: TimerGate,
        loaded_at_ms: u64,
        commands_completed: u64,
    }

    impl ZellijPlugin for State {
        fn load(&mut self, configuration: BTreeMap<String, String>) {
            self.workspace_id = configuration.get("workspace_id").cloned();
            self.session_name = configuration.get("session_name").cloned();
            self.rimz_bin = configuration.get("rimz_bin").cloned();
            self.plugin_id = Some(get_plugin_ids().plugin_id);
            self.focus_key = configuration.get("focus_key").cloned();
            self.focus_follows_mouse = wire::parse_configuration_bool(
                configuration.get("focus_follows_mouse").map(String::as_str),
            );
            self.mouse_click_through = wire::parse_configuration_bool(
                configuration.get("mouse_click_through").map(String::as_str),
            );
            let permissions = vec![
                PermissionType::ReadApplicationState,
                PermissionType::RunCommands,
                PermissionType::Reconfigure,
                PermissionType::StartWebServer,
            ];
            request_permission(&permissions);
            subscribe(&[
                EventType::PaneUpdate,
                EventType::TabUpdate,
                EventType::CommandChanged,
                EventType::PaneClosed,
                EventType::Timer,
                EventType::PermissionRequestResult,
                EventType::RunCommandResult,
            ]);
            let now = now_ms();
            self.loaded_at_ms = now;
            self.policy = Some(PokePolicy::new(now));
            self.rearm(now);
        }

        fn update(&mut self, event: Event) -> bool {
            let now = now_ms();
            match event {
                Event::PermissionRequestResult(PermissionStatus::Granted) => {
                    self.mark_granted(now);
                    // The explicit grant is the authoritative moment every
                    // requested permission — Reconfigure included — is live.
                    // Re-issue the idempotent runtime reconfigure here so an
                    // upgrade that proved an older permission cache via an
                    // app-state event before answering the new prompt still
                    // applies every new capability.
                    self.apply_runtime_reconfigure();
                    if self.share_requested {
                        share_current_session();
                    }
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
                    let raw_hash = raw_manifest_stable_hash(&manifest, &self.tab_names);
                    let stable_unchanged =
                        self.last_raw_stable_hash == Some(raw_hash) && !self.tabs.is_empty();
                    self.last_raw_stable_hash = Some(raw_hash);
                    if !stable_unchanged {
                        let projected = project(&manifest, &self.tab_names);
                        // Zellij can deliver partial pane manifests; omitted tabs
                        // retain their previous state instead of collapsing the room.
                        let mut next_tabs = policy::merged_room(&self.tabs, &projected);
                        policy::apply_foreground_commands(&mut next_tabs, &self.foreground);
                        let opened = policy::opened_card_panes(&self.tabs, &next_tabs);
                        let focus_patch =
                            policy::focus_shortcut_if_only_focus_changed(&self.tabs, &next_tabs);
                        if let Some(focused) = focus_patch
                            .as_ref()
                            .and_then(|patch| focused_patch_id(patch))
                        {
                            self.session_focused_pane = Some(focused);
                        }
                        self.tabs = next_tabs;
                        // Poke every opened pane — `fold`, not `any`, so a manifest
                        // carrying two new panes emits both card-create events.
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
                            ) || emitted
                        });
                        match focus_patch {
                            Some(patch) => {
                                if self.run_wake(wire::WakeRequest::FocusChanged { patch }, now) {
                                    let hash = policy::manifest_hash(&self.tabs);
                                    if let Some(policy) = self.policy.as_mut() {
                                        policy.accept_manifest(hash);
                                        policy.on_optimistic_signal(now);
                                    }
                                } else if emitted_open {
                                    let hash = policy::manifest_hash(&self.tabs);
                                    if let Some(policy) = self.policy.as_mut() {
                                        policy.accept_manifest(hash);
                                    }
                                } else {
                                    self.fold(now);
                                }
                            }
                            None if emitted_open => {
                                let hash = policy::manifest_hash(&self.tabs);
                                if let Some(policy) = self.policy.as_mut() {
                                    policy.accept_manifest(hash);
                                }
                            }
                            None => self.fold(now),
                        }
                        self.resolve_focus_correction(now, true);
                        self.active_focused_pane = policy::resolved_focused_pane_id(
                            &self.tabs,
                            self.active_tab,
                            self.session_focused_pane,
                        );
                    }
                }
                Event::TabUpdate(tabs) => {
                    self.mark_granted(now);
                    let previous_active = self.active_tab;
                    let previous_focused_pane = self.active_focused_pane.or_else(|| {
                        policy::resolved_focused_pane_id(
                            &self.tabs,
                            previous_active,
                            self.session_focused_pane,
                        )
                    });
                    let next_active = tabs.iter().find(|tab| tab.active).map(|tab| tab.position);
                    self.tab_names = tab_names(&tabs);
                    self.active_tab = next_active;
                    self.active_focused_pane = policy::resolved_focused_pane_id(
                        &self.tabs,
                        self.active_tab,
                        self.session_focused_pane,
                    );
                    self.focus_correction.on_active_tab_change(
                        previous_active,
                        next_active,
                        previous_focused_pane,
                        now,
                    );
                }
                Event::CommandChanged(pane_id, command, is_foreground, _) => {
                    match policy::foreground_command_update(&command, is_foreground) {
                        ForegroundCommandUpdate::Remember(command_text) => {
                            self.set_foreground_command(&pane_id, Some(command_text));
                            if let Some(id) = self.optimistic_command_poke_pane(&pane_id, now)
                                && self.run_wake(
                                    wire::WakeRequest::CommandChanged {
                                        pane_id: id,
                                        args: command,
                                    },
                                    now,
                                )
                            {
                                if let Some(policy) = self.policy.as_mut() {
                                    let hash = policy::manifest_hash(&self.tabs);
                                    policy.accept_manifest(hash);
                                    policy.accept_optimistic_pane_poke(id, now);
                                }
                            } else {
                                self.signal_change(now);
                            }
                        }
                        ForegroundCommandUpdate::Forget => {
                            self.set_foreground_command(&pane_id, None);
                            self.signal_change(now);
                        }
                    }
                }
                Event::PaneClosed(pane_id) => {
                    self.mark_granted(now);
                    let closed_terminal = match &pane_id {
                        PaneId::Terminal(id) => Some(*id),
                        PaneId::Plugin(_) => None,
                    };
                    self.remove_pane(&pane_id);
                    if let PaneId::Terminal(id) = pane_id
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
                        self.run_wake(wire::WakeRequest::PaneClosed { pane_id }, now)
                    }) {
                        self.signal_change(now);
                    } else {
                        let hash = policy::manifest_hash(&self.tabs);
                        if let Some(policy) = self.policy.as_mut() {
                            policy.accept_manifest(hash);
                        }
                    }
                }
                Event::Timer(_) => {
                    self.timer_gate.on_fire(now);
                }
                Event::RunCommandResult(..) => {
                    self.commands_completed = self.commands_completed.saturating_add(1);
                    return false;
                }
                _ => {}
            }
            self.dispatch_due(now);
            self.resolve_focus_correction(now, false);
            self.rearm(now);
            false // headless: never render
        }

        fn pipe(&mut self, pipe_message: PipeMessage) -> bool {
            // The focus-key channel: a keybind pipes `FOCUS_SIDEBAR_PIPE` and the
            // plugin runs `rimz sidebar focus --toggle`, reaching the sidebar
            // from any pane — a Zellij keybind cannot focus a pane by id itself.
            if pipe_message.name == FOCUS_SIDEBAR_PIPE {
                self.run_focus_sidebar();
                return false;
            }
            if pipe_message.name == SHARE_SESSION_PIPE {
                self.share_requested = true;
                // This can be dropped while the upgraded StartWebServer grant
                // is pending; PermissionRequestResult(Granted) replays it.
                share_current_session();
                return false;
            }
            // Otherwise the launch channel: rimz loads this plugin via `zellij
            // pipe --plugin`, the one load verb that works on a clientless
            // session. The message carries nothing — loading was the point — and
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
            self.apply_runtime_reconfigure();
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
            let hash = policy::manifest_hash(&self.tabs);
            if let Some(policy) = self.policy.as_mut() {
                policy.on_manifest(hash, now);
            }
        }

        /// Resolve a switched-tab focus classification and publish the exact
        /// overlay the renderer needs before the next producer pull.
        fn resolve_focus_correction(&mut self, now: u64, manifest_fresh: bool) {
            match self.focus_correction.resolve(
                &self.tabs,
                self.active_tab,
                self.session_focused_pane,
                manifest_fresh,
                now,
            ) {
                CorrectionAction::StrandedSidebar(pane_id) => {
                    self.session_focused_pane = Some(pane_id);
                    self.run_wake(wire::WakeRequest::FocusStranded { pane_id }, now);
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
                    if self.run_wake(wire::WakeRequest::FocusChanged { patch }, now) {
                        let hash = policy::manifest_hash(&self.tabs);
                        if let Some(policy) = self.policy.as_mut() {
                            policy.accept_manifest(hash);
                            policy.on_optimistic_signal(now);
                        }
                    } else {
                        self.signal_change(now);
                    }
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

        /// Dispatch a policy poke through the wire argv builder. Fire-and-forget:
        /// a failed wake means no stamp, and the producer degrades to poll mode
        /// on its own.
        fn poke(&self, poke: Poke, now: u64) {
            if !self.granted {
                return;
            }
            let request = match poke {
                Poke::Changed => wire::WakeRequest::Changed,
                Poke::Alive => wire::WakeRequest::Alive(wire::PluginTelemetry {
                    mem_pages: wasm_pages(),
                    uptime_ms: now.saturating_sub(self.loaded_at_ms),
                    commands_completed: self.commands_completed,
                    zellij_version: get_zellij_version(),
                }),
            };
            self.run_wake(request, now);
        }

        fn wake_context(&self) -> wire::WakeContext<'_> {
            wire::WakeContext {
                rimz_bin: self.rimz_bin.as_deref(),
                workspace_id: self.workspace_id.as_deref(),
                session_name: self.session_name.as_deref(),
            }
        }

        fn run_wake(&self, request: wire::WakeRequest, now: u64) -> bool {
            if !self.granted {
                return false;
            }
            let focused = policy::resolved_focused_pane_id(
                &self.tabs,
                self.active_tab,
                self.session_focused_pane,
            );
            let topology = wire::topology_json(
                self.session_name.as_deref(),
                now,
                focused,
                &self.tabs,
                &self.foreground,
            );
            let Some(argv) = wire::wake_argv(&self.wake_context(), request, topology.as_deref())
            else {
                return false;
            };
            let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
            run_command(&refs, BTreeMap::new());
            true
        }

        /// Apply rimz-owned runtime config in one `reconfigure(..., false)`:
        /// the mouse booleans are top-level options, and the focus chord (when
        /// configured and parseable) pipes [`FOCUS_SIDEBAR_PIPE`] to this
        /// plugin from any pane in normal or locked mode. Runtime-only: it never
        /// touches the user's `config.kdl` and resets when the session ends.
        /// Requires the Reconfigure grant; a refused call is a harmless no-op
        /// that leaves birth-time options and any hand-written keybind as the
        /// fallback.
        fn apply_runtime_reconfigure(&self) {
            let config = wire::RuntimeReconfigure {
                plugin_id: self.plugin_id,
                focus_key: self.focus_key.as_deref(),
                focus_follows_mouse: self.focus_follows_mouse,
                mouse_click_through: self.mouse_click_through,
            };
            if let Some(kdl) = wire::runtime_reconfigure_kdl(&config) {
                reconfigure(kdl, false);
            }
        }

        /// Run `rimz sidebar focus --toggle` for the focus-key pipe. The command
        /// resolves and focuses the room's sidebar pane (or toggles back), built
        /// from the session name the wake poke uses. Fire-and-forget through
        /// the granted `RunCommands` capability.
        fn run_focus_sidebar(&self) {
            if !self.granted {
                return;
            }
            let argv = wire::focus_sidebar_argv(&self.wake_context());
            let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
            run_command(&refs, BTreeMap::new());
        }

        fn set_foreground_command(&mut self, pane_id: &PaneId, command: Option<String>) {
            let PaneId::Terminal(id) = pane_id else {
                return;
            };
            match command.as_ref() {
                Some(command) => {
                    self.foreground.insert(*id, command.clone());
                }
                None => {
                    self.foreground.remove(id);
                }
            }
            for pane in self.tabs.values_mut().flatten() {
                if !pane.is_plugin && pane.id == *id {
                    pane.pane_command = command;
                    return;
                }
            }
        }

        fn optimistic_command_poke_pane(&self, pane_id: &PaneId, now: u64) -> Option<u32> {
            let PaneId::Terminal(id) = pane_id else {
                return None;
            };
            self.policy
                .as_ref()
                .is_some_and(|policy| policy.optimistic_pane_poke_allowed(*id, now))
                .then_some(*id)
        }

        fn remove_pane(&mut self, pane_id: &PaneId) {
            let (is_plugin, id) = match pane_id {
                PaneId::Terminal(id) => (false, *id),
                PaneId::Plugin(id) => (true, *id),
            };
            policy::remove_pane_from_tabs(&mut self.tabs, is_plugin, id);
            if !is_plugin {
                self.foreground.remove(&id);
                if let Some(policy) = self.policy.as_mut() {
                    policy.forget_pane(id);
                }
            }
        }
    }

    fn focused_patch_id(patch: &[FocusPatch]) -> Option<u32> {
        patch
            .iter()
            .find(|patch| patch.is_focused)
            .map(|patch| patch.id)
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
                        is_floating: pane.is_floating,
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

    fn raw_manifest_stable_hash(
        manifest: &PaneManifest,
        tab_names: &BTreeMap<usize, String>,
    ) -> u64 {
        policy::raw_stable_hash(manifest.panes.iter().flat_map(|(tab, panes)| {
            let tab_name = tab_names.get(tab).map(String::as_str);
            panes.iter().map(move |pane| {
                (
                    *tab,
                    RawStablePaneFields {
                        id: pane.id,
                        is_plugin: pane.is_plugin,
                        is_focused: pane.is_focused,
                        is_suppressed: pane.is_suppressed,
                        is_floating: pane.is_floating,
                        exited: pane.exited,
                        is_held: pane.is_held,
                        tab_position: *tab as u64,
                        tab_name,
                        pane_x: Some(pane.pane_x as u64),
                        pane_columns: Some(pane.pane_columns as u64),
                        terminal_command: pane.terminal_command.as_deref(),
                    },
                )
            })
        }))
    }

    fn tab_names(tabs: &[TabInfo]) -> BTreeMap<usize, String> {
        tabs.iter()
            .map(|tab| (tab.position, tab.name.clone()))
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

    fn wasm_pages() -> u64 {
        core::arch::wasm32::memory_size(0) as u64
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
