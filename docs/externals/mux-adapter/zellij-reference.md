# Zellij upstream reference

> The Rimz-side contracts live in [multiplexers.md](../../internals/sidebar/multiplexers.md) — the `MuxBackend` seam, the presence channel, the birth layout, the health gates — and [web.md](../../internals/reach/web.md) for browser access. This doc mirrors the upstream surface itself.

This is the single home for the **Zellij upstream surface** Rimz binds to — the wasm plugin API (lifecycle, events, commands, types, permissions, workers, pipes), the CLI control surface, the configuration options, the layout KDL, and session serialization. It is a hand-maintained mirror of zellij.dev's docs cross-checked against the installed binary's `--help` and the `zellij-utils`/`zellij-tile` 0.44.3 source, captured at **Zellij 0.44.3** (2026-06). Where the website and the source disagree, the source wins.

Coverage is **depth on what Rimz wires, breadth as an index**: the events and host commands the presence plugin uses, the CLI verbs the backend adapter calls, and the layout nodes the birth templates spell are documented in full; the rest of the catalog is listed so a contributor wiring a new surface knows it exists.

## Upstream sources

Re-fetch these to refresh this mirror. The canonical type definitions live in the [`zellij-tile` docs.rs](https://docs.rs/zellij-tile/latest/zellij_tile/) and `zellij-utils/src/data.rs`; the website lags the source — when refreshing, trust the installed binary's `--help` and the source over the website's tables.

| Surface | Source |
| --- | --- |
| Plugin events | <https://zellij.dev/documentation/plugin-api-events.html> |
| Plugin commands | <https://zellij.dev/documentation/plugin-api-commands.html> |
| Plugin types | <https://zellij.dev/documentation/plugin-api-types.html> → docs.rs |
| Plugin workers | <https://zellij.dev/documentation/plugin-api-workers.html> |
| Plugin lifecycle | <https://zellij.dev/documentation/plugin-lifecycle.html> |
| Plugin permissions | <https://zellij.dev/documentation/plugin-api-permissions.html> |
| Plugin loading & aliases | <https://zellij.dev/documentation/plugin-loading.html>, <https://zellij.dev/documentation/plugin-aliases.html> |
| Pipes (concept + plugin side) | <https://zellij.dev/documentation/zellij-plugin-and-pipe.html>, <https://zellij.dev/documentation/plugin-pipes.html> |
| CLI control | <https://zellij.dev/documentation/controlling-zellij-through-cli.html>, <https://zellij.dev/documentation/cli-actions.html>, <https://zellij.dev/documentation/zellij-run-and-edit.html> |
| CLI recipes | <https://zellij.dev/documentation/cli-recipes.html> |
| Config & options | <https://zellij.dev/documentation/configuration.html>, <https://zellij.dev/documentation/options.html>, <https://zellij.dev/documentation/command-line-options.html> |
| Layout KDL | <https://zellij.dev/documentation/creating-a-layout.html> |
| Session resurrection | <https://zellij.dev/documentation/session-resurrection.html> |

## Plugin API

Plugins are wasm32-wasip1 binaries loaded into Zellij's plugin host. Rimz ships one: [`rimz-presence-zellij`](../../../crates/rimz-presence-zellij/) ([multiplexers.md → presence channel](../../internals/sidebar/multiplexers.md#zellij-presence-channel)).

### Lifecycle

The `ZellijPlugin` trait (from `zellij_tile::prelude::*`), registered with `register_plugin!(MyPlugin)`; Zellij instantiates via `Default`:

```rust
fn load(&mut self, configuration: BTreeMap<String, String>)  // init; subscribe + request_permission here
fn update(&mut self, event: Event) -> bool                   // subscribed events; true = re-render
fn render(&mut self, rows: usize, cols: usize)               // print UI to STDOUT; rows/cols exclude the frame
fn pipe(&mut self, pipe_message: PipeMessage) -> bool        // pipe messages; true = re-render
```

Events arrive asynchronously with no ordering guarantee. `render` runs after an `update`/`pipe` returns `true`, and on startup and resize. A headless plugin (the presence plugin's shape) simply never renders and calls `hide_self()` once granted.

### Events

Full `Event` catalog as defined in `zellij-utils 0.44.3 src/data.rs` (46 variants). ✓ marks what `rimz-presence-zellij` subscribes to. The permission column is the *event-delivery* gate as documented upstream; an ungated event still requires `subscribe`. `RunCommandResult` is subscribed so the plugin drains replies to its `run_command` pokes.

| Event | Payload | Permission | ✓ |
| --- | --- | --- | :---: |
| `ModeUpdate` | `ModeInfo` | ReadApplicationState | |
| `TabUpdate` | `Vec<TabInfo>` | ReadApplicationState | ✓ |
| `PaneUpdate` | `PaneManifest` | ReadApplicationState | ✓ |
| `Key` | `KeyWithModifier` | — (own pane) | |
| `Mouse` | `Mouse` | — (own pane) | |
| `Timer` | `f64` seconds elapsed | — (reply to `set_timeout`) | ✓ |
| `CopyToClipboard` | `CopyDestination` | ReadApplicationState | |
| `SystemClipboardFailure` | — | ReadApplicationState | |
| `InputReceived` | — (any input, unspecified which) | — | |
| `Visible` | `bool` | — | |
| `CustomMessage` | `(message: String, payload: String)` — from a worker | — | |
| `FileSystemCreate` / `FileSystemRead` / `FileSystemUpdate` / `FileSystemDelete` | `Vec<(PathBuf, Option<FileMetadata>)>` | — (after `watch_filesystem()`) | |
| `PermissionRequestResult` | `PermissionStatus` (`Granted` \| `Denied`) | — | ✓ |
| `SessionUpdate` | `Vec<SessionInfo>`, `Vec<(String, Duration)>` resurrectable sessions | ReadApplicationState | |
| `RunCommandResult` | `Option<i32>` exit code, `Vec<u8>` stdout, `Vec<u8>` stderr, `Context` | — (reply to `run_command`) | ✓ |
| `WebRequestResult` | `u16` status, `BTreeMap` headers, `Vec<u8>` body, `Context` | — (reply to `web_request`) | |
| `CommandPaneOpened` | `u32` terminal pane id, `Context` | ReadApplicationState | |
| `CommandPaneExited` | `u32`, `Option<i32>` exit code, `Context` — pane stays open | ReadApplicationState | |
| `PaneClosed` | `PaneId` | ReadApplicationState | ✓ |
| `EditPaneOpened` / `EditPaneExited` | `u32` (+ `Option<i32>` on exit), `Context` | ReadApplicationState | |
| `CommandPaneReRun` | `u32`, `Context` | ReadApplicationState | |
| `FailedToWriteConfigToDisk` | `Option<String>` file path | ReadApplicationState | |
| `ListClients` | `Vec<ClientInfo>` — reply to `list_clients()` | ReadApplicationState | |
| `HostFolderChanged` / `FailedToChangeHostFolder` | `PathBuf` / `Option<String>` error | — | |
| `PastedText` | `String` | — | |
| `ConfigWasWrittenToDisk` | — | — | |
| `WebServerStatus` | `WebServerStatus` (`Online(url)` \| `Offline` \| `DifferentVersion(v)`) | — | |
| `FailedToStartWebServer` | `String` error | — | |
| `BeforeClose` | — fires before plugin unload, for cleanup | — | |
| `InterceptedKeyPress` | `KeyWithModifier` — consumed, not processed normally | InterceptInput | |
| `UserAction` | `Action`, `ClientId`, `Option<u32>` terminal id, `Option<ClientId>` CLI client | InterceptInput | |
| `PaneRenderReport` / `PaneRenderReportWithAnsi` | `HashMap<PaneId, PaneContents>` — periodic; without/with ANSI | ReadPaneContents | |
| `ActionComplete` | `Action`, `Option<PaneId>`, `Context` — reply to `run_action` | RunActionsAsUser | |
| `CwdChanged` | `PaneId`, `PathBuf` new cwd, `Vec<ClientId>` focused clients | — | |
| `CommandChanged` | `PaneId`, `Vec<String>` argv, `bool` is_foreground, `Vec<ClientId>` focused clients | — | ✓ |
| `AvailableLayoutInfo` | `Vec<LayoutInfo>`, `Vec<LayoutWithError>` | — | |
| `PluginConfigurationChanged` | `BTreeMap<String, String>` — runtime config update; pipe identity stays bound to the *load-time* configuration | — | |
| `HighlightClicked` | `{ pane_id, pattern, matched_string, context }` — from `set_pane_regex_highlights`; `matched_string` is capture group 1 if present | — | |
| `InitialKeybinds` | `KeybindsVec` | — | |
| `HostTerminalThemeChanged` | `HostTerminalThemeMode` (`Dark` \| `Light`) via CSI 2031 / DSR 997 | — | |

`Context` is `BTreeMap<String, String>` — the caller-supplied dictionary echoed back on the matching reply event; it is how a plugin correlates async replies to requests.

Verified Rimz caveat: the `SessionUpdate` pane manifest for the current session can arrive transiently partial on Zellij's roughly 60s serialization cadence while `PaneUpdate`, `list-panes -j -a`, and the serialized session metadata still reflect the full live room. Rimz treats `PaneUpdate` as the authoritative pane roster and uses `SessionUpdate` only as an upstream session-info event reference.

**`CommandChanged` is the load-bearing event for Rimz**: it pushes the foreground-command handoff with the full argv, the foreground bit, and the focused clients — exactly the live process state `list-panes -j` does not report ([caveats](../../internals/sidebar/multiplexers.md#zellij-backend-caveats)). A cached permission grant produces **no** `PermissionRequestResult`; application state flowing is the proof of grant (verified live, 0.44.3).

### Commands (host functions)

The full catalog, grouped, with required permission. ✓ marks what the presence plugin calls today. Signatures abbreviated; canonical forms on [docs.rs](https://docs.rs/zellij-tile/latest/zellij_tile/shim/index.html).

**Ungated** — `subscribe(&[EventType])` ✓ · `unsubscribe(&[EventType])` · `request_permission(&[PermissionType])` ✓ · `set_selectable(bool)` · `show_cursor(Option<(x, y)>)` · `set_self_mouse_selection_support(bool)` · `get_plugin_ids() -> PluginIds` · `get_zellij_version() -> String` · `set_timeout(secs: f64)` ✓ → `Timer` · `hide_self()` ✓ · `show_self(float_if_hidden)` · `close_self()` · `post_message_to(PluginMessage)` (to worker) · `post_message_to_plugin(PluginMessage)` (worker → plugin, as `CustomMessage`) · `report_panic(&PanicHookInfo)` · `scan_host_folder(&Path)` · `watch_filesystem()` → FileSystem\* events.

**ReadApplicationState** — `generate_random_name()` · `get_layout_dir()` · `get_focused_pane_info() -> Result<(usize, PaneId)>` · `get_pane_info(PaneId) -> Option<PaneInfo>` · `get_tab_info(tab_id) -> Option<TabInfo>` · `get_pane_pid(PaneId) -> Result<i32>` · `get_pane_running_command(PaneId) -> Result<Vec<String>>` · `get_pane_cwd(PaneId) -> Result<PathBuf>` · `get_session_list() -> Result<SessionListSnapshot>` · `list_clients()` → `ListClients` event · `save_session()` · `current_session_last_saved_time() -> Option<u64>` ms · `dump_layout(name)` · `dump_session_layout()` / `dump_session_layout_for_tab(idx)` -> KDL + `LayoutMetadata` · `parse_layout(&str) -> Result<LayoutMetadata, LayoutParsingError>`.

The `get_pane_pid` / `get_pane_running_command` / `get_pane_cwd` request/response trio exposes **live process state the CLI does not** — the plugin surface is strictly richer than `zellij action list-panes`.

**ChangeApplicationState** — the bulk of the catalog:

- *Focus & navigation:* `focus_terminal_pane(id, float_if_hidden, in_place_if_hidden)` · `focus_plugin_pane(…)` · `focus_pane_with_id(PaneId, …)` · `focus_next_pane()` / `focus_previous_pane()` · `move_focus(Direction)` / `move_focus_or_tab(Direction)` · `switch_tab_to(idx)` / `go_to_tab(idx)` / `go_to_tab_name(&str)` / `focus_or_create_tab(&str)` / `toggle_tab()` / `go_to_next_tab()` / `go_to_previous_tab()`.
- *Pane lifecycle & shape:* `close_focus()` · `close_terminal_pane(id)` / `close_plugin_pane(id)` / `close_pane_with_id(PaneId)` / `close_multiple_panes(Vec<PaneId>)` · `rename_terminal_pane` / `rename_plugin_pane` / `rename_pane_with_id` / `undo_rename_pane()` · `toggle_focus_fullscreen()` / `toggle_pane_id_fullscreen(PaneId)` · `toggle_pane_frames()` · `toggle_pane_embed_or_eject[_for_pane_id]` · `toggle_pane_borderless(PaneId)` / `set_pane_borderless(PaneId, bool)` · `move_pane[_with_direction|_with_pane_id|…]` · `replace_pane_with_existing_pane(replace, existing, suppress_replaced)` · `set_floating_pane_pinned(PaneId, bool)` · `stack_panes(Vec<PaneId>)` · `float_multiple_panes` / `embed_multiple_panes` · `change_floating_panes_coordinates(Vec<(PaneId, FloatingPaneCoordinates)>)` · `group_and_ungroup_panes(group, ungroup, for_all_clients)` · `highlight_and_unhighlight_panes(hl, unhl)` · `set_pane_color(PaneId, fg, bg)` · `hide_pane_with_id(PaneId)` / `show_pane_with_id(PaneId, float, focus)` · `show_floating_panes(tab)` / `hide_floating_panes(tab)`.
- *Resize & scroll:* `resize_focused_pane(Resize)` / `…_with_direction(Resize, Direction)` / `resize_pane_with_id(ResizeStrategy, PaneId)` · scroll family: `scroll_up/down[_in_pane_id]`, `scroll_to_top/bottom[_in_pane_id]`, `page_scroll_up/down[_in_pane_id]` · `edit_scrollback[_for_pane_with_id]` · `clear_screen[_for_pane_id]`.
- *Tabs:* `new_tab(name, cwd) -> Option<usize>` · `new_tabs_with_layout(kdl: &str) -> Vec<usize>` · `new_tabs_with_layout_info(LayoutInfo)` · `close_focused_tab()` / `close_tab_with_index(usize)` / `close_tab_with_id(u64)` · `rename_tab(position, name)` / `rename_tab_with_id(u64, name)` / `undo_rename_tab()` · `toggle_active_tab_sync()` · `break_panes_to_new_tab(ids, name, focus)` / `…_to_tab_with_index` / `…_to_tab_with_id` · `open_command_pane_in_new_tab` / `open_plugin_pane_in_new_tab` / `open_editor_pane_in_new_tab` — each returns `(Option<usize> tab, Option<PaneId>)`.
- *Sessions:* `switch_session(Option<&str>)` / `…_with_layout` / `…_with_cwd` / `…_with_focus(name, tab, (pane_id, is_plugin))` · `rename_session(&str)` · `kill_sessions(&[names])` · `delete_dead_session(name)` / `delete_all_dead_sessions()` · `detach()` · `disconnect_other_clients()` · `quit_zellij()` · `change_host_folder(PathBuf)`.
- *Signals & layouts:* `send_sigint_to_pane_id(PaneId)` / `send_sigkill_to_pane_id(PaneId)` · `rerun_command_pane(terminal_id)` · `save_layout(name, kdl, overwrite)` / `delete_layout(name)` / `rename_layout(old, new)` / `edit_layout(name, ctx)` · `override_layout(LayoutInfo, retain_terminals, retain_plugins, active_tab_only, ctx)` · `previous_swap_layout()` / `next_swap_layout()`.

**RunCommands** — `run_command(&[&str], Context)` ✓ → `RunCommandResult` · `run_command_with_env_variables_and_cwd(cmd, env, cwd, ctx)` · the `open_command_pane*` family (tiled / `_floating` / `_in_place` / `_near_plugin` / `_floating_near_plugin` / `_in_place_of_plugin(close_after)` / `_in_place_of_pane_id` / `_background`) each `(CommandToRun, [coords,] Context) -> Option<PaneId>`. `run_command` spawns with the **server's** env and the launching CLI's cwd — pass absolute argv. Both `run_command` and timers keep working with **zero clients attached** (verified live, 0.44.3), though a detached or starved server can still drop *pane-lifecycle* processing until the next attach ([caveats](../../internals/sidebar/multiplexers.md#zellij-backend-caveats)).

**OpenFiles** — `open_file*` family mirroring the command-pane variants, each `(FileToOpen, [coords,] Context) -> Option<PaneId>`, plus `open_edit_pane_in_place_of_pane_id`.

**OpenTerminalsOrPlugins** — `open_terminal*` family `(path) -> Option<PaneId>` · `open_plugin_pane_floating(url, config, coords, ctx)` · `start_or_reload_plugin(url)` · `reload_plugin_with_id(u32)` · `load_new_plugin(url, config, in_background, skip_cache)`.

**WriteToStdin** — `write(Vec<u8>)` / `write_chars(&str)` to the focused pane; `write_to_pane_id` / `write_chars_to_pane_id` to a specific pane.

**Other gates** — `web_request(url, HttpVerb, headers, body, ctx)` (WebAccess) → `WebRequestResult` · `copy_to_clipboard(text)` (WriteToClipboard) · `reconfigure(kdl: String, save_to_disk: bool)` and `rebind_keys(unbind, rebind, save)` (Reconfigure) · `intercept_key_presses()` / `clear_key_presses_intercepts()` (InterceptInput) · `block_cli_pipe_input(pipe_id)` / `unblock_cli_pipe_input(pipe_id)` / `cli_pipe_output(pipe_id, output)` (ReadCliPipes) · `pipe_message_to_plugin(MessageToPlugin)` (MessageAndLaunchOtherPlugins) · `start_web_server()` (StartWebServer) · `get_session_environment_variables()` (ReadSessionEnvironmentVariables) · `get_pane_scrollback(PaneId, full) -> Result<PaneContents>` (ReadPaneContents).

### Types

Canonical home: [docs.rs `zellij_tile`](https://docs.rs/zellij-tile/latest/zellij_tile/) / `zellij-utils/src/data.rs`. The shapes Rimz reads or will read, verified against 0.44.3 source:

```rust
enum PaneId { Terminal(u32), Plugin(u32) }

struct PaneManifest { panes: HashMap<usize /* tab position */, Vec<PaneInfo>> }
```

`PaneInfo` (full shape on docs.rs) groups into identity (`id`, `is_plugin`, `title`), state bits (`is_focused`, `is_fullscreen`, `is_floating`, `is_suppressed`, `is_selectable`, `exited`, `exit_status: Option<i32>`, `is_held`), geometry (`pane_x/y/rows/columns` including the frame, `pane_content_*` excluding it, `cursor_coordinates_in_pane`), the spawn surface (`terminal_command` — a command pane's **launch** command, not the live foreground command — and `plugin_url`), and per-client chrome (`index_in_pane_group: BTreeMap<ClientId, usize>`, `default_fg`/`default_bg` color strings). The subset the presence plugin folds — and why `title` is carried but excluded from the change hash — lives in code: [`policy.rs::PaneFields`](../../../crates/rimz-presence-zellij/src/policy.rs).

`is_focused` is a projected focus mark, not a uniqueness guarantee. In Zellij 0.44.3 a reconnect-churned SSH room with one listed client has been observed reporting multiple durable `is_focused: true` terminal panes in one tab; treat the marks as candidates, use `TabInfo.other_focused_clients` as related client-focus evidence, and let Rimz's presence plugin or producer resolve the tab's structural active pane. `is_held` means the command pane sits at the `Press ENTER to run` banner — the resurrection fingerprint Rimz's pre-attach gate keys on. There is **no pid, cwd, or live-command field** — those are the request/response host commands above, or `CommandChanged`/`CwdChanged` pushes.

| Type | Fields (abridged) |
| --- | --- |
| `TabInfo` | `position`, `name`, `active`, `panes_to_hide`, `is_fullscreen_active`, `is_sync_panes_active`, `are_floating_panes_visible`, `other_focused_clients: Vec<ClientId>`, `active_swap_layout_name`, `is_swap_layout_dirty`, `viewport_rows/columns`, `display_area_rows/columns`, `selectable_tiled/floating_panes_count`, `tab_id: usize`, `has_bell_notification`, `is_flashing_bell` |
| `SessionInfo` | `name`, `tabs: Vec<TabInfo>`, `panes: PaneManifest`, `connected_clients: usize`, `is_current_session`, `available_layouts`, `plugins: BTreeMap<u32, PluginInfo>`, `web_clients_allowed`, `web_client_count`, `tab_history` / `pane_history: BTreeMap<ClientId, Vec<PaneId>>`, `creation_time` |
| `SessionListSnapshot` | `live_sessions: Vec<SessionInfo>`, `resurrectable_sessions: Vec<(String, Duration)>` |
| `ClientInfo` | `client_id: ClientId (u16)`, `pane_id: PaneId`, `running_command: String`, `is_current_client` |
| `PluginIds` | `plugin_id: u32`, `zellij_pid: u32`, `initial_cwd: PathBuf`, `client_id: ClientId` |
| `CommandToRun` | `path: PathBuf`, `args: Vec<String>`, `cwd: Option<PathBuf>` |
| `FileToOpen` | `path`, `line_number: Option<usize>`, `cwd: Option<PathBuf>` |
| `FloatingPaneCoordinates` | `x/y/width/height: Option<PercentOrFixed>`, `pinned: Option<bool>`, `borderless: Option<bool>` |
| `PercentOrFixed` | `Percent(usize)` 1–100 \| `Fixed(usize)` |
| `LayoutInfo` | `BuiltIn(String)` (`"default"`, `"compact"`, `"welcome"`) \| `File(String, LayoutMetadata)` \| `Url(String)` \| `Stringified(String)` raw KDL |
| `MessageToPlugin` | `plugin_url: Option<String>`, `destination_plugin_id: Option<u32>`, `plugin_config`, `message_name`, `message_payload`, `message_args`, `new_plugin_args: Option<NewPluginArgs>`, `floating_pane_coordinates` |
| `NewPluginArgs` | `should_float`, `pane_id_to_replace`, `pane_title`, `cwd`, `skip_cache`, `should_focus` |
| `PaneContents` | `viewport: Vec<String>`, `lines_above_viewport` / `lines_below_viewport` (full-scrollback requests only), `selected_text: Option<SelectedText>` |
| `RegexHighlight` | `pattern`, `style: HighlightStyle`, `layer: HighlightLayer` (`Hint` < `Tool` < `ActionFeedback`), `context` (echoed on `HighlightClicked`), `on_hover`, `bold/italic/underline`, `tooltip_text` |
| `ModeInfo` | `mode: InputMode`, `base_mode`, `keybinds`, `style`, `capabilities`, `session_name`, `editor`, `shell`, web fields |
| `KeyWithModifier` | `bare_key: BareKey` (`Char(c)`, `Enter`, `F(n)`, …), `key_modifiers: BTreeSet<KeyModifier>` (`Ctrl`/`Alt`/`Shift`/`Super`) |
| `Mouse` | `ScrollUp/Down(usize)`, `LeftClick/RightClick/Hold/Release/Hover(line: isize, col: usize)` |
| `InputMode` | `Normal`, `Locked`, `Resize`, `Pane`, `Tab`, `Scroll`, `EnterSearch`, `Search`, `RenameTab`, `RenamePane`, `Session`, `Move`, `Prompt`, `Tmux` |

Integer-width inconsistency to keep straight: `TabInfo.tab_id` is `usize`, `close_tab_with_id` / `rename_tab_with_id` take `u64`, `break_panes_to_tab_with_id` takes `usize`; terminal/plugin pane ids are `u32` inside `PaneId`.

### Permissions

17 `PermissionType` variants (0.44.3 source):

| Permission | Grants (upstream display string) |
| --- | --- |
| `ReadApplicationState` | Access Zellij state (panes, tabs, UI) — most read events and queries |
| `ChangeApplicationState` | Change Zellij state and run actions — focus, close, resize, tabs, sessions |
| `OpenFiles` | Open files (editor panes) |
| `RunCommands` | Run commands (background `run_command` and command panes) |
| `OpenTerminalsOrPlugins` | Start new terminals and plugins |
| `WriteToStdin` | Write to a pane's STDIN as the user |
| `WebAccess` | Make web requests |
| `ReadCliPipes` | Control CLI pipe input/output |
| `MessageAndLaunchOtherPlugins` | Pipe messages to and launch other plugins |
| `Reconfigure` | Change runtime configuration and keybinds |
| `FullHdAccess` | Filesystem access beyond the plugin's host folder |
| `StartWebServer` | Control the session web server |
| `InterceptInput` | Intercept keyboard/mouse input |
| `ReadPaneContents` | Read pane viewport/scrollback |
| `RunActionsAsUser` | Execute Zellij actions as the user |
| `WriteToClipboard` | Write to the clipboard |
| `ReadSessionEnvironmentVariables` | Read env vars present at session creation |

`request_permission(&[…])` raises one floating prompt for the whole batch; the answer arrives as `PermissionRequestResult`. Zellij persists grants in its permission cache **keyed on the exact plugin path string** — canonicalize before loading or the grant misses ([multiplexers.md](../../internals/sidebar/multiplexers.md#zellij-presence-channel)). A cached grant re-delivers **no** `PermissionRequestResult` on later loads; treat subscribed state flowing as the grant signal. Upstream documents no cache file location or revocation UX beyond the plugin manager.

### Workers

For long-running work without wasm threads. Declared with `register_worker!(TestWorker, test_worker, TEST_WORKER)` — the namespace is the middle token minus the `_worker` suffix (here: addressed as `"test"`).

```rust
pub trait ZellijWorker<'de>: Default + Serialize + Deserialize<'de> {
    fn on_message(&mut self, message: String, payload: String) {}
}
```

Plugin → worker: `post_message_to(PluginMessage { name, payload, worker_name })`. Worker → plugin: `post_message_to_plugin(…)`, delivered as a `CustomMessage(message, payload)` event (subscribe first). Both directions are stringly typed; serialization strategy is the plugin's own.

### Pipes

A pipe sends messages to one or more plugins, **launching a target plugin on first message** (the pipe waits for the load, then delivers). Delivered to the plugin's `pipe` method as:

```rust
PipeMessage {
    source: PipeSource,        // Cli(input_pipe_id: uuid) | Plugin(source_plugin_id) | Keybind
    name: String,              // user-provided, or a random UUID
    payload: Option<String>,
    args: BTreeMap<String, String>,
    is_private: bool,          // true when targeted at this plugin; false when broadcast
}
```

- A pipe with no explicit destination **broadcasts to all running plugins**; `--plugin <url>` targets (and launch-or-messages) one. Same URL + different `--plugin-configuration` = a **different plugin identity** for destination matching.
- **CLI backpressure:** the piping process's STDIN buffer is released only after the plugin renders (or declines to render). `block_cli_pipe_input(id)` holds the pipeline; `unblock_cli_pipe_input(id)` resumes it; `cli_pipe_output(id, data)` writes to the CLI pipe's STDOUT independently. Several plugins can hold/feed the same pipe if they share its id.
- Plugin → plugin: `pipe_message_to_plugin(MessageToPlugin)`; destination `zellij:OWN_URL` expands to the caller's own URL (self-replication — guard against config-keyed message loops).
- The CLI side (`zellij pipe`) blocks while a launched plugin's permission prompt is pending — bound the call and reap only the CLI client ([multiplexers.md](../../internals/sidebar/multiplexers.md#zellij-presence-channel)).

### Keybind actions

Plugin keybind KDL nodes parse to the same `Action::KeybindPipe` variant: the action shape carries `plugin: Option<String>`, `plugin_id: Option<u32>` (which supersedes `plugin`), `configuration`, `launch_new`, `skip_cache`, optional `cwd`, and pane launch hints ([`actions.rs`](https://github.com/zellij-org/zellij/blob/v0.44.3/zellij-utils/src/input/actions.rs#L513-L525)). `MessagePlugin "<url>" { … }` fills the URL destination, child configuration, optional `cwd`, and defaults `launch_new` and `skip_cache` to `false` unless child nodes set them ([`kdl/mod.rs`](https://github.com/zellij-org/zellij/blob/v0.44.3/zellij-utils/src/kdl/mod.rs#L2152-L2208)). `MessagePluginId <id> { … }` fills `plugin_id`, leaves URL/configuration/cwd unset, and hardcodes `launch_new` and `skip_cache` to `false` ([`kdl/mod.rs`](https://github.com/zellij-org/zellij/blob/v0.44.3/zellij-utils/src/kdl/mod.rs#L2211-L2248)).

On Zellij 0.44.x, plugin keybinds that dispatch `KeybindPipe` can pause the UI for about one second before the action completes; upstream tracks this in [zellij #4635](https://github.com/zellij-org/zellij/issues/4635).

## CLI surface (0.44.3)

From the installed binary's `--help`.

### Top level

```text
zellij [OPTIONS] [SUBCOMMAND]
  -s, --session <name>      name a new session
  -l, --layout <name|path>  inside a session (or with --session): adds the layout's tabs to it; otherwise starts a new session
      --layout-string <kdl> same, from a raw KDL string
  -n, --new-session-with-layout <name|path>  always a new session, even from inside one
  -c, --config <file>       [env: ZELLIJ_CONFIG_FILE]   --config-dir <dir> [env: ZELLIJ_CONFIG_DIR]
      --data-dir <dir>      plugin lookup    --max-panes <n>    -d, --debug
```

Subcommands (aliases): `action`/`ac` · `attach`/`a` · `run`/`r` · `edit`/`e` · `plugin`/`p` · `pipe` · `subscribe` · `watch`/`w` · `web` · `options` · `setup` · `list-sessions`/`ls` · `list-aliases`/`la` · `kill-session`/`k` · `kill-all-sessions`/`ka` · `delete-session`/`d` · `delete-all-sessions`/`da` · `convert-config` / `convert-layout` / `convert-theme` (YAML→KDL migration). `run`, `edit`, and `plugin` print the created pane id (`terminal_<id>` / `plugin_<id>`) — treat it as a hint, not an authority ([caveats](../../internals/sidebar/multiplexers.md#zellij-backend-caveats)).

### `attach`

```text
zellij attach [OPTIONS] [SESSION_NAME] [options …]
  -c, --create               create if absent (attached)
  -b, --create-background    create DETACHED if absent — the Rimz birth verb
  -f, --force-run-commands   resurrecting: run held commands immediately (skip the ENTER banner)
      --forget               delete the saved session before connecting
      --index <n>            pick session by creation-order index
  remote/web auth: -t/--token, -r/--remember (4-week re-auth), --ca-cert <pem>, --insecure
```

`attach <session> options --…` applies room options onto the attach (and `attach --create-background <s> options --…` onto birth) — this is the channel Rimz uses for per-machine `[zellij]` config. **`options` accepts every config option as a kebab-case flag** (42 flags at 0.44.3 — `zellij options --help` is the authority). `attach` doubles as the remote client for `zellij web`-served sessions, hence the token/TLS flags.

### `action` catalog

`zellij action <verb>` targets the session from inside; `zellij --session <name> action <verb>` from anywhere. Every `--pane-id` takes the `terminal_<n>` / `plugin_<n>` form (or a bare number).

| Group | Verbs |
| --- | --- |
| Query | `list-panes [--tab] [--command] [--state] [--geometry] [--all] [--json/-j]` · `list-tabs [--state] [--dimensions] [--panes] [--layout] [--all] [--json]` · `list-clients` · `current-tab-info [--json]` · `query-tab-names` · `dump-layout` · `dump-screen [--path f] [--full] [--ansi] [--pane-id]` |
| Panes | `new-pane [--direction right\|down] [--floating] [--in-place] [--cwd] [--name] [--close-on-exit] [--start-suspended] [--stacked] [--tab-id] [--near-current-pane] [--borderless true\|false] [--plugin url] [--blocking \| --block-until-exit[-success\|-failure]] [-- cmd…]` · `close-pane [--pane-id]` · `rename-pane` / `undo-rename-pane` · `move-pane [dir]` / `move-pane-backwards` · `resize [dir\|+\|-]` · `clear` · `toggle-fullscreen` · `stack-panes -- id…` |
| Floating | `toggle-floating-panes` · `show-/hide-floating-panes [--tab-id]` · `are-floating-panes-visible` (exit code) · `toggle-pane-embed-or-floating` · `toggle-pane-pinned` · `change-floating-pane-coordinates --pane-id [--x --y --width --height --pinned --borderless]` |
| Style | `set-pane-color [--pane-id] [--fg c] [--bg c] [--reset]` · `toggle-pane-borderless` / `set-pane-borderless` · `toggle-pane-frames` |
| Focus | `focus-pane-id <id>` (implicitly switches to the holding tab) · `focus-next-pane` / `focus-previous-pane` · `move-focus [dir]` / `move-focus-or-tab [dir]` |
| Tabs | `new-tab [--layout path] [--layout-string kdl] [--layout-dir] [--name] [--cwd] [--initial-plugin url] [block flags] [-- cmd…]` · `close-tab [--tab-id]` / `close-tab-by-id` · `go-to-tab <idx>` / `go-to-tab-by-id` / `go-to-tab-name [--create]` / `go-to-next/previous-tab` · `rename-tab` / `rename-tab-by-id` / `undo-rename-tab` · `move-tab [right\|left]` · `toggle-active-sync-tab` |
| Input | `send-keys [--pane-id] <key…>` (named keys: `"Enter"`, `"ctrl c"`) · `write [--pane-id] <bytes…>` · `write-chars [--pane-id] <str>` · `paste [--pane-id] <text>` (bracketed paste) |
| Scroll | `scroll-up/down`, `scroll-to-top/bottom`, `page-` and `half-page-scroll-up/down` — all `[--pane-id]` · `edit-scrollback [--ansi]` |
| Plugins | `launch-plugin <url> [--floating] [--in-place] [--configuration k=v,…] [--skip-plugin-cache] [--tab-id]` · `launch-or-focus-plugin <url> [--move-to-focused-tab] […]` · `start-or-reload-plugin <url> [--configuration]` — requires a connected client and exits 0 even when refused (verified live, 0.44.3) · `pipe` (same surface as top-level `zellij pipe` plus `--force-launch-plugin`, `--skip-plugin-cache`, `--floating-plugin`, `--in-place-plugin`, `--plugin-cwd`, `--plugin-title`) |
| Layout | `override-layout <path> [--layout-string kdl] [--retain-existing-terminal-panes] [--retain-existing-plugin-panes] [--apply-only-to-active-tab]` · `next-/previous-swap-layout` |
| Session | `detach` · `rename-session <name>` · `save-session` (force a serialization) · `switch-session <name> [--tab-position] [--pane-id] [--layout] [--cwd]` · `switch-mode <mode>` |
| Theme | `set-dark-theme` / `set-light-theme` / `toggle-theme` |
| Files | `edit <path> [--line-number n] [--direction] [--floating] [--in-place] [--cwd] [--tab-id] [floating geometry]` |

`list-panes -j` output (0.44) carries per-pane `id`, internal `tab_id`, `tab_position`, `tab_name`, `title`, and geometry, plus the *spawn* command — `terminal_command` for command panes (preserved verbatim across an in-place re-exec), `pane_command` for default-shell panes (the shell) — and **no durable live foreground command, cwd, or pid**: live process state is plugin-surface-only (the `get_pane_*` trio and the `CommandChanged`/`CwdChanged` pushes above). Rimz's `pane-topology.json` cache is not raw `list-panes`: the presence plugin keeps a per-pane foreground map from `CommandChanged` and publishes that as `pane_command` beside the spawn `terminal_command`, so topology and CLI reads populate foreground and spawn as distinct fields. In 0.44.3 source, the route handler enters `enrich_panes_with_pty_data` whenever JSON output is requested, independent of the selected field flags; that enrichment performs command and cwd PTY round trips per terminal pane, so Rimz avoids `-j` on the hot path when the presence plugin has published fresh topology.

**Blocking panes** turn panes into pipeline steps: `--blocking` waits for exit *and pane close*; `--block-until-exit` waits for any exit; `--block-until-exit-success` / `-failure` gate on the status — available on `new-pane`, `new-tab`, and `zellij run`. Exit code propagates to the caller, so `zellij action new-pane --block-until-exit-success -- cargo test && next-step` works.

### `run` / `edit` / `plugin`

`zellij run [flags] -- <cmd…>` opens a **command pane**: it survives command exit showing the status on the frame, `ENTER` re-runs, `Ctrl-c` closes. Flags: `-d/--direction` (splits **right/down only**), `-f/--floating` with `--width/--height/--x/--y` (int or `%`; **ignored for tiled panes** — a sized left pane requires a layout), `-i/--in-place [--close-replaced-pane]`, `-c/--close-on-exit`, `-s/--start-suspended`, `-n/--name`, `--cwd`, `-b/--borderless <true|false>`, `--near-current-pane`, `--pinned`, `--stacked`, plus the block flags. Shell aliases upstream suggests: `zr`, `zrf`, `ze`.

`zellij edit <file>` opens `$EDITOR`/`$VISUAL` (or `scrollback_editor`) in a pane; same geometry flags plus `-l/--line-number`.

`zellij plugin [flags] -- <url>` loads a plugin pane; `-c/--configuration k=v`, `-s/--skip-plugin-cache`, floating geometry, `--tab-id`.

### `pipe`

```text
zellij pipe [--name n] [--args a=b,…] [--plugin url] [--plugin-configuration k=v,…] [--] <payload>
```

Blank payload reads STDIN line-buffered with plugin backpressure; plugin `cli_pipe_output` lands on this command's STDOUT. No `--plugin` = broadcast to all listening plugins. `--plugin` launches the target if absent — and **works clientless**, the one plugin-load verb that does.

### `subscribe` / `watch`

`zellij [--session s] subscribe --pane-id <id>… [--format raw|json] [--ansi] [--scrollback [n]]` streams rendered pane output: the live CLI alternative to the plugin `ReadPaneContents` surface, JSON events shaped `{ "event": "pane_update", "viewport": [...] }`. `zellij watch [session]` attaches read-only.

### `web`

`zellij web [--start|--stop|--status] [-d/--daemonize] [--ip] [--port]` (defaults `127.0.0.1:8082`) `[--cert/--key]` (required off-localhost). Token auth: `--create-token [--token-name]` (shown once), `--create-read-only-token` (watcher-only), `--revoke-token <name>`, `--revoke-all-tokens`, `--list-tokens`. Pairs with the `web_server*` / `web_sharing` config options and `attach`'s token flags. Rimz's use lives in [web.md](../../internals/reach/web.md).

### `setup` and sessions

`zellij setup --check` (print resolved dirs) · `--dump-config` · `--dump-layout <name>` / `--dump-swap-layout <name>` · `--dump-plugins [dir]` (materialize the builtin wasm) · `--clean` (run with shipped defaults, no config) · `--generate-completion <shell>` · `--generate-auto-start <shell>`.

`list-sessions [-s/--short] [-n/--no-formatting] [-r/--reverse]` — exits 0 with empty output when no server runs; dead sessions are annotated `EXITED - attach to resurrect`. `kill-session <name>` stops a live session (stays resurrectable); `delete-session [-f/--force] <name>` removes the serialized state too (`--force` kills first). `list-aliases` prints the plugin alias table.

## Configuration

KDL, one file. Lookup order: `--config-dir` flag → `$ZELLIJ_CONFIG_DIR` → `$HOME/.config/zellij` → platform default (Linux XDG; macOS `~/Library/Application Support/org.Zellij-Contributors.Zellij`) → `/etc/zellij`. `zellij setup --dump-config` prints the default. Zellij **watches the active config file and live-applies most changes**. A missing config births the first-run setup wizard, which silently drops `action new-pane` mounts while showing ([caveats](../../internals/sidebar/multiplexers.md#zellij-backend-caveats)); note Zellij prefers and creates `$HOME/.config/zellij` over `$XDG_CONFIG_HOME/zellij`.

### Options catalog

Top-level KDL options (`option_name value`). Every one doubles as a kebab-case `options` CLI flag. Field names verified against the 0.44.3 source.

| Option | Values (default first) | Note |
| --- | --- | --- |
| `default_mode` | `normal` \| `locked` | Rimz always sets `locked` so typing reaches the agent pane |
| `default_shell` | `$SHELL` | |
| `default_cwd` | path | |
| `default_layout` | `default` | name in the layout dir |
| `layout_dir` / `theme_dir` | path | default: subdir of config dir |
| `theme` / `theme_dark` / `theme_light` | `default` / theme name | dark/light pair drives `ToggleTheme` and `HostTerminalThemeChanged` auto-switching |
| `on_force_close` | `detach` \| `quit` | SIGTERM/SIGINT/SIGQUIT/SIGHUP behaviour |
| `session_name` | string | + `attach_to_session true\|false` to attach if it exists |
| `mirror_session` | `false` \| `true` | multi-client: mirror vs independent views |
| `mouse_mode` | `true` \| `false` | |
| `mouse_click_through` | `false` \| `true` | click focuses **and** reaches the pane — first-click jump; Rimz sets `true`; flag exists ≥ 0.44.0 |
| `advanced_mouse_actions` | `true` \| `false` | hover effects, pane grouping, mouse resize; flag exists ≥ 0.43.0 |
| `mouse_hover_effects` | `true` \| `false` | frame highlight + help text on hover; flag exists ≥ 0.44.0 |
| `focus_follows_mouse` | `false` \| `true` | 0.44: first click only passes through when click-through on **and** this off |
| `support_kitty_keyboard_protocol` | `true` \| `false` | |
| `copy_command` | e.g. `wl-copy` | replaces OSC52 |
| `copy_clipboard` | `system` \| `primary` | OSC52 destination |
| `copy_on_select` | `true` \| `false` | |
| `osc8_hyperlinks` | `false` \| `true` | |
| `scroll_buffer_size` | `10000` | lines per pane |
| `scrollback_editor` | `$EDITOR`/`$VISUAL` | |
| `styled_underlines` | `true` \| `false` | |
| `pane_frames` | `true` \| `false` | plus `ui { pane_frames { rounded_corners true; hide_session_name true } }` |
| `simplified_ui` | `false` \| `true` | no arrow fonts in plugins |
| `auto_layout` | `true` \| `false` | predefined swap-layout flow on new panes; Rimz passes `true` by default so Rimz birth layouts can apply the sidebar-and-compact-bar-pinning `rimz-work-area` swap layout |
| `stacked_resize` | `true` \| `false` | non-directional resize stacks neighbours |
| `visual_bell` | `true` \| `false` | |
| `show_startup_tips` / `show_release_notes` | `true` \| `false` | Rimz suppresses both |
| `session_serialization` | `true` \| `false` | Rimz passes `false` — see [resurrection](#session-serialization-and-resurrection) |
| `serialize_pane_viewport` | `false` \| `true` | |
| `scrollback_lines_to_serialize` | int; `0` = all | only with viewport serialization |
| `serialization_interval` | seconds | default ~1s |
| `disable_session_metadata` | `false` \| `true` | |
| `post_command_discovery_hook` | shell snippet | rewrites the discovered `$RESURRECT_COMMAND` |
| `env` | `env { KEY "value" }` | set on every terminal pane |
| `web_server` | `false` \| `true` | start server on startup; + `web_server_ip` (`127.0.0.1`), `web_server_port` (`8082`), `web_server_cert` / `web_server_key`, `enforce_https_for_localhost` |
| `web_sharing` | `"off"` \| `"on"` \| `"disabled"` | `disabled` cannot be re-enabled at runtime |

Plus the `keybinds`, `themes`, `plugins` (aliases), and `load_plugins` blocks. `load_plugins { "file:/path.wasm" }` starts background plugins at session start — config-level only; **a layout-level `load_plugins` does not exist** (verified live, 0.44.3), so a layout cannot be a plugin-load channel.

Boolean `options` CLI flags are toggles over config values, not absolute overrides: `Options::merge_from_cli` XORs a bool when both `config.kdl` and the CLI set it, and only uses the CLI value directly when the config omits that key. A room that needs an absolute boolean over user config must use the KDL merge/reconfigure path instead ([`merge_from_cli`](https://github.com/zellij-org/zellij/blob/v0.44.3/zellij-utils/src/input/options.rs#L443-L506), plain [`Options::merge`](https://github.com/zellij-org/zellij/blob/v0.44.3/zellij-utils/src/input/options.rs#L330-L371)).

### Mouse handling and reconfigure (0.44.3)

The tab mouse handler gathers `focus_follows_mouse` and `mouse_click_through` from tab state, then `determine_mouse_action` returns `FocusPaneAndClickThrough` for a plain left press on an inactive pane only when click-through is enabled and focus-follows-mouse is disabled; otherwise the press focuses the pane and the user needs a later click to reach the terminal/application ([`mouse_handler.rs`](https://github.com/zellij-org/zellij/blob/v0.44.3/zellij-server/src/tab/mouse_handler.rs#L396-L401), [`determine_mouse_action`](https://github.com/zellij-org/zellij/blob/v0.44.3/zellij-server/src/tab/mouse_handler.rs#L1196-L1441)).

`advanced_mouse_actions` participates in hover chrome, grouping, and resize branches; the click-through branch reads only `mouse_click_through` and `focus_follows_mouse` ([hover branch](https://github.com/zellij-org/zellij/blob/v0.44.3/zellij-server/src/tab/mouse_handler.rs#L1086-L1089), [click-through branch](https://github.com/zellij-org/zellij/blob/v0.44.3/zellij-server/src/tab/mouse_handler.rs#L1357-L1362)).

Runtime reconfigure parses the supplied KDL with the current client configuration as its base, then `Options::merge` applies only supplied option keys (`other.or(self)`); an absent option stays at its current live value, while an explicit option wins over `config.kdl`. `propagate_configuration_changes` then applies the resulting live config to all tabs ([server reconfigure path](https://github.com/zellij-org/zellij/blob/v0.44.3/zellij-server/src/lib.rs#L366-L385), [`Config::from_kdl`](https://github.com/zellij-org/zellij/blob/v0.44.3/zellij-utils/src/kdl/mod.rs#L4855-L4861), [`propagate_configuration_changes`](https://github.com/zellij-org/zellij/blob/v0.44.3/zellij-server/src/lib.rs#L387-L435)).

The client input handler enables mouse reporting from `mouse_mode`, converts terminal mouse events, and forwards them as `Action::MouseEvent` to the server; the mode-specific key handling sits on the keyboard path, so locked mode still forwards mouse events ([`input_handler.rs`](https://github.com/zellij-org/zellij/blob/v0.44.3/zellij-client/src/input_handler.rs#L149-L180), [mouse forwarding](https://github.com/zellij-org/zellij/blob/v0.44.3/zellij-client/src/input_handler.rs#L326-L330)).

Per-pane frameless rendering has two surfaces: `zellij action new-pane --borderless <true|false>` on the CLI and `borderless=true` on layout `pane` nodes. Both avoid changing global `pane_frames` ([CLI action definition](https://github.com/zellij-org/zellij/blob/v0.44.3/zellij-utils/src/cli.rs#L1607-L1620), [layout parser](https://github.com/zellij-org/zellij/blob/v0.44.3/zellij-utils/src/kdl/kdl_layout_parser.rs#L90-L91), [pane parse](https://github.com/zellij-org/zellij/blob/v0.44.3/zellij-utils/src/kdl/kdl_layout_parser.rs#L523-L533)).

### Plugin aliases

```kdl
plugins {
    tab-bar location="zellij:tab-bar"
    filepicker location="zellij:strider" {
        cwd "/"
    }
}
```

Bare alias names resolve in layouts, the CLI, keybinds, and pipes. Defaults shipped: `tab-bar`, `status-bar`, `compact-bar`, `strider`, `session-manager`, plus re-configured aliases onto the same wasm — `welcome-screen` (session-manager with `welcome_screen true`) and `filepicker` (strider with `cwd "/"`). An alias that sets `cwd` also receives `caller_cwd` (the focused pane's cwd) in its configuration. Built-in URLs use the `zellij:` scheme (`zellij:compact-bar`); the other schemes are `file:` and `http(s):`.

## Layout KDL

The surface Rimz's birth templates use ([multiplexers.md → Zellij backend](../../internals/sidebar/multiplexers.md#zellij-backend)). Root node `layout`; children: `pane`, `tab`, `pane_template`, `tab_template`, `default_tab_template`, `new_tab_template`, `floating_panes`, and a global `cwd`. Layouts apply **only at session birth** (or via `new-tab --layout` / `override-layout`). `zellij setup --dump-layout default` prints the built-in.

**`pane`** — leaf or container. Attributes: `split_direction "vertical"|"horizontal"` (container; default horizontal), `size "30%"|<fixed>` (upstream calls fixed sizes unstable — and a fixed size wider than a detached session's default 80×24 geometry kills the birth, hence Rimz's percentage-at-birth rule), `borderless`, `focus`, `name`, `cwd`, `command` + `args "a" "b"` (args only in child braces), `close_on_exit`, `start_suspended`, `edit "file"`, `plugin { location "zellij:…" }` (location only in child braces), `stacked` / `expanded`, `default_fg` / `default_bg`.

**`tab`** — `name`, `focus` (one tab), `split_direction`, `cwd`, `hide_floating_panes`, children panes.

**Templates** — `pane_template name="…"` / `tab_template name="…"` carry a `children` node marking the insertion point; consumers invoke them by name as nodes. A template with `command` accepts `args`/`cwd` from the consumer. **`default_tab_template`** shapes the initial tabs *and* every later tab — and **replaces Zellij's built-in template, dropping the tab/status bar unless re-added explicitly**. **`new_tab_template`** shapes only user-opened tabs and does not apply to the birth tabs. Two sharp edges Rimz hit: a `children` node nested inside a split is never auto-filled with a default terminal (spell the terminal pane explicitly), and on 0.44.3 a layout carrying a `new_tab_template` but **no `tab` node** kills a `--create-background` birth ([multiplexers.md](../../internals/sidebar/multiplexers.md#zellij-backend)).

**`floating_panes`** — child panes with `x`/`y`/`width`/`height` as fixed or `"%"` values.

**cwd composition** — relative paths chain pane ← tab ← global ← invocation dir (`/hi` + `there` + `friend` = `/hi/there/friend`); an absolute pane cwd overrides all parents.

Swap layouts (`*.swap.kdl`, `swap_tiled_layout` / `swap_floating_layout`) drive the `auto_layout` flow and `next-/previous-swap-layout`; Rimz includes a `rimz-work-area` `swap_tiled_layout` in every birth layout and explicit tab layout so no-direction pane opens rebalance the work area while the sidebar and compact-bar plugin stay fixed. Behaviours verified locally on 0.44.3: a template's `max_panes` budget counts plugin panes and assigns them to slots like terminals, so a template without a plugin slot re-tiles a status bar into the work area as a full-size pane; without an applicable swap layout, closing the first pane of a split leaves the survivor holding the closed pane's logical share, so a later no-direction `NewPane` halves that stale share instead of the work area; and a root `swap_tiled_layout` coexists with a `new_tab_template` only when no `default_tab_template` is present. `default_tab_template` plus a root swap layout requires a `children` node, which collapses the docked sidebar shape, so Rimz births from explicit `tab` nodes instead.

## Session serialization and resurrection

By default every session serializes to the user **cache folder** (`~/.cache/zellij/<contract_version>/session_info/<session>/`) on `serialization_interval` (~1s) as human-readable KDL layouts — the same dialect as `--layout`, shareable across machines. Serialized: the layout plus each pane's *discovered* command (`$RESURRECT_COMMAND`, rewritable via `post_command_discovery_hook`, whose STDOUT replaces the discovered command); optionally the viewport (`serialize_pane_viewport`) and scrollback (`scrollback_lines_to_serialize`).

A dead serialized session lists as `EXITED - attach to resurrect`; attach recreates the layout with every command pane **held at a `Press ENTER to run…` banner** (`PaneInfo.is_held`) — `attach -f/--force-run-commands` runs them immediately instead. `kill-session` keeps the serialized state; `delete-session` / `delete-all-sessions` removes it; `attach --forget` deletes before connecting. `session_serialization false` switches the whole machinery off — Rimz's choice, and why: agents cannot restore their running state, so a resurrected room is a wall of held panes ([multiplexers.md](../../internals/sidebar/multiplexers.md#zellij-backend)).
