//! The Zellij plugin shell: projects host events into the pure policy core,
//! runs the pokes it returns, and corrects a switched-to tab whose remembered
//! focus is Rimz's sidebar. Headless — it renders nothing and holds no pane.
//! Compiled only for wasm (`register_plugin!` defines the wasip1 `main`); host
//! targets build a stub so `--workspace` builds, lints, and the policy unit
//! tests run without the wasm toolchain.

#[cfg(target_family = "wasm")]
mod shell {
    use std::collections::BTreeMap;
    use std::time::{SystemTime, UNIX_EPOCH};

    use rimz_presence_zellij::policy::{self, PaneFields, Poke, PokePolicy, TimerGate};
    use zellij_tile::prelude::*;

    #[derive(Default)]
    pub struct State {
        /// `None` until `load` runs (the `Default` the macro requires).
        policy: Option<PokePolicy>,
        /// The projected room shape, refreshed per manifest event.
        tabs: BTreeMap<usize, Vec<PaneFields>>,
        active_tab: Option<usize>,
        /// Set on active-tab changes, then consumed once a pane manifest proves
        /// whether Zellij restored that tab's focus to Rimz's sidebar.
        pending_focus_tab: Option<usize>,
        /// Configuration written by rimz at load time (never user config):
        /// the workspace to poke and the absolute rimz binary, insulating the
        /// poke from the host PATH. Absent (a hand-loaded plugin), the wake
        /// CLI resolves the workspace from the host cwd ladder.
        workspace_id: Option<String>,
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
            self.rimz_bin = configuration.get("rimz_bin").cloned();
            request_permission(&[
                PermissionType::ReadApplicationState,
                PermissionType::ChangeApplicationState,
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
                    self.tabs = project(&manifest);
                    self.fold(now);
                    self.correct_switched_tab_focus();
                }
                Event::TabUpdate(tabs) => {
                    self.mark_granted(now);
                    let next_active = tabs.iter().find(|tab| tab.active).map(|tab| tab.position);
                    if next_active != self.active_tab {
                        self.pending_focus_tab = next_active;
                    }
                    self.active_tab = next_active;
                    self.correct_switched_tab_focus();
                }
                Event::CommandChanged(_, _, is_foreground, _) => {
                    if is_foreground {
                        self.signal_change(now);
                    }
                }
                Event::PaneClosed(_) => {
                    self.mark_granted(now);
                    self.signal_change(now);
                }
                Event::Timer(_) => {
                    self.timer_gate.on_fire(now);
                }
                _ => {}
            }
            self.dispatch_due(now);
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
                self.poke(Poke::Alive);
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

        /// Zellij remembers per-tab focus. When the user switches back to a tab
        /// whose remembered focus is the sidebar, redirect once to that tab's
        /// first live working pane. Ordinary same-tab sidebar focus is left
        /// alone so the native renderer remains interactive.
        fn correct_switched_tab_focus(&mut self) {
            let Some(tab) = self.pending_focus_tab else {
                return;
            };
            if self.active_tab != Some(tab) || !self.tabs.contains_key(&tab) {
                return;
            }
            if let Some(target) = policy::switched_tab_focus_target(&self.tabs, Some(tab)) {
                focus_terminal_pane(target, false, false);
            }
            self.pending_focus_tab = None;
        }

        fn dispatch_due(&mut self, now: u64) {
            let Some(policy) = self.policy.as_mut() else {
                return;
            };
            for poke in policy.due(now) {
                self.poke(poke);
            }
        }

        /// Arm one host timer for the policy's next deadline, deduplicated by
        /// the [`TimerGate`]: event bursts arm one timer, an earlier deadline
        /// supersedes, and a superseded timer's fire is a harmless no-op.
        fn rearm(&mut self, now: u64) {
            let Some(policy) = self.policy.as_ref() else {
                return;
            };
            let at = policy.next_wake_at();
            if self.timer_gate.should_arm(at) {
                set_timeout(at.saturating_sub(now) as f64 / 1_000.0);
            }
        }

        /// One fixed argv per poke — the whole host-side surface of this
        /// plugin. Fire-and-forget: a failed wake means no stamp, and the
        /// producer degrades to poll mode on its own.
        fn poke(&self, poke: Poke) {
            if !self.granted {
                return;
            }
            let reason = match poke {
                Poke::Changed => "panes-changed",
                Poke::Alive => "alive",
            };
            let program = self.rimz_bin.as_deref().unwrap_or("rimz");
            let mut argv = vec![program, "sidebar", "wake", "--reason", reason];
            if let Some(workspace_id) = self.workspace_id.as_deref() {
                argv.extend(["--workspace-id", workspace_id]);
            }
            run_command(&argv, BTreeMap::new());
        }
    }

    fn project(manifest: &PaneManifest) -> BTreeMap<usize, Vec<PaneFields>> {
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
                        title: pane.title.clone(),
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
