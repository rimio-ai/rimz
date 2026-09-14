# Zellij upstream reference

This page mirrors the Zellij surfaces RimZ binds to: the wasm plugin API (lifecycle, events, host commands, types, permissions, workers, pipes, keybind actions), the CLI control surface, configuration options and their merge rules, mouse handling, the kitty graphics protocol, layout KDL, and session serialization. It records what upstream ships. How RimZ maps these surfaces onto its own backend, including which events, host commands, and verbs it wires, is in [multiplexers.md](../../internals/multiplexers.md): the [Zellij backend](../../internals/multiplexers.md#zellij-backend), its [caveats](../../internals/multiplexers.md#zellij-backend-caveats), and [the Zellij presence plugin](../../internals/multiplexers.md#the-zellij-presence-plugin). Browser access is in [web.md](../../internals/web.md).

**Baseline.** Zellij **0.45.1** (tag [`v0.45.1`](https://github.com/zellij-org/zellij/releases/tag/v0.45.1), commit `efd8fd5a89a20c07a111d248ad7fce53848d2c18`, released 2026-08-28). Docs and source were read 2026-09-14, and the host binary (`zellij --version` prints `zellij 0.45.1`) matches the baseline. Source anchors are paths under <https://github.com/zellij-org/zellij/tree/v0.45.1/> plus a symbol. Claims marked "observed on 0.44.3" are live captures from that release that the 0.45.1 source does not contradict. Version boundaries that matter below 0.45: `CwdChanged` exists at 0.44.0, `CommandChanged` first ships in 0.44.2, and `new-pane --no-focus` and the `stacked_pane_list` option first ship in 0.45.0.

## Upstream sources

The type definitions live in `zellij-utils/src/data.rs` and the plugin shims in `zellij-tile/src/shim.rs`, published as [`zellij-tile` on docs.rs](https://docs.rs/zellij-tile/latest/zellij_tile/). The website lags the source in places (for example, its permissions page lists 14 of the 17 permissions); each disagreement that matters is flagged at its claim, and the page follows the source and the binary.

| Surface | Source |
| --- | --- |
| Release and changelog | <https://github.com/zellij-org/zellij/releases/tag/v0.45.1>, [`CHANGELOG.md`](https://github.com/zellij-org/zellij/blob/v0.45.1/CHANGELOG.md) |
| Plugin events | <https://zellij.dev/documentation/plugin-api-events.html>, `zellij-server/src/plugins/wasm_bridge.rs` `check_event_permission` |
| Plugin commands | <https://zellij.dev/documentation/plugin-api-commands.html>, `zellij-tile/src/shim.rs`, `zellij-server/src/plugins/zellij_exports.rs` `check_command_permission` |
| Plugin types | <https://zellij.dev/documentation/plugin-api-types.html>, <https://docs.rs/zellij-tile/latest/zellij_tile/> |
| Plugin workers | <https://zellij.dev/documentation/plugin-api-workers.html> |
| Plugin lifecycle | <https://zellij.dev/documentation/plugin-lifecycle.html> |
| Plugin permissions | <https://zellij.dev/documentation/plugin-api-permissions.html>, `zellij-utils/src/plugin_api/plugin_permission.proto` |
| Plugin loading and aliases | <https://zellij.dev/documentation/plugin-loading.html>, <https://zellij.dev/documentation/plugin-aliases.html> |
| Pipes | <https://zellij.dev/documentation/zellij-plugin-and-pipe.html>, <https://zellij.dev/documentation/plugin-pipes.html> |
| CLI control | <https://zellij.dev/documentation/controlling-zellij-through-cli.html>, <https://zellij.dev/documentation/cli-actions.html>, <https://zellij.dev/documentation/zellij-run-and-edit.html>, <https://zellij.dev/documentation/cli-recipes.html>, `zellij-utils/src/cli.rs` |
| Configuration and options | <https://zellij.dev/documentation/configuration.html>, <https://zellij.dev/documentation/options.html>, <https://zellij.dev/documentation/command-line-options.html>, `zellij-utils/src/input/options.rs` |
| Kitty graphics | `zellij-server/src/panes/kitty_graphics/`, `zellij-integration-tests/tests/kitty_graphics.rs` |
| Layout KDL | <https://zellij.dev/documentation/creating-a-layout.html>, `zellij-utils/src/kdl/kdl_layout_parser.rs` |
| Session resurrection | <https://zellij.dev/documentation/session-resurrection.html>, `zellij-server/src/background_jobs.rs` |

## Plugin API

Plugins are wasm32-wasip1 binaries that run inside the Zellij server's plugin host. RimZ ships one, [`rimz-presence-zellij`](../../../crates/rimz-presence-zellij/).

### Lifecycle

A plugin implements the `ZellijPlugin` trait from `zellij_tile::prelude::*` and registers it with `register_plugin!(MyPlugin)`; Zellij constructs the value through `Default`.

```rust
fn load(&mut self, configuration: BTreeMap<String, String>)  // init; subscribe and request_permission here
fn update(&mut self, event: Event) -> bool                   // subscribed events; true = re-render
fn render(&mut self, rows: usize, cols: usize)               // print UI to STDOUT; rows/cols exclude the frame
fn pipe(&mut self, pipe_message: PipeMessage) -> bool        // pipe messages; true = re-render
```

Events arrive asynchronously with no ordering guarantee. `render` runs after `update` or `pipe` returns `true`, and on startup and resize. A headless plugin never draws and calls `hide_self()` once its permissions are granted.

### Events

`Event` in `zellij-utils/src/data.rs` has 48 variants, all listed below. A plugin receives an event only after `subscribe`, and the gate column is the permission `check_event_permission` requires at delivery. Built-in plugins (`zellij:` URLs) bypass every gate.

| Event | Payload | Delivery gate |
| --- | --- | --- |
| `ModeUpdate` | `ModeInfo` | ReadApplicationState |
| `TabUpdate` | `Vec<TabInfo>` | ReadApplicationState |
| `PaneUpdate` | `PaneManifest` | ReadApplicationState |
| `Key` | `KeyWithModifier`, keys sent to the plugin's own pane | none |
| `Mouse` | `Mouse`, events on the plugin's own pane | none |
| `Timer` | `f64` seconds elapsed, the reply to `set_timeout` | none |
| `CopyToClipboard` | `CopyDestination` | ReadApplicationState |
| `SystemClipboardFailure` | none | ReadApplicationState |
| `InputReceived` | none (some input arrived; which input is unspecified) | ReadApplicationState |
| `Visible` | `bool` | none |
| `CustomMessage` | `(message: String, payload: String)` from a worker | none |
| `FileSystemCreate`, `FileSystemRead`, `FileSystemUpdate`, `FileSystemDelete` | `Vec<(PathBuf, Option<FileMetadata>)>`, after `watch_filesystem()` | none |
| `PermissionRequestResult` | `PermissionStatus` (`Granted` or `Denied`) | none |
| `SessionUpdate` | `Vec<SessionInfo>`, plus `Vec<(String, Duration)>` resurrectable sessions | ReadApplicationState |
| `RunCommandResult` | `Option<i32>` exit code, `Vec<u8>` stdout, `Vec<u8>` stderr, `Context` | none |
| `WebRequestResult` | `u16` status, `BTreeMap` headers, `Vec<u8>` body, `Context` | none |
| `CommandPaneOpened` | `u32` terminal pane id, `Context` | ReadApplicationState |
| `CommandPaneExited` | `u32`, `Option<i32>` exit code, `Context`; the pane stays open | ReadApplicationState |
| `PaneClosed` | `PaneId` | ReadApplicationState |
| `EditPaneOpened`, `EditPaneExited` | `u32` (plus `Option<i32>` on exit), `Context` | ReadApplicationState |
| `CommandPaneReRun` | `u32`, `Context` | ReadApplicationState |
| `FailedToWriteConfigToDisk` | `Option<String>` file path | ReadApplicationState |
| `ListClients` | `Vec<ClientInfo>`, the reply to `list_clients()` | none at delivery; the `list_clients()` command needs ReadApplicationState |
| `HostFolderChanged`, `FailedToChangeHostFolder` | `PathBuf`, `Option<String>` error | none |
| `PastedText` | `String` | none |
| `ConfigWasWrittenToDisk` | none | none |
| `WebServerStatus` | `WebServerStatus` (`Online(url)`, `Offline`, `DifferentVersion(v)`) | StartWebServer |
| `FailedToStartWebServer` | `String` error | none |
| `BeforeClose` | none; fires before the plugin unloads | none |
| `InterceptedKeyPress` | `KeyWithModifier`, consumed instead of processed | none at delivery; `intercept_key_presses()` needs InterceptInput |
| `UserAction` | `Action`, `ClientId`, `Option<u32>` terminal id, `Option<ClientId>` CLI client | InterceptInput |
| `PaneRenderReport` | `HashMap<PaneId, PaneContents>`, periodic pane contents with ANSI | ReadPaneContents |
| `ActionComplete` | `Action`, `Option<PaneId>`, `Context`, the reply to `run_action` | none at delivery; `run_action` needs RunActionsAsUser |
| `CwdChanged` | `PaneId`, `PathBuf` new cwd, `Vec<ClientId>` focused clients | ReadApplicationState |
| `CommandChanged` | `PaneId`, `Vec<String>` argv, `bool` is_foreground, `Vec<ClientId>` focused clients | ReadApplicationState |
| `AvailableLayoutInfo` | `Vec<LayoutInfo>`, `Vec<LayoutWithError>` | ReadApplicationState |
| `PluginConfigurationChanged` | `BTreeMap<String, String>` new configuration | ReadApplicationState |
| `HighlightClicked` | `{ pane_id, pattern, matched_string, context }`; `matched_string` is capture group 1 when present | ReadApplicationState |
| `InitialKeybinds` | `KeybindsVec` | none |
| `HostTerminalThemeChanged` | `HostTerminalThemeMode` (`Dark` or `Light`), from CSI 2031 or DSR 997 | none |
| `SoftKeyboardVisibilityChanged` | `bool` | ReadApplicationState |
| `HintText` | `BTreeMap<usize, StyledText>` | ReadApplicationState |
| `ActivePaneScroll` | `Option<(usize, usize)>` | ReadApplicationState |

`Context` is a `BTreeMap<String, String>` the caller passes to an async command and Zellij echoes on the matching reply event; it is how a plugin correlates replies with requests. `ListClients` has no `Context`, so its replies cannot be matched to a request.

`CwdChanged` and `CommandChanged` come from one server-side probe. Every second (`UPDATE_AND_REPORT_CWDS_INTERVAL_MS` in `zellij-server/src/background_jobs.rs`), `Pty::update_and_report_cwds` in `zellij-server/src/pty.rs` inspects only the terminal panes that produced output since the last tick. It reads each pane's root-process cwd and emits `CwdChanged` when it differs from the last value, then discovers the foreground command (with `post_command_discovery_hook` applied) and emits `CommandChanged` when that differs. Both events broadcast to every subscribed plugin. A pane that prints nothing is not re-probed, so a change in a silent pane surfaces on its next output.

Four delivery behaviours are easy to miss:

- `Visible` fires for plugins in a tab being hidden or shown, and for plugins in the floating layer when floating panes are toggled (`Tab::notify_floating_plugins_of_visibility` in `zellij-server/src/tab/mod.rs`).
- `HostTerminalThemeChanged` replays the last known host mode to a plugin at the moment it subscribes (`WasmBridge::send_host_terminal_theme_mode_to_plugin`), so a plugin loaded after the host reported still learns the mode.
- A cached permission grant produces no `PermissionRequestResult` on later loads; subscribed state starting to flow is the evidence of a grant (observed on 0.44.3).
- A `SessionUpdate` manifest for the current session can arrive transiently partial while `PaneUpdate`, `list-panes --json --all`, and the serialized session metadata still show the full session (observed on 0.44.3). `PaneUpdate` is the complete roster.

### Commands (host functions)

The shims in `zellij-tile/src/shim.rs` are grouped below by the permission `check_command_permission` requires. Signatures are abbreviated; [docs.rs](https://docs.rs/zellij-tile/latest/zellij_tile/shim/index.html) carries the full forms.

**Ungated.** `subscribe(&[EventType])`, `unsubscribe(&[EventType])`, `request_permission(&[PermissionType])`, `set_selectable(bool)`, `show_cursor(Option<(x, y)>)`, `set_self_mouse_selection_support(bool)`, `get_plugin_ids() -> PluginIds`, `get_zellij_version() -> String`, `set_timeout(secs: f64)` (replies with `Timer`), `hide_self()`, `show_self(float_if_hidden)`, `close_self()`, `focus_last_pane()`, `post_message_to(PluginMessage)` (plugin to worker), `post_message_to_plugin(PluginMessage)` (worker to plugin, delivered as `CustomMessage`), `report_panic(&PanicHookInfo)`, `scan_host_folder(&Path)`, `watch_filesystem()` (enables the `FileSystem*` events). `get_focused_tab(&Vec<TabInfo>)` is a local helper that calls no host function.

**ReadApplicationState.** `generate_random_name()`, `get_layout_dir()`, `get_focused_pane_info() -> Result<(usize, PaneId)>`, `get_pane_info(PaneId) -> Option<PaneInfo>`, `get_tab_info(tab_id) -> Option<TabInfo>`, `get_pane_pid(PaneId) -> Result<i32>`, `get_pane_running_command(PaneId) -> Result<Vec<String>>`, `get_pane_cwd(PaneId) -> Result<PathBuf>`, `get_session_list() -> Result<SessionListSnapshot>`, `list_clients()` (replies with `ListClients`), `save_session()`, `current_session_last_saved_time() -> Option<u64>` in milliseconds, `dump_layout(name)`, `dump_session_layout()` and `dump_session_layout_for_tab(idx)` (KDL plus `LayoutMetadata`), `parse_layout(&str) -> Result<LayoutMetadata, LayoutParsingError>`.

`get_pane_pid`, `get_pane_running_command`, and `get_pane_cwd` are synchronous round trips to the server's PTY thread. `get_pane_pid` returns the pane's root process id. `get_pane_running_command` returns the discovered foreground command and falls back to the root process's argv; `get_pane_cwd` returns the root process's cwd (`Pty::get_pane_running_command`, `Pty::get_pane_cwd`).

**ChangeApplicationState** covers most of the catalog:

- Focus and navigation: `focus_terminal_pane(id, float_if_hidden, in_place_if_hidden)`, `focus_plugin_pane(...)`, `focus_pane_with_id(PaneId, ...)`, `focus_next_pane()`, `focus_previous_pane()`, `move_focus(Direction)`, `move_focus_or_tab(Direction)`, `switch_tab_to(idx)`, `go_to_tab(idx)`, `go_to_tab_name(&str)`, `focus_or_create_tab(&str)`, `toggle_tab()`, `go_to_next_tab()`, `go_to_previous_tab()`, `switch_to_input_mode(&InputMode)`, `focus_host_session()`.
- Pane lifecycle and shape: `new_pane()`, `close_focus()`, `close_terminal_pane(id)`, `close_plugin_pane(id)`, `close_pane_with_id(PaneId)`, `close_multiple_panes(Vec<PaneId>)`, `rename_terminal_pane`, `rename_plugin_pane`, `rename_pane_with_id`, `undo_rename_pane()`, `toggle_focus_fullscreen()`, `toggle_pane_id_fullscreen(PaneId)`, `toggle_focus_no_ui_fullscreen()`, `toggle_pane_frames()`, `set_pane_frame_style(PaneFrameStyle)`, `toggle_pane_embed_or_eject[_for_pane_id]`, `toggle_pane_borderless(PaneId)`, `set_pane_borderless(PaneId, bool)`, `move_pane[_with_direction|_with_pane_id|_with_pane_id_in_direction]`, `replace_pane_with_existing_pane(replace, existing, suppress_replaced)`, `set_floating_pane_pinned(PaneId, bool)`, `stack_panes(Vec<PaneId>)`, `float_multiple_panes`, `embed_multiple_panes`, `change_floating_panes_coordinates(Vec<(PaneId, FloatingPaneCoordinates)>)`, `group_and_ungroup_panes(group, ungroup, for_all_clients)`, `highlight_and_unhighlight_panes(highlight, unhighlight)`, `set_pane_color(PaneId, fg, bg)`, `hide_pane_with_id(PaneId)`, `show_pane_with_id(PaneId, float, focus)`, `toggle_floating_panes(Option<u64> tab_id)`, `show_floating_panes(tab)`, `hide_floating_panes(tab)`, `set_pane_regex_highlights(PaneId, Vec<RegexHighlight>)`, `clear_pane_highlights(PaneId)`, `set_soft_keyboard(bool)`.
- Resize and scroll: `resize_focused_pane(Resize)`, `resize_focused_pane_with_direction(Resize, Direction)`, `resize_pane_with_id(ResizeStrategy, PaneId)`, `scroll_up` and `scroll_down`, `scroll_to_top` and `scroll_to_bottom`, `page_scroll_up` and `page_scroll_down` (each also as `_in_pane_id`), `edit_scrollback[_for_pane_with_id]`, `clear_screen[_for_pane_id]`.
- Tabs: `new_tab(name, cwd) -> Option<usize>`, `new_tab_unfocused(name, cwd) -> Option<usize>`, `new_tabs_with_layout(kdl: &str) -> Vec<usize>`, `new_tabs_with_layout_info(LayoutInfo)`, `close_focused_tab()`, `close_tab_with_index(usize)`, `close_tab_with_id(u64)`, `rename_tab(position, name)`, `rename_tab_with_id(u64, name)`, `undo_rename_tab()`, `toggle_active_tab_sync()`, `break_panes_to_new_tab(ids, name, focus)`, `break_panes_to_tab_with_index`, `break_panes_to_tab_with_id`.
- Sessions: `switch_session(Option<&str>)`, `switch_session_with_layout`, `switch_session_with_cwd`, `switch_session_with_focus(name, tab, (pane_id, is_plugin))`, `rename_session(&str)`, `kill_sessions(&[names])`, `delete_dead_session(name)`, `delete_all_dead_sessions()`, `detach()`, `disconnect_other_clients()`, `quit_zellij()`.
- Plugins, signals, and layouts: `reload_plugin_with_id(u32)`, `load_new_plugin(url, config, in_background, skip_cache)`, `send_sigint_to_pane_id(PaneId)`, `send_sigkill_to_pane_id(PaneId)`, `rerun_command_pane(terminal_id)`, `save_layout(name, kdl, overwrite)`, `delete_layout(name)`, `rename_layout(old, new)`, `edit_layout(name, ctx)`, `override_layout(LayoutInfo, retain_terminals, retain_plugins, active_tab_only, ctx)`, `previous_swap_layout()`, `next_swap_layout()`.

**RunCommands.** `run_command(&[&str], Context)` and `run_command_with_env_variables_and_cwd(cmd, env, cwd, ctx)` (both reply with `RunCommandResult`), the `open_command_pane*` family (`open_command_pane`, `_floating`, `_in_place`, `_near_plugin`, `_floating_near_plugin`, `_in_place_of_plugin(close_after)`, `_in_place_of_pane_id`, `_background`, each `(CommandToRun, [coords,] Context) -> Option<PaneId>`), `open_command_pane_in_new_tab`, and the doc-hidden `exec_cmd(&[&str])`. A background command spawns with the server's environment and the launching CLI's cwd unless the call passes its own. `run_command` and timers keep working with zero clients attached (observed on 0.44.3).

**OpenFiles.** The `open_file*` family mirrors the command-pane variants, each `(FileToOpen, [coords,] Context) -> Option<PaneId>`, plus `open_edit_pane_in_place_of_pane_id` and `open_editor_pane_in_new_tab`.

**OpenTerminalsOrPlugins.** The `open_terminal*` family `(path) -> Option<PaneId>`, `new_tiled_pane_in_tab(tab_position) -> Option<PaneId>`, `open_plugin_pane_floating(url, config, coords, ctx)`, `open_plugin_pane_in_new_tab`, `start_or_reload_plugin(url)`.

**WriteToStdin.** `write(Vec<u8>)` and `write_chars(&str)` to the focused pane; `write_to_pane_id` and `write_chars_to_pane_id` to a given pane.

**Other gates.**

| Permission | Commands |
| --- | --- |
| WebAccess | `web_request(url, HttpVerb, headers, body, ctx)`, replying with `WebRequestResult` |
| WriteToClipboard | `copy_to_clipboard(text)` |
| Reconfigure | `reconfigure(kdl: String, save_to_disk: bool)`, `rebind_keys(unbind, rebind, save)` |
| FullHdAccess | `change_host_folder(PathBuf)`, `list_windows_volumes()` (Windows only; the result arrives as `FileSystemUpdate`) |
| InterceptInput | `intercept_key_presses()`, `clear_key_presses_intercepts()` |
| ReadCliPipes | `block_cli_pipe_input(pipe_id)`, `unblock_cli_pipe_input(pipe_id)`, `cli_pipe_output(pipe_id, output)` |
| MessageAndLaunchOtherPlugins | `pipe_message_to_plugin(MessageToPlugin)` |
| StartWebServer | `start_web_server()`, `stop_web_server()`, `query_web_server_status()`, `share_current_session()`, `stop_sharing_current_session()`, `generate_web_login_token(label, read_only)`, `revoke_web_login_token(label)`, `list_web_login_tokens()`, `revoke_all_web_tokens()`, `rename_web_token(old, new)` |
| RunActionsAsUser | `run_action(Action, Context)`, replying with `ActionComplete` |
| ReadSessionEnvironmentVariables | `get_session_environment_variables()` |
| ReadPaneContents | `get_pane_scrollback(PaneId, full) -> Result<PaneContents>` |

**Focus actuation.** Focus commands act for the client that owns the calling plugin instance: `focus_terminal_pane` routes with the instance's `env.client_id`, so a background plugin instance moves the focus of its own plugin client, not a human client. From the CLI, `action focus-pane-id` can move the visible pane and route later terminal input while `ListClients` keeps reporting the pre-action pane (observed on 0.44.3). Neither path confirms which human client's focus moved.

### Types

The shapes below are from `zellij-utils/src/data.rs` at the baseline; docs.rs has the rest.

```rust
enum PaneId { Terminal(u32), Plugin(u32) }

struct PaneManifest { panes: HashMap<usize /* tab position */, Vec<PaneInfo>> }
```

`PaneInfo` has five field groups:

| Group | Fields |
| --- | --- |
| Identity | `id: u32`, `is_plugin`, `title` |
| State | `is_focused`, `is_fullscreen`, `is_floating`, `is_suppressed`, `is_selectable`, `exited`, `exit_status: Option<i32>`, `is_held` |
| Geometry | `pane_x`, `pane_y`, `pane_rows`, `pane_columns` (including the frame); `pane_content_x`, `pane_content_y`, `pane_content_rows`, `pane_content_columns` (excluding it); `cursor_coordinates_in_pane: Option<(usize, usize)>` |
| Spawn | `terminal_command: Option<String>` (a command pane's launch command, not its live foreground command), `plugin_url: Option<String>` |
| Per-client chrome | `index_in_pane_group: BTreeMap<ClientId, usize>`, `default_fg`, `default_bg` (color strings) |

`PaneInfo` carries no pid, cwd, or live command; those come from the `get_pane_*` commands or the `CwdChanged` and `CommandChanged` events. `is_held` means a command pane waits at the `Press ENTER to run` banner, which is how a resurrected pane looks. `is_focused` is a projected mark with no uniqueness guarantee: a session with one listed client has reported several `is_focused: true` terminal panes in one tab after SSH reconnect churn (observed on 0.44.3). The `(client_id, pane_id)` pairs from `list_clients()` are the per-client focus record, and they include plugin panes.

| Type | Fields (abridged) |
| --- | --- |
| `TabInfo` | `position`, `name`, `active`, `panes_to_hide`, `is_fullscreen_active`, `is_sync_panes_active`, `are_floating_panes_visible`, `other_focused_clients: Vec<ClientId>`, `active_swap_layout_name`, `is_swap_layout_dirty`, `viewport_rows`, `viewport_columns`, `display_area_rows`, `display_area_columns`, `selectable_tiled_panes_count`, `selectable_floating_panes_count`, `tab_id: usize`, `has_bell_notification`, `is_flashing_bell` |
| `SessionInfo` | `name`, `tabs: Vec<TabInfo>`, `panes: PaneManifest`, `connected_clients: usize`, `is_current_session`, `available_layouts`, `plugins: BTreeMap<u32, PluginInfo>`, `web_clients_allowed`, `web_client_count`, `tab_history` and `pane_history: BTreeMap<ClientId, Vec<PaneId>>`, `creation_time` |
| `SessionListSnapshot` | `live_sessions: Vec<SessionInfo>`, `resurrectable_sessions: Vec<(String, Duration)>` |
| `ClientInfo` | `client_id: ClientId` (`u16`), `pane_id: PaneId`, `running_command: String`, `is_current_client` |
| `PluginIds` | `plugin_id: u32`, `zellij_pid: u32`, `initial_cwd: PathBuf`, `client_id: ClientId` |
| `CommandToRun` | `path: PathBuf`, `args: Vec<String>`, `cwd: Option<PathBuf>` |
| `FileToOpen` | `path`, `line_number: Option<usize>`, `cwd: Option<PathBuf>` |
| `FloatingPaneCoordinates` | `x`, `y`, `width`, `height: Option<PercentOrFixed>`, `pinned: Option<bool>`, `borderless: Option<bool>` |
| `PercentOrFixed` | `Percent(usize)` (1 to 100) or `Fixed(usize)` |
| `LayoutInfo` | `BuiltIn(String)` (`"default"`, `"compact"`, `"welcome"`), `File(String, LayoutMetadata)`, `Url(String)`, `Stringified(String)` raw KDL |
| `MessageToPlugin` | `plugin_url: Option<String>`, `destination_plugin_id: Option<u32>`, `plugin_config`, `message_name`, `message_payload`, `message_args`, `new_plugin_args: Option<NewPluginArgs>`, `floating_pane_coordinates` |
| `NewPluginArgs` | `should_float`, `pane_id_to_replace`, `pane_title`, `cwd`, `skip_cache`, `should_focus` |
| `PaneContents` | `viewport: Vec<String>`, `lines_above_viewport` and `lines_below_viewport` (full-scrollback requests only), `selected_text: Option<SelectedText>` |
| `RegexHighlight` | `pattern`, `style: HighlightStyle` (`None`, theme emphasis foreground and background variants, `CustomRgb`, `CustomIndex`), `layer: HighlightLayer` (`Hint` below `Tool` below `ActionFeedback`), `context` (echoed on `HighlightClicked`), `on_hover`, `bold`, `italic`, `underline`, `tooltip_text` |
| `ModeInfo` | `mode: InputMode`, `base_mode`, `keybinds`, `style`, `capabilities`, `session_name`, `editor`, `shell`, web fields |
| `KeyWithModifier` | `bare_key: BareKey` (`Char(c)`, `Enter`, `F(n)`, and others), `key_modifiers: BTreeSet<KeyModifier>` (`Ctrl`, `Alt`, `Shift`, `Super`) |
| `Mouse` | `ScrollUp(usize)`, `ScrollDown(usize)`, `LeftClick`, `RightClick`, `Hold`, `Release`, `Hover` (each `(line: isize, col: usize)`) |
| `InputMode` | `Normal`, `Locked`, `Resize`, `Pane`, `Tab`, `Scroll`, `EnterSearch`, `Search`, `RenameTab`, `RenamePane`, `Session`, `Move`, `Prompt`, `Tmux` |
| `ThemeHue` | `Light`, `Dark`; parses `"light"` and `"dark"` and converts to and from `HostTerminalThemeMode` |

Integer widths differ across tab and pane ids: `TabInfo.tab_id` is `usize`, `close_tab_with_id` and `rename_tab_with_id` take `u64`, `break_panes_to_tab_with_id` takes `usize`, and terminal and plugin ids inside `PaneId` are `u32`.

### Permissions

`PermissionType` has 17 variants (`zellij-utils/src/plugin_api/plugin_permission.proto`). The website's permissions page lists 14 and omits `WebAccess`, `ReadCliPipes`, and `MessageAndLaunchOtherPlugins`; docs.rs lists all 17.

| Permission | Grants |
| --- | --- |
| `ReadApplicationState` | Most state events and read queries (panes, tabs, sessions, clients) |
| `ChangeApplicationState` | Focus, pane, tab, session, plugin-reload, and layout changes |
| `OpenFiles` | Editor panes |
| `RunCommands` | Background commands and command panes |
| `OpenTerminalsOrPlugins` | New terminal and plugin panes |
| `WriteToStdin` | Writing to a pane's STDIN as the user |
| `WebAccess` | HTTP requests |
| `ReadCliPipes` | Holding, releasing, and writing to CLI pipes |
| `MessageAndLaunchOtherPlugins` | Piping to and launching other plugins |
| `Reconfigure` | Runtime configuration and keybind changes |
| `FullHdAccess` | Changing the host folder beyond the plugin's own |
| `StartWebServer` | The session web server, sharing, and login tokens |
| `InterceptInput` | Intercepting key presses and user actions |
| `ReadPaneContents` | Pane viewport and scrollback |
| `RunActionsAsUser` | Running Zellij actions as the user |
| `WriteToClipboard` | Writing to the clipboard |
| `ReadSessionEnvironmentVariables` | Environment variables present at session creation |

`request_permission(&[...])` raises one floating prompt for the whole batch, and the answer arrives as `PermissionRequestResult`. Zellij caches grants in `<cache-dir>/zellij/permissions.kdl` (`ZELLIJ_PLUGIN_PERMISSIONS_CACHE` in `zellij-utils/src/consts.rs`), keyed on the exact plugin location string, so two spellings of one path are two plugins. The file holds one node per plugin location with one child node per granted permission:

```kdl
"/home/user/.local/share/rimz/plugins/rimz-presence-zellij.wasm" {
    ReadApplicationState
    RunCommands
    Reconfigure
    ChangeApplicationState
}
```

The cache file is documented only in source. A user revokes grants through the Zellij plugin manager.

### Workers

A worker runs long work off the plugin's main loop, since wasm has no threads. It is declared with `register_worker!(TestWorker, test_worker, TEST_WORKER)`; the namespace is the middle token minus its `_worker` suffix, so this worker is addressed as `"test"`.

```rust
pub trait ZellijWorker<'de>: Default + Serialize + Deserialize<'de> {
    fn on_message(&mut self, message: String, payload: String) {}
}
```

The plugin sends with `post_message_to(PluginMessage { name, payload, worker_name })`. The worker replies with `post_message_to_plugin(...)`, which arrives as a `CustomMessage(message, payload)` event once the plugin subscribes to it. Both directions carry strings, and serialization is the plugin's own choice.

### Pipes

A pipe delivers a message to one or more plugins and launches a named target plugin on the first message, waiting for the load before delivery. The plugin's `pipe` method receives:

```rust
PipeMessage {
    source: PipeSource,        // Cli(input_pipe_id: uuid) | Plugin(source_plugin_id) | Keybind
    name: String,              // user-provided, or a random UUID
    payload: Option<String>,
    args: BTreeMap<String, String>,
    is_private: bool,          // true when targeted at this plugin; false when broadcast
}
```

Pipe routing and flow control follow these rules:

- A pipe with no destination broadcasts to every running plugin. `--plugin <url>` targets one plugin, launching it if absent. The same URL with a different `--plugin-configuration` is a different plugin for destination matching, and a runtime `PluginConfigurationChanged` does not change that identity.
- The piping CLI's STDIN is released only after the plugin renders or declines to render. `block_cli_pipe_input(id)` holds the pipeline, `unblock_cli_pipe_input(id)` resumes it, and `cli_pipe_output(id, data)` writes to the CLI's STDOUT independently. Several plugins can hold or feed one pipe by sharing its id.
- When the receiving plugin errors while handling a CLI pipe message, Zellij releases the pipe, so the `zellij pipe` client returns instead of blocking until the plugin unloads (`release_pipe_of_crashed_plugin` in `zellij-server/src/plugins/pipes.rs`).
- `zellij pipe` blocks while a launched plugin's permission prompt is pending.
- Plugin to plugin: `pipe_message_to_plugin(MessageToPlugin)`. The destination `zellij:OWN_URL` expands to the caller's own URL, which lets a plugin launch copies of itself; a configuration-keyed message can then loop.

### Keybind actions

Plugin keybind nodes parse to one variant, `Action::KeybindPipe` in `zellij-utils/src/input/actions.rs`. It carries `plugin: Option<String>`, `plugin_id: Option<u32>` (which takes precedence over `plugin`), `configuration`, `launch_new`, `skip_cache`, optional `cwd`, and pane launch hints. The KDL parser (`zellij-utils/src/kdl/mod.rs`) fills it two ways:

| Node | Fills |
| --- | --- |
| `MessagePlugin "<url>" { ... }` | URL destination, child configuration, optional `cwd`; `launch_new` and `skip_cache` default to `false` unless child nodes set them |
| `MessagePluginId <id> { ... }` | `plugin_id` only; URL, configuration, and cwd stay unset, and `launch_new` and `skip_cache` are always `false` |

On 0.44.x a plugin keybind that dispatches `KeybindPipe` can freeze the UI for about a second before the action completes ([zellij #4635](https://github.com/zellij-org/zellij/issues/4635)).

## CLI surface

Flags below are from the 0.45.1 binary's `--help` and `zellij-utils/src/cli.rs`. Every `--pane-id` accepts `terminal_<n>`, `plugin_<n>`, or a bare number meaning a terminal.

### Top level

```text
zellij [OPTIONS] [COMMAND]
  -s, --session <name>                     name a new session
  -l, --layout <name|path>                 inside a session (or with --session): add the layout's tabs; otherwise start a new session
      --layout-string <kdl>                the same, from a raw KDL string
  -n, --new-session-with-layout <name|path> always start a new session, even from inside one
  -c, --config <file>       [env: ZELLIJ_CONFIG_FILE]
      --config-dir <dir>    [env: ZELLIJ_CONFIG_DIR]
      --data-dir <dir>                     plugin lookup directory
      --max-panes <n>                      opening more panes closes old ones
  -d, --debug
```

| Subcommand | Alias |
| --- | --- |
| `options`, `setup`, `web`, `pipe`, `subscribe` | none |
| `action` | `ac` |
| `list-sessions` | `ls` |
| `list-aliases` | `la` |
| `attach` | `a` |
| `watch` | `w` |
| `kill-session`, `kill-all-sessions` | `k`, `ka` |
| `delete-session`, `delete-all-sessions` | `d`, `da` |
| `run`, `plugin`, `edit` | `r`, `p`, `e` |

`run`, `edit`, `plugin`, `action new-pane`, `action edit`, and `action launch-plugin` print the created pane id (`terminal_<id>` or `plugin_<id>`), and `action new-tab` prints the created tab id. The id is allocated before the screen thread mounts the pane, so a printed id does not prove the pane exists. The YAML converters (`convert-config`, `convert-layout`, `convert-theme`) are gone as of 0.45.0 (release notes, "Breaking Changes for Packagers").

### `attach`

```text
zellij attach [OPTIONS] [SESSION_NAME] [-- <INITIAL_COMMAND>...] [options ...]
  -c, --create               create the session if absent, attached
  -b, --create-background    create the session detached if absent
  -f, --force-run-commands   when resurrecting, run held commands immediately
      --index <n>            pick a session by creation-order index
      --close-on-exit        close the initial command's pane when the command exits
      --start-suspended      hold the initial command until ENTER
  remote and web auth: -t/--token, -r/--remember (4-week re-auth), --forget (delete the saved session first), --ca-cert <pem>, --insecure
```

`attach` accepts a trailing `options` subcommand, `zellij attach <session> options --<flag> <value> ...`, which layers option flags onto that client's runtime configuration; the merge rule is in [Command-line options](#command-line-options). `attach` is also the remote client for sessions served by `zellij web`, hence the token and TLS flags.

An initial command after `--` runs in the first pane only when `attach` creates the session (`initial_panes_from_cli` in `zellij-utils/src/input/actions.rs`); it becomes a command pane that holds on exit unless `--close-on-exit` is set. With a remote session URL, an initial command exits with status 2 and `Cannot run an initial command on a remote session.`

### `action` catalog

`zellij action <verb>` targets the current session from inside it; `zellij --session <name> action <verb>` targets a session from anywhere.

| Group | Verbs |
| --- | --- |
| Query | `list-panes [-t/--tab] [-c/--command] [-s/--state] [-g/--geometry] [-a/--all] [-j/--json]`, `list-tabs [-s/--state] [-d/--dimensions] [-p/--panes] [-l/--layout] [-a/--all] [-j/--json]`, `list-clients`, `current-tab-info [-j/--json]`, `query-tab-names`, `dump-layout`, `dump-screen [--path f] [-f/--full] [-a/--ansi] [-p/--pane-id]` |
| Panes | `new-pane` (flags below), `close-pane [--pane-id]`, `rename-pane [--pane-id] <name>`, `undo-rename-pane [--pane-id]`, `move-pane [--pane-id] [dir]`, `move-pane-backwards`, `resize [--pane-id] <increase\|decrease> [dir]`, `clear`, `toggle-fullscreen`, `toggle-no-ui-fullscreen [--pane-id]` (covers the UI bars too), `stack-panes -- <id>...` |
| Floating | `toggle-floating-panes`, `show-floating-panes [--tab-id]`, `hide-floating-panes [--tab-id]`, `are-floating-panes-visible` (answers by exit code), `toggle-pane-embed-or-floating`, `toggle-pane-pinned`, `change-floating-pane-coordinates --pane-id [--x --y --width --height --pinned --borderless]` |
| Style | `set-pane-color [--pane-id] [--fg c] [--bg c] [--reset]`, `toggle-pane-borderless`, `set-pane-borderless --pane-id [-b/--borderless]`, `toggle-pane-frames`, `set-pane-frame-style <full\|titles\|none>` |
| Focus | `focus-pane-id <id>` (switches to the tab holding the pane), `focus-next-pane`, `focus-previous-pane`, `focus-last-pane`, `move-focus <dir>`, `move-focus-or-tab <dir>` |
| Tabs | `new-tab [-l/--layout path] [--layout-string kdl] [--layout-dir] [-n/--name] [-c/--cwd] [--initial-plugin url] [--close-on-exit] [--start-suspended] [--block-until-exit[-success\|-failure]] [--no-focus] [-- cmd...]`, `close-tab`, `close-tab-by-id`, `go-to-tab <idx>`, `go-to-tab-by-id`, `go-to-tab-name [--create]`, `go-to-next-tab`, `go-to-previous-tab`, `rename-tab`, `rename-tab-by-id`, `undo-rename-tab`, `move-tab <right\|left>`, `toggle-active-sync-tab` |
| Input | `send-keys [--pane-id] <key>...` (named keys such as `"Enter"`, `"Ctrl c"`, `"Alt Shift b"`), `write [--pane-id] <bytes>...`, `write-chars [--pane-id] <str>`, `paste [--pane-id] <text>` (bracketed paste) |
| Scroll | `scroll-up`, `scroll-down`, `scroll-to-top`, `scroll-to-bottom`, `page-scroll-up`, `page-scroll-down`, `half-page-scroll-up`, `half-page-scroll-down` (all `[--pane-id]`), `edit-scrollback [--pane-id] [--ansi]` |
| Plugins | `launch-plugin <url> [-f/--floating] [-i/--in-place] [--close-replaced-pane] [-c/--configuration k=v,...] [-s/--skip-plugin-cache] [--no-focus] [--tab-id]`, `launch-or-focus-plugin <url> [--move-to-focused-tab] [...]`, `start-or-reload-plugin <url> [-c/--configuration]`, `pipe` (the top-level `zellij pipe` flags plus `-l/--force-launch-plugin`, `-s/--skip-plugin-cache`, `-f/--floating-plugin`, `-i/--in-place-plugin`, `-w/--plugin-cwd`, `-t/--plugin-title`) |
| Layout | `override-layout [path] [--layout-string kdl] [--layout-dir] [--retain-existing-terminal-panes] [--retain-existing-plugin-panes] [--apply-only-to-active-tab]`, `next-swap-layout`, `previous-swap-layout` |
| Session | `detach`, `rename-session <name>`, `save-session` (serialize now), `switch-session <name> [--tab-position] [--pane-id] [-l/--layout] [--layout-string] [--layout-dir] [-c/--cwd]`, `switch-mode <mode>` |
| Theme | `set-dark-theme`, `set-light-theme`, `toggle-theme` |
| Files | `edit <path> [--line-number n] [--direction] [--floating] [--in-place] [--cwd] [--tab-id] [floating geometry]` |

`start-or-reload-plugin` needs a connected client and exits 0 even when the server refuses it (observed on 0.44.3).

**`new-pane` flags.** `-d/--direction <right|down>` (without it the pane takes the largest free space, or splits the focused pane under `stacked_resize`), `-p/--plugin <url>` with `--configuration` and `--skip-plugin-cache`, `--cwd`, `-f/--floating` with `-x`, `-y`, `--width`, `--height` (integer or percent) and `--pinned <bool>`, `-i/--in-place` with `--close-replaced-pane` and `--pane-id` (the pane to replace), `-n/--name`, `-c/--close-on-exit`, `-s/--start-suspended`, `--stacked`, `--near-current-pane`, `--no-focus`, `--borderless <bool>`, `--tab-id`, and the blocking flags below. Placement interacts in three ways:

- `--tab-id` takes precedence over the CLI's pane context and discards its pane anchor.
- `--near-current-pane` keeps that anchor for stacked spawns. Combined with `--direction`, it creates nothing while still printing a pane id (observed on 0.44.3).
- `--no-focus` (0.45.0 and later) opens the pane without moving any client's focus, placed relative to the pane the command came from or the explicit tab.

**`list-panes` output.** JSON output is an array of `PaneListEntry` (`zellij-utils/src/data.rs`): every `PaneInfo` field flattened, plus `tab_id`, `tab_position`, `tab_name`, and, for terminal panes, `pane_command` and `pane_cwd`. With `--command`, `--all`, or `--json`, the server fills those last two per terminal pane through `enrich_panes_with_pty_data` in `zellij-server/src/route.rs`, whichever field flags are set:

| Field | Value | When absent |
| --- | --- | --- |
| `pane_command` | The discovered foreground command joined with spaces, falling back to the root process's argv | The lookup failed or took more than 100 ms |
| `pane_cwd` | The root process's cwd | The lookup failed or took more than 100 ms |
| `terminal_command` | The command a command pane was launched with, unchanged by an in-place re-exec | Not a command pane |

The output has no pid. Each enriched pane costs two sequential round trips to the PTY thread, so a JSON listing of a large session takes time in proportion to its terminal pane count.

**Blocking panes.** `--blocking` waits for the command to exit and its pane to close; `--block-until-exit` waits for any exit; `--block-until-exit-success` and `--block-until-exit-failure` return on that status or when the pane closes. `new-pane` and `zellij run` take all four, and `new-tab` takes the three `--block-until-exit` forms. The exit code reaches the caller, so `zellij action new-pane --block-until-exit-success -- cargo test && next-step` chains.

### `run`, `edit`, `plugin`

`zellij run [flags] -- <cmd>...` opens a command pane: it stays open after the command exits and shows the exit status on its frame, `ENTER` re-runs it, and `Ctrl-c` closes it. Its flags match `new-pane` without the plugin flags: `-d/--direction` (right or down), `-f/--floating` with geometry and `--pinned` (geometry is ignored for tiled panes; a sized tiled pane needs a layout), `-i/--in-place` with `--close-replaced-pane`, `-c/--close-on-exit`, `-s/--start-suspended`, `-n/--name`, `--cwd`, `-b/--borderless <bool>`, `--near-current-pane`, `--no-focus`, `--stacked`, `--tab-id`, and the blocking flags.

`zellij edit <file>` opens `$EDITOR` or `$VISUAL` (or `scrollback_editor`) in a pane, with `-l/--line-number`, the placement and geometry flags, `--no-focus`, `-b/--borderless`, and `--tab-id`.

`zellij plugin [flags] -- <url>` loads a plugin pane from an `http(s):`, `file:`, or `zellij:` URL, with `-c/--configuration`, `-s/--skip-plugin-cache`, `-f/--floating` with geometry and `--pinned`, `-i/--in-place` with `--close-replaced-pane`, `--no-focus`, `-b/--borderless`, and `--tab-id`.

### `pipe`

```text
zellij pipe [-n/--name n] [-a/--args a=b,...] [-p/--plugin url] [-c/--plugin-configuration k=v,...] [--] <payload>
```

A blank payload reads STDIN line by line, with plugin backpressure, and a plugin's `cli_pipe_output` lands on this command's STDOUT. Without `--plugin` the message broadcasts to every listening plugin. With `--plugin`, the target launches if absent; this path loads a plugin into a session with no client attached, which `start-or-reload-plugin` does not (observed on 0.44.3).

### `subscribe` and `watch`

`zellij [--session s] subscribe -p/--pane-id <id>... [-f/--format raw|json] [--ansi] [-s/--scrollback [n]]` streams rendered pane output, the CLI counterpart of the plugin `ReadPaneContents` surface; JSON events are shaped `{ "event": "pane_update", "viewport": [...] }`. A bare `--scrollback` includes all scrollback in the first delivery, and `--scrollback N` the last N lines. `zellij watch [session]` attaches read-only.

### `web`

`zellij web` serves sessions to browsers. The server flags are `--start` (the default), `--stop`, `--status` with `--timeout <seconds>` (default 30), `-d/--daemonize`, `--ip` (default `127.0.0.1`), `--port` (default `8082`), and `--cert` with `--key` (required when not listening on `127.0.0.1`). `--server-startup-timeout` (default 10 seconds) applies only on Windows, where the daemon is polled over TCP; Unix signals startup through a pipe.

Login tokens have their own flags. `--create-token` (shown once) and `--create-read-only-token` (attach as a watcher only) are clap `exclusive(true)`, so neither combines with any other flag, `--token-name` included. `--revoke-token <name>`, `--revoke-all-tokens`, and `--list-tokens` manage existing tokens. The server pairs with the `web_server*` and `web_sharing` options and with `attach`'s token flags.

### `setup` and sessions

`zellij setup` flags: `--check` (print the resolved directories and check the configuration), `--dump-config`, `--dump-layout <name>`, `--dump-swap-layout <name>`, `--dump-plugins [dir]` (write the built-in wasm), `--clean` (ignore the configuration file and use shipped defaults), `--generate-completion <shell>`, `--generate-auto-start <shell>`.

`list-sessions [-s/--short] [-n/--no-formatting] [-r/--reverse]` exits 0 with empty output when no server runs, and marks dead sessions `EXITED - attach to resurrect`. `kill-session <name>` stops a live session, which stays resurrectable. `delete-session [-f/--force] <name>` removes the serialized state, and `--force` kills a live session first. `list-aliases` prints the plugin alias table.

## Configuration

Zellij reads one KDL file. It looks for the configuration directory in this order: `--config-dir`, `$ZELLIJ_CONFIG_DIR`, `$HOME/.config/zellij`, the platform default (the XDG path on Linux; `~/Library/Application Support/org.Zellij-Contributors.Zellij` on macOS), then `/etc/zellij`. Because `$HOME/.config/zellij` comes before the platform default, Zellij uses and creates it even when `$XDG_CONFIG_HOME` points elsewhere. `zellij setup --dump-config` prints the default file. Zellij watches the active configuration file and applies most changes live.

A server that finds no configuration file starts a first-run setup wizard, and while it shows, `action new-pane` mounts are dropped; panes from a layout mount normally (observed on 0.44.3).

### Options catalog

Top-level KDL options are written `option_name value`. `zellij options --help` exposes 54 of them as kebab-case long flags; `env` exists only in KDL, and the web server address and TLS fields are positional `zellij options` arguments (`[WEB_SERVER_IP] [WEB_SERVER_PORT] [WEB_SERVER_CERT] [WEB_SERVER_KEY] [ENFORCE_HTTPS_FOR_LOCALHOST]`). Field names are from `zellij-utils/src/input/options.rs`.

| Option | Values (default first) | Note |
| --- | --- | --- |
| `default_mode` | `normal`, `locked`, or another `InputMode` | Input mode for new clients |
| `default_shell` | `$SHELL` | |
| `default_cwd` | path | |
| `default_layout` | `default` | Name in the layout directory |
| `layout_dir`, `theme_dir` | path | Default: a subdirectory of the configuration directory |
| `theme`, `theme_dark`, `theme_light` | `default`, or a theme name | The dark and light pair drives `toggle-theme` and switching on host theme reports |
| `explicit_theme_hue` | unset, `dark`, `light` | Pins the session's appearance before the first render and overrides host terminal reports (CSI 2031, DSR 997); unset follows the host |
| `on_force_close` | `detach`, `quit` | Response to SIGTERM, SIGINT, SIGQUIT, SIGHUP |
| `session_name` | string | With `attach_to_session true`, attach when that session exists |
| `attach_to_session` | `false`, `true` | See `session_name` |
| `mirror_session` | `false`, `true` | Multiple clients share one view, or each has its own |
| `mouse_mode` | `true`, `false` | |
| `mouse_click_through` | `false`, `true` | A click on an inactive pane both focuses it and reaches the application; see [Mouse handling](#mouse-handling) |
| `advanced_mouse_actions` | `true`, `false` | Hover effects, pane grouping, mouse resize |
| `mouse_hover_effects` | `true`, `false` | Frame highlight and help text on hover |
| `mouse_hover_tips` | `true`, `false` | Hover tips |
| `mouse_scroll_resize` | `true`, `false` | Ctrl+wheel resizes panes |
| `scroll_mode_sync` | `true`, `false` | Scrolling a pane enters Scroll mode, and leaving the scroll exits it |
| `focus_follows_mouse` | `false`, `true` | |
| `support_kitty_keyboard_protocol` | `true`, `false` | |
| `support_kitty_graphics_protocol` | `true`, `false` | Effective only when the host terminal answers Zellij's startup probe; see [Kitty graphics protocol](#kitty-graphics-protocol) |
| `copy_command` | for example `wl-copy` | Replaces OSC 52 |
| `copy_clipboard` | `system`, `primary` | OSC 52 destination |
| `copy_on_select` | `true`, `false` | |
| `dangerously_enable_paste_buffer_read` | `false`, `true` | Allows reading the paste buffer |
| `osc8_hyperlinks` | `true`, `false` | |
| `osc133_command_selection` | `true`, `false` | OSC 133-aware command selection |
| `word_separators` | `"[]{}<>()"` | Extra characters that end a word selection |
| `host_notification_protocol` | `auto`, `osc9`, `osc99`, `bell`, `off` | How pane notifications reach the host terminal |
| `scroll_buffer_size` | `10000` | Lines per pane |
| `scrollback_editor` | `$EDITOR` or `$VISUAL` | |
| `styled_underlines` | `true`, `false` | |
| `pane_frames` | `true`, `false` | Frame look: `ui { pane_frames { rounded_corners true; hide_session_name true } }` |
| `pane_frame_style` | `titles`, `full`, `none` | Pane frame presentation |
| `simplified_ui` | `false`, `true` | No arrow fonts in plugins |
| `auto_layout` | `true`, `false` | New panes follow the predefined swap layouts |
| `stacked_resize` | `true`, `false` | A pane opened with no direction splits the focused pane, and closing returns the space to that sibling |
| `stacked_pane_list` | `true`, `false` | `list-panes` marks collapsed stack members `is_suppressed` and gives every member the stack's full rectangle (0.45.0 and later) |
| `visual_bell` | `true`, `false` | |
| `show_startup_tips`, `show_release_notes` | `true`, `false` | |
| `session_serialization` | `true`, `false` | See [Session serialization and resurrection](#session-serialization-and-resurrection) |
| `serialize_pane_viewport` | `false`, `true` | The website calls the KDL key `pane_viewport_serialization`; the parser reads `serialize_pane_viewport` (`zellij-utils/src/kdl/mod.rs`) |
| `scrollback_lines_to_serialize` | integer; `0` means all | Applies only with viewport serialization |
| `serialization_interval` | seconds; default 60 | See [Session serialization and resurrection](#session-serialization-and-resurrection) |
| `disable_session_metadata` | `false`, `true` | Stops the once-a-second session metadata writer |
| `post_command_discovery_hook` | shell snippet | Rewrites each discovered command (`$RESURRECT_COMMAND`) |
| `env` | `env { KEY "value" }` | Set in every terminal pane; KDL only |
| `web_server` | `false`, `true` | Start the web server with the session; with `web_server_ip` (`127.0.0.1`), `web_server_port` (`8082`), `web_server_cert`, `web_server_key`, `enforce_https_for_localhost` |
| `web_sharing` | `"off"`, `"on"`, `"disabled"` | `disabled` cannot be turned on at runtime |
| `client_async_worker_tasks` | `4`; `0` means the physical core count | Async workers per active client; used by web clients |
| `nested_session_handling` | `ask`, `fullscreen`, `descend`, `never` | Policy when a Zellij client runs inside a Zellij pane |

The file also takes `keybinds`, `themes`, `plugins` (aliases), and `load_plugins` blocks. `load_plugins { "file:/path.wasm" }` starts background plugins when a session starts. It is a configuration-file block only; layouts have no `load_plugins` (observed on 0.44.3).

Nested-session detection negotiates over the terminal stream: every client announces itself to the host client whether or not `ZELLIJ` is set in its environment, so `nested_session_handling` also applies across SSH.

### Command-line options

Options from the command line merge into the configuration at two points, with different rules (`Setup::from_cli_args` in `zellij-utils/src/setup.rs`, `Options::merge` and `Options::merge_from_cli` in `zellij-utils/src/input/options.rs`):

| Source | Merge | Effect |
| --- | --- | --- |
| Top-level `zellij options --<flag>` | `Options::merge` | A set flag replaces the configuration value |
| Layout options | `Options::merge` | Between the configuration file and the top-level flags in precedence |
| `zellij attach ... options --<flag>` | `Options::merge_from_cli` | Most options: a set flag replaces the value. Nine booleans are toggles instead (below) |

Under `merge_from_cli`, these nine booleans are XORed when both the resolved configuration and the flag set them: `simplified_ui`, `mouse_mode`, `pane_frames`, `auto_layout`, `mirror_session`, `session_serialization`, `serialize_pane_viewport`, `focus_follows_mouse`, `mouse_click_through`. A flag value of `false` therefore leaves a configured value unchanged, and `true` inverts it; only when the configuration omits the key does the flag value apply directly. Every other option, booleans such as `disable_session_metadata` and `stacked_pane_list` included, takes the flag value.

A detached birth drops the `attach` options entirely. `zellij attach --create-background <s> options ...` builds the new server's `CliAssets` in `start_server_detached` (`zellij-client/src/lib.rs`) from `CliArgs::options()`, which reads only a top-level `zellij options` command. Options a session fixes when its first client initializes must travel in the birth layout instead.

### Runtime reconfigure

`reconfigure(kdl, save_to_disk)` parses the supplied KDL on top of the client's current configuration (`Config::from_kdl` in `zellij-utils/src/kdl/mod.rs`), then applies `Options::merge`, where a supplied key wins and an absent key keeps its live value. No XOR applies. `propagate_configuration_changes` in `zellij-server/src/lib.rs` pushes the result to every tab. With `save_to_disk` false, the configuration file is untouched.

### Mouse handling

The client enables mouse reporting from `mouse_mode`, converts terminal mouse events, and forwards them to the server as `Action::MouseEvent` (`zellij-client/src/input_handler.rs`). Input-mode handling sits on the keyboard path only, so locked mode still forwards mouse events.

On the server, `determine_mouse_action` in `zellij-server/src/tab/mouse_handler.rs` decides a plain left press on an inactive pane:

| `mouse_click_through` | `focus_follows_mouse` | Result |
| --- | --- | --- |
| `true` | `false` | `FocusPaneAndClickThrough`: the pane takes focus and the application receives the click |
| any other combination | | The press only focuses the pane; a second click reaches the application |

`advanced_mouse_actions` gates the hover, grouping, and resize branches and has no effect on click-through.

A single pane can drop its frame without changing global `pane_frames`: `--borderless true` on `new-pane`, `run`, `edit`, or `plugin`, `action set-pane-borderless`, or `borderless=true` on a layout `pane` node.

### Kitty graphics protocol

Zellij terminates the kitty graphics protocol inside the server instead of passing APC sequences through (`zellij-server/src/panes/kitty_graphics/`). It base64-decodes payloads and, when asked, decompresses them. It decodes PNG input to RGBA and holds images in a session-wide 320 MiB LRU store, and it assigns its own image ids on the host terminal. When a pane becomes invisible, including a collapsed member of a stack, Zellij removes its images from the host terminal and places them again when the pane returns; the decoded image stays in the store. Pixel dimensions the host reports apply to every existing pane.

Placement is cursor-addressed:

| Accepted | Rejected |
| --- | --- |
| Transmit-and-place (`a=T`) and place (`a=p`), with cell spans (`c`, `r`), source crop (`x`, `y`, `w`, `h`), pixel offsets (`X`, `Y`), and z-index (`z`) | Unicode placeholders (`U=1`), animation actions (`a=f`, `a=a`, `a=c`), and shared-memory transport (`t=s`) |

Limits: one unchunked APC is capped at 1 MiB, a decoded image at 100 MiB (`MAX_DECODED_BYTES`), and either image axis at 10,000 pixels (`MAX_DIMENSION`).

Support is negotiated once per client. At startup Zellij queries the host with `a=q,i=31` ahead of a Primary DA barrier, and folds the answer with `support_kitty_graphics_protocol` into one server-side state. A pane's own `a=q` query then gets `i=<id>;OK` when graphics work, `ENOTSUPPORTED` when the host lacks support, and no reply when the option disables the protocol. From inside a pane, no reply cannot be told apart from an older server or a lost or slow reply.

### Plugin aliases

```kdl
plugins {
    tab-bar location="zellij:tab-bar"
    filepicker location="zellij:strider" {
        cwd "/"
    }
}
```

An alias name works anywhere a plugin URL does: layouts, the CLI, keybinds, and pipes. The shipped aliases are `tab-bar`, `status-bar`, `compact-bar`, `strider`, and `session-manager`, plus two reconfigured copies of the same wasm: `welcome-screen` (session-manager with `welcome_screen true`) and `filepicker` (strider with `cwd "/"`). An alias that sets `cwd` also receives `caller_cwd`, the focused pane's cwd, in its configuration. Plugin URLs use the `zellij:` scheme for built-ins (`zellij:compact-bar`), and `file:` or `http(s):` otherwise.

## Layout KDL

A layout's root node is `layout`, with children `pane`, `tab`, `pane_template`, `tab_template`, `default_tab_template`, `new_tab_template`, `floating_panes`, swap layout nodes, and a global `cwd`. A layout applies when a session is born, and afterwards only through `new-tab --layout`, `override-layout`, or a plugin's layout commands. `zellij setup --dump-layout default` prints the built-in layout.

**`pane`** is a leaf or a container. Its attributes are `split_direction "vertical"|"horizontal"` (containers; default horizontal), `size "30%"` or a fixed integer, `borderless`, `focus`, `name`, `cwd`, `command` with `args "a" "b"` (args only in child braces), `close_on_exit`, `start_suspended`, `edit "file"`, `plugin { location "zellij:..." }` (location only in child braces), `stacked` and `expanded`, and `default_fg` and `default_bg`. Upstream docs call fixed sizes unstable. A detached session is created at a placeholder 50 by 50 cell size until a client connects (`start_server_detached`), and a fixed pane size that does not fit the birth geometry fails a `--create-background` birth (observed on 0.44.3).

**`tab`** takes `name`, `focus` (on one tab), `split_direction`, `cwd`, `hide_floating_panes`, and child panes.

**Templates.** `pane_template name="..."` and `tab_template name="..."` mark their insertion point with a `children` node, and a consumer invokes a template by using its name as a node. A template with a `command` accepts `args` and `cwd` from the consumer. `default_tab_template` shapes the initial tabs and every later tab, and replaces Zellij's built-in template, so the tab bar and status bar disappear unless the template adds them. `new_tab_template` shapes only tabs opened after birth. Two constructs fail quietly (observed on 0.44.3):

- A `children` node nested inside a split is never filled with a default terminal; the terminal pane has to be written out.
- A layout with a `new_tab_template` and no `tab` node kills a `--create-background` birth.

**`floating_panes`** holds child panes with `x`, `y`, `width`, and `height`, each fixed or a percentage. A tab's or the global `cwd` applies to floating panes as it does to tiled ones.

**cwd composition.** Relative paths chain from pane to tab to global `cwd` to the invocation directory (`/hi` plus `there` plus `friend` gives `/hi/there/friend`), and an absolute pane `cwd` overrides every parent.

**Outer-terminal title.** The server sets the host terminal's title (OSC 0) from the focused pane as `<session> | <pane title>`, dropping the separator when the title is empty (`make_terminal_title` in `zellij-utils/src/shared.rs`, `TerminalPane::render_terminal_title` in `zellij-server/src/panes/terminal_pane.rs`). A non-empty pane name, from a layout `name` or `new-pane --name`, wins over the title the application sets with OSC 0, 1, or 2; only an unnamed pane shows the application's title.

**Swap layouts** (`*.swap.kdl`, or `swap_tiled_layout` and `swap_floating_layout` nodes) drive the `auto_layout` flow and `next-swap-layout` and `previous-swap-layout`. Behaviours observed on 0.44.3:

| Situation | Behaviour |
| --- | --- |
| `max_panes` budget in a swap template | Plugin panes count and take slots like terminals, so a template without a plugin slot re-tiles a status bar into the work area at full size |
| Underfilled explicit slots | Panes fill slots in order and the tier renders fewer rows instead of failing |
| No-direction `NewPane` without `stacked_resize` | Targets the pane with the largest weighted area (`rows × 4 × cols`), which can be a full-height fixed side pane |
| No-direction `NewPane` with `stacked_resize` | Splits the focused pane; closing it returns the space to that sibling |
| No-direction open after a manual resize | Keeps the user's proportions where the split allows |
| A new pane in a `children` stack | Appends at the end of the stack and keeps focus, whatever was focused before |
| An unbounded swap `tab` with a `children` work area | Applies at any pane count and re-applies after a manual resize |
| A root `swap_tiled_layout` with a `new_tab_template` | Coexists only when no `default_tab_template` is present |
| `default_tab_template` with a root swap layout | Requires a `children` node in the template |

## Session serialization and resurrection

With `session_serialization` on (the default), each session serializes to the cache folder, `~/.cache/zellij/<contract_version>/session_info/<session>/`, as a KDL layout in the same dialect `--layout` reads, so it can be shared across machines. The serializer writes the layout plus each pane's discovered command (`$RESURRECT_COMMAND`, which `post_command_discovery_hook` rewrites: its STDOUT replaces the discovered command), and optionally the viewport (`serialize_pane_viewport`) and scrollback (`scrollback_lines_to_serialize`).

Serialization and the disk write are two timers in `zellij-server/src/background_jobs.rs`:

| Timer | Interval | Work | Switched off by |
| --- | --- | --- | --- |
| Serialization | `serialization_interval` seconds; source default 60 (`DEFAULT_SERIALIZATION_INTERVAL`) | Discovers each pane's command and cwd and serializes the layout in memory | `session_serialization false` |
| Session metadata | 1 second (`SESSION_METADATA_WRITE_INTERVAL_MS`) | Writes the session info and the latest serialized layout files, each only when its content changed | `disable_session_metadata true` |

The website's resurrection page says sessions serialize every second; the source default is 60 seconds. Because the metadata timer is what writes the serialized layout to disk, `disable_session_metadata true` with serialization on writes the layout to disk only on explicit saves: `action save-session` and the plugin `save_session()` serialize and write immediately. Neither option stops the one-second activity probe that emits `CwdChanged` and `CommandChanged` (see [Events](#events)).

A dead serialized session lists as `EXITED - attach to resurrect`. Attaching recreates its layout with every command pane held at a `Press ENTER to run...` banner (`PaneInfo.is_held`), and `attach -f/--force-run-commands` runs them at once instead. Removal works at three levels: `kill-session` keeps the serialized state, `delete-session` and `delete-all-sessions` remove it, and `attach --forget` removes it before connecting. `session_serialization false` stops the serializer, so a session that dies leaves nothing to resurrect.

`attach` reads the target session's resurrection layout before it checks whether the session is live (`resurrection_layout(&s)` ahead of the live and exists match in `src/commands.rs`), and exits with status 2 when that parse fails. A corrupt serialized layout therefore blocks attaching to a healthy live session until the cache entry is removed.
