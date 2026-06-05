//! The Zellij plugin shell: projects host events into the pure policy core
//! and executes the pokes it returns. Headless — it renders nothing, holds no
//! pane, and its one side effect is running the fixed `rimz sidebar wake`
//! argv on the host. Compiled only for wasm (`register_plugin!` defines the
//! wasip1 `main`); host targets build a stub so `--workspace` builds, lints,
//! and the policy unit tests run without the wasm toolchain.

#[cfg(target_family = "wasm")]
mod shell {
    use std::collections::BTreeMap;
    use std::time::{SystemTime, UNIX_EPOCH};

    use rimz_presence_zellij::policy::{self, PaneFields, Poke, PokePolicy};
    use zellij_tile::prelude::*;

    #[derive(Default)]
    pub struct State {
        /// `None` until `load` runs (the `Default` the macro requires).
        policy: Option<PokePolicy>,
        /// The projected room shape, refreshed per manifest event.
        tabs: BTreeMap<usize, Vec<PaneFields>>,
        active_tab: Option<usize>,
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
        /// The absolute deadline a host timer is already armed for, so a
        /// burst of events arms one timer, not one per event.
        armed_for: Option<u64>,
    }

    impl ZellijPlugin for State {
        fn load(&mut self, configuration: BTreeMap<String, String>) {
            self.workspace_id = configuration.get("workspace_id").cloned();
            self.rimz_bin = configuration.get("rimz_bin").cloned();
            request_permission(&[
                PermissionType::ReadApplicationState,
                PermissionType::RunCommands,
            ]);
            subscribe(&[
                EventType::PaneUpdate,
                EventType::TabUpdate,
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
                    // First grant: one immediate keepalive flips the producer
                    // into event mode now rather than after the first cadence.
                    if !self.granted {
                        self.granted = true;
                        self.poke(Poke::Alive);
                    }
                }
                Event::PermissionRequestResult(PermissionStatus::Denied) => {
                    self.granted = false;
                }
                Event::PaneUpdate(manifest) => {
                    // Application state flowing proves the (possibly cached)
                    // grant covers us even when no explicit result arrived.
                    if !self.granted {
                        self.granted = true;
                        self.poke(Poke::Alive);
                    }
                    self.tabs = project(&manifest);
                    self.fold(now);
                }
                Event::TabUpdate(tabs) => {
                    if !self.granted {
                        self.granted = true;
                        self.poke(Poke::Alive);
                    }
                    self.active_tab = tabs.iter().find(|tab| tab.active).map(|tab| tab.position);
                    self.fold(now);
                }
                Event::Timer(_) => {
                    self.armed_for = None;
                }
                _ => {}
            }
            self.dispatch_due(now);
            self.rearm(now);
            false // headless: never render
        }
    }

    impl State {
        /// Fold the current projected shape into the policy.
        fn fold(&mut self, now: u64) {
            let hash = policy::manifest_hash(&self.tabs, self.active_tab);
            if let Some(policy) = self.policy.as_mut() {
                policy.on_manifest(hash, now);
            }
        }

        fn dispatch_due(&mut self, now: u64) {
            let Some(policy) = self.policy.as_mut() else {
                return;
            };
            for poke in policy.due(now) {
                self.poke(poke);
            }
        }

        /// Arm one host timer for the policy's next deadline. Tracking the
        /// armed deadline dedupes timers under event bursts; an earlier
        /// deadline supersedes (the stale timer fires as a harmless no-op).
        fn rearm(&mut self, now: u64) {
            let Some(policy) = self.policy.as_ref() else {
                return;
            };
            let at = policy.next_wake_at();
            if self.armed_for.is_none_or(|armed| at < armed) {
                set_timeout(at.saturating_sub(now) as f64 / 1_000.0);
                self.armed_for = Some(at);
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
                        exited: pane.exited,
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
