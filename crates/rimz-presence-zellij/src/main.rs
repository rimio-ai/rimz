//! The Zellij plugin shell: projects host events into the pure engine, executes
//! returned effects, and renders nothing. Compiled only for wasm
//! (`register_plugin!` defines the wasip1 `main`); host targets build a stub so
//! `--workspace` builds, lints, and host unit tests run without the wasm
//! toolchain.

#[cfg(target_family = "wasm")]
mod shell {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use rimz_presence_zellij::engine::{
        Effect, Engine, EngineConfig, Host, ProjectedClientFocus, ProjectedPaneId,
    };
    use rimz_presence_zellij::policy::{self, PaneFields, RawStablePaneFields};
    use rimz_presence_zellij::wire::{
        self, DUMP_TOPOLOGY_PIPE, FOCUS_SIDEBAR_PIPE, RETIRE_PIPE, SHARE_SESSION_PIPE,
    };
    use zellij_tile::prelude::*;

    #[derive(Default)]
    pub struct State {
        /// `None` until `load` runs (the `Default` the macro requires).
        engine: Option<Engine>,
        /// The shell owns command counters and feeds them into telemetry.
        commands: wire::CommandCounters,
        /// Zellij answers this through plugin stdio, so cache it at load.
        zellij_version: String,
    }

    struct ShellHost<'a> {
        commands: wire::CommandCounters,
        zellij_version: &'a str,
    }

    impl Host for ShellHost<'_> {
        fn pane_pid(&self, pane_id: u32) -> Option<u32> {
            get_pane_pid(PaneId::Terminal(pane_id))
                .ok()
                .and_then(|pid| u32::try_from(pid).ok())
        }

        fn telemetry(&self) -> wire::PluginTelemetry {
            wire::PluginTelemetry {
                plugin_id: None,
                loaded_at_ms: 0,
                mem_pages: wasm_pages(),
                uptime_ms: 0,
                commands_completed: self.commands.completed,
                commands_succeeded: self.commands.succeeded,
                commands_failed: self.commands.failed(),
                stale_writer_rejections: self.commands.stale_writer_rejections,
                topology_failures: self.commands.topology_failures,
                other_failures: self.commands.other_failures,
                zellij_version: self.zellij_version.to_owned(),
            }
        }
    }

    impl ZellijPlugin for State {
        fn load(&mut self, configuration: BTreeMap<String, String>) {
            let permissions = vec![
                PermissionType::ReadApplicationState,
                PermissionType::RunCommands,
                PermissionType::Reconfigure,
                PermissionType::StartWebServer,
            ];
            request_permission(&permissions);
            subscribe(&subscribed_events());
            let now = now_ms();
            self.zellij_version = get_zellij_version();
            let config = EngineConfig {
                workspace_id: configuration.get("workspace_id").cloned(),
                session_name: configuration.get("session_name").cloned(),
                rimz_bin: configuration.get("rimz_bin").cloned(),
                plugin_id: Some(get_plugin_ids().plugin_id),
                plugin_build: configuration.get("plugin_build").cloned(),
                plugin_config: configuration.get("plugin_config").cloned(),
                focus_key: configuration.get("focus_key").cloned(),
                focus_follows_mouse: wire::parse_configuration_bool(
                    configuration.get("focus_follows_mouse").map(String::as_str),
                ),
                mouse_click_through: wire::parse_configuration_bool(
                    configuration.get("mouse_click_through").map(String::as_str),
                ),
            };
            let mut engine = Engine::new(now, config);
            let host = ShellHost {
                commands: self.commands,
                zellij_version: &self.zellij_version,
            };
            execute(engine.on_load(now, &host));
            self.engine = Some(engine);
        }

        fn update(&mut self, event: Event) -> bool {
            let now = now_ms();
            if let Event::RunCommandResult(exit_code, _, _, context) = event {
                let published_topology = context
                    .get(wire::TOPOLOGY_PUBLISH_CONTEXT)
                    .is_some_and(|value| value == "1");
                self.commands.record(exit_code, published_topology);
                let Some(engine) = self.engine.as_mut() else {
                    return false;
                };
                let host = ShellHost {
                    commands: self.commands,
                    zellij_version: &self.zellij_version,
                };
                execute(engine.on_run_command_result(exit_code, published_topology, now, &host));
                return false;
            }
            let Some(engine) = self.engine.as_mut() else {
                return false;
            };
            let host = ShellHost {
                commands: self.commands,
                zellij_version: &self.zellij_version,
            };
            let effects = match event {
                Event::PermissionRequestResult(PermissionStatus::Granted) => {
                    engine.on_permission_granted(now, &host)
                }
                Event::PermissionRequestResult(PermissionStatus::Denied) => {
                    engine.on_permission_denied(now, &host)
                }
                Event::PaneUpdate(manifest) => {
                    let raw_hash = raw_manifest_stable_hash(&manifest, engine.tab_names());
                    engine.on_pane_manifest(
                        raw_hash,
                        |tab_names| project(&manifest, tab_names),
                        now,
                        &host,
                    )
                }
                Event::TabUpdate(tabs) => {
                    let active = tabs.iter().find(|tab| tab.active).map(|tab| tab.position);
                    engine.on_tab_update(active, tab_names(&tabs), now, &host)
                }
                Event::CommandChanged(pane_id, command, is_foreground, _) => engine
                    .on_command_changed(
                        project_pane_id(pane_id),
                        command,
                        is_foreground,
                        now,
                        &host,
                    ),
                Event::CwdChanged(pane_id, path, _) => engine.on_cwd_changed(
                    project_pane_id(pane_id),
                    path.into_os_string()
                        .into_string()
                        .ok()
                        .filter(|cwd| !cwd.is_empty()),
                    now,
                    &host,
                ),
                Event::PaneClosed(pane_id) => {
                    engine.on_pane_closed(project_pane_id(pane_id), now, &host)
                }
                Event::Timer(_) => engine.on_timer(now, &host),
                Event::SessionUpdate(sessions, _) => {
                    let connected_clients =
                        own_session_connected_clients(engine.session_name(), &sessions);
                    engine.on_session_update(connected_clients, now, &host)
                }
                Event::ListClients(clients) => {
                    engine.on_list_clients(project_clients(&clients), now, &host)
                }
                _ => Vec::new(),
            };
            execute(effects);
            false // headless: never render
        }

        fn pipe(&mut self, pipe_message: PipeMessage) -> bool {
            let now = now_ms();
            let Some(engine) = self.engine.as_mut() else {
                return false;
            };
            // The focus-key channel: a keybind pipes `FOCUS_SIDEBAR_PIPE` and
            // the plugin runs `rimz sidebar focus --toggle`, reaching the
            // sidebar from any pane — a Zellij keybind cannot focus a pane by
            // id itself.
            if pipe_message.name == FOCUS_SIDEBAR_PIPE {
                execute(engine.on_focus_sidebar_pipe());
                return false;
            }
            if pipe_message.name == SHARE_SESSION_PIPE {
                execute(engine.on_share_session_pipe());
                return false;
            }
            if pipe_message.name == DUMP_TOPOLOGY_PIPE {
                let host = ShellHost {
                    commands: self.commands,
                    zellij_version: &self.zellij_version,
                };
                execute(engine.on_dump_topology_pipe(now, &host));
                return false;
            }
            if pipe_message.name == RETIRE_PIPE {
                execute(engine.on_retire_pipe(pipe_message.payload.as_deref()));
                return false;
            }
            // Otherwise the launch channel: rimz loads this plugin via `zellij
            // pipe --plugin`, the one load verb that works on a clientless
            // session. The message carries nothing — loading was the point —
            // and delivery itself releases the CLI (an explicit
            // `unblock_cli_pipe_input` would need a third permission,
            // `ReadCliPipes`, for nothing). The CLI blocks only while the
            // launch is permission-pending, which rimz bounds with its command
            // deadline.
            false // headless: never render
        }
    }

    fn execute(effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::RunCommand(argv) => {
                    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
                    let mut context = BTreeMap::new();
                    if wire::publishes_topology(&argv) {
                        context.insert(wire::TOPOLOGY_PUBLISH_CONTEXT.to_owned(), "1".to_owned());
                    }
                    run_command_with_env_variables_and_cwd(
                        &refs,
                        BTreeMap::new(),
                        PathBuf::from("/"),
                        context,
                    );
                }
                Effect::HideSelf => hide_self(),
                Effect::Reconfigure(kdl) => reconfigure(kdl, false),
                Effect::ShareSession => share_current_session(),
                Effect::CloseSelf => close_self(),
                Effect::Unsubscribe => unsubscribe(&subscribed_events()),
                Effect::Resubscribe => subscribe(&subscribed_events()),
                Effect::SetTimeout(delay_ms) => set_timeout(delay_ms as f64 / 1_000.0),
                Effect::ListClients => list_clients(),
            }
        }
    }

    fn subscribed_events() -> [EventType; 10] {
        [
            EventType::PaneUpdate,
            EventType::TabUpdate,
            EventType::CommandChanged,
            EventType::CwdChanged,
            EventType::PaneClosed,
            EventType::Timer,
            EventType::PermissionRequestResult,
            EventType::RunCommandResult,
            EventType::SessionUpdate,
            EventType::ListClients,
        ]
    }

    fn project_pane_id(pane_id: PaneId) -> ProjectedPaneId {
        match pane_id {
            PaneId::Terminal(id) => ProjectedPaneId::Terminal(id),
            PaneId::Plugin(id) => ProjectedPaneId::Plugin(id),
        }
    }

    fn own_session_connected_clients(
        session_name: Option<&str>,
        sessions: &[SessionInfo],
    ) -> Option<usize> {
        session_name
            .and_then(|name| {
                sessions
                    .iter()
                    .find(|session| session.name == name)
                    .map(|session| session.connected_clients)
            })
            .or_else(|| {
                sessions
                    .iter()
                    .find(|session| session.is_current_session)
                    .map(|session| session.connected_clients)
            })
    }

    fn project_clients(clients: &[ClientInfo]) -> Vec<ProjectedClientFocus> {
        clients
            .iter()
            .filter_map(|client| match client.pane_id {
                PaneId::Terminal(pane_id) => Some(ProjectedClientFocus {
                    client_id: client.client_id,
                    pane_id,
                }),
                PaneId::Plugin(_) => None,
            })
            .collect()
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
                    .map(|pane| {
                        PaneFields::from_stable(
                            &stable_fields(*tab, pane, tab_names),
                            pane.title.clone(),
                        )
                    })
                    .collect();
                // Deterministic order regardless of the host map's iteration;
                // terminal and plugin panes have separate id spaces.
                fields.sort_unstable_by_key(|pane| (pane.is_plugin, pane.id));
                (*tab, fields)
            })
            .collect()
    }

    fn stable_fields<'a>(
        tab: usize,
        pane: &'a PaneInfo,
        tab_names: &'a BTreeMap<usize, String>,
    ) -> RawStablePaneFields<'a> {
        RawStablePaneFields {
            id: pane.id,
            is_plugin: pane.is_plugin,
            is_focused: pane.is_focused,
            is_suppressed: pane.is_suppressed,
            is_floating: pane.is_floating,
            exited: pane.exited,
            is_held: pane.is_held,
            tab_position: tab as u64,
            tab_name: tab_names.get(&tab).map(String::as_str),
            pane_x: Some(pane.pane_x as u64),
            pane_columns: Some(pane.pane_columns as u64),
            terminal_command: pane.terminal_command.as_deref(),
        }
    }

    fn raw_manifest_stable_hash(
        manifest: &PaneManifest,
        tab_names: &BTreeMap<usize, String>,
    ) -> u64 {
        policy::raw_stable_hash(manifest.panes.iter().flat_map(|(tab, panes)| {
            panes
                .iter()
                .map(move |pane| (*tab, stable_fields(*tab, pane, tab_names)))
        }))
    }

    fn tab_names(tabs: &[TabInfo]) -> BTreeMap<usize, String> {
        tabs.iter()
            .map(|tab| (tab.position, tab.name.clone()))
            .collect()
    }

    /// Unix milliseconds via the WASI clock. The policy only compares relative
    /// instants, so coarse wall-clock is plenty.
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
