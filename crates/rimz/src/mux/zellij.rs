//! Zellij `MuxBackend` implementation.
//!
//! Interactive actions run `zellij action <verb> ...` against the session
//! inferred from the caller's `ZELLIJ_SESSION_NAME` env var. Operations that
//! may run before the user attaches, such as native sidebar launch and wakeup
//! fanout, carry the session name explicitly via `zellij --session <name>`.
//!
//! Caveats live in `docs/internals/multiplexers.md` under
//! "Zellij backend caveats" — namely that raw Zellij pane IDs are
//! integers, scoped per-session, and that the spike does not yet expose
//! tab-level operations beyond what's needed to identify a pane.

mod layout;

use std::num::NonZeroU16;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use std::{env, fs};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use layout::{
    TempLayoutFile, render_background_view_layout, render_session_layout, render_sidebar_layout,
    render_tab_layout,
};

use super::{
    BackgroundViewLaunch, BackgroundViewOptions, ClientFocusOptions, CommandSpec, DaemonView,
    MuxBackend, MuxErr, PaneCapture, PaneListOptions, Result, SessionHealth, SessionOptions,
    SidebarLiveness, SidebarPaneOptions, SidebarRecovery, SidebarWidth, SplitPaneOptions,
    TabOptions, ViewSidebars, ensure_pane_backend,
};
use crate::config::ZellijConfig;
use crate::feed::PaneRef;
use crate::ids::{MuxName, PaneId, ViewKind};
use crate::ledger::{atomic, paths};

const EMBEDDED_PRESENCE_PLUGIN: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/rimz-presence-zellij.wasm"));
const PRESENCE_PLUGIN_FILE: &str = "rimz-presence-zellij.wasm";

/// Minimum Zellij version that ships the pipe-broadcast semantics Rimz uses
/// as a best-effort wakeup optimization.
pub const MIN_ZELLIJ_VERSION: (u32, u32, u32) = (0, 41, 0);

/// Minimum Zellij version that ships `advanced_mouse_actions`. Below this the
/// flag is unknown, so Rimz omits it and accepts Zellij's older defaults.
const MIN_ADVANCED_MOUSE_ACTIONS_VERSION: (u32, u32, u32) = (0, 43, 0);

/// Minimum Zellij version that ships the `mouse_click_through` option. Below
/// this the flag is unknown, so we omit it — a single click then focuses the
/// sidebar without reaching the renderer (degrade, never error).
const MIN_MOUSE_CLICK_THROUGH_VERSION: (u32, u32, u32) = (0, 44, 0);

/// Minimum Zellij version that ships `mouse_hover_effects`, the narrower
/// switch that suppresses hover chrome while leaving other mouse handling alone.
const MIN_MOUSE_HOVER_EFFECTS_VERSION: (u32, u32, u32) = (0, 44, 0);

/// Pane name the sidebar layout assigns, and the title Zellij reports back for
/// it. The sole source of truth for both rendering the layout and detecting
/// whether a live session still carries its sidebar — also the chrome label the
/// producer's pane-pid backfill skips, since sidebar panes share one cmdline and
/// are excluded from rows anyway.
pub const SIDEBAR_PANE_NAME: &str = "rimz-sidebar";

/// Zellij's action client occasionally answers `list-panes` with an empty
/// stdout and a success status when the session server is mid-tick — a known
/// race that a short retry clears. Without this, the sidebar's snapshot loop
/// flashes a "could not parse mux output: EOF" alert for a single blip.
const LIST_PANES_ATTEMPTS: u32 = 3;
const LIST_PANES_RETRY_DELAY: Duration = Duration::from_millis(50);

/// Per-attempt bound for the pre-attach health probe. A healthy action client
/// answers `list-panes` in milliseconds; a wedged one (busy-looping against a
/// dead session server) is SIGKILLed here and reads as uninspectable so `rimz
/// start` stops before deleting a room that may still hold live panes.
const HEALTH_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// `query-tab-names` can hit the same action-client startup race as
/// `list-panes`. Treat an empty successful response as transient; otherwise
/// `open_background_view` may miss an existing daemon tab and launch a
/// duplicate.
const TAB_NAMES_ATTEMPTS: u32 = 5;
const TAB_NAMES_RETRY_DELAY: Duration = Duration::from_millis(50);

/// Minimum Zellij that loads the presence plugin — the line `zellij-tile` is
/// pinned to, so plugin and host speak one protocol version. Older hosts
/// (down to [`MIN_ZELLIJ_VERSION`]) skip the load and keep today's pane poll;
/// everything else about them is unaffected.
pub const PRESENCE_PLUGIN_MIN_ZELLIJ: (u32, u32, u32) = (0, 44, 0);

/// Pipe name the presence-plugin launch sends its boot message down. The
/// message carries nothing — loading the plugin is the point, and delivery
/// itself releases the CLI — so the name only labels the launch in
/// `zellij`'s own tooling.
const PRESENCE_BOOT_PIPE: &str = "rimz_presence_boot";

/// Deadline for the presence-plugin boot pipe. Granted-and-running, the
/// pipe delivers and releases the CLI within milliseconds; permission-pending,
/// the CLI blocks until the user answers the prompt — so the kill at this
/// bound is the expected first-run path, and it reaps only the CLI client:
/// the plugin launch is already delivered server-side (verified on 0.44.3).
const PRESENCE_PIPE_TIMEOUT: Duration = Duration::from_secs(2);

/// Ceiling on how long `create_session_with_sidebar` holds the temp layout file
/// on disk while waiting for Zellij to parse it (Zellij reads `--default-layout`
/// asynchronously, after the create call returns). A *ceiling*, not a fixed
/// wait: a healthy birth materializes the sidebar sub-second and returns at
/// once; this only bounds the pathological case where the layout never parses,
/// where deleting the file early births a sidebar-less session.
const SIDEBAR_LAYOUT_TIMEOUT: Duration = Duration::from_secs(10);

/// Ceiling on how long an in-place sidebar add waits for its `new-pane` to
/// mount. `new-pane` prints the allocated pane id *before* the screen thread
/// mounts the pane, so the add discovers the mounted pane by listing rather
/// than trusting that id — a healthy mount lands within a tick or two; the
/// ceiling bounds a dropped mount (e.g. a detached session discards it).
const MOUNT_POLL_TIMEOUT: Duration = Duration::from_secs(2);
const MOUNT_POLL_STEP: Duration = Duration::from_millis(50);

/// A kept sidebar wider than this share of its tab's columns — and past the
/// `max_cols` cap, so a deliberately cap-wide pane on a narrow client never
/// reads as broken — is resized back toward the layout width. Tolerant by
/// design: a layout-born width never trips it; only a mis-mounted ~50% split
/// does, so geometry convergence can never churn a healthy pane.
const SIDEBAR_RESIZE_TRIGGER_PERCENT: u64 = 45;

/// Bundle reported by `rimz doctor` when the active backend is Zellij.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ZellijCapabilities {
    pub binary_version: String,
    pub parsed_version: Option<(u32, u32, u32)>,
    pub meets_min_version: bool,
}

/// Probe the installed Zellij. Cheap: one `zellij --version` call.
pub fn capabilities() -> Result<ZellijCapabilities> {
    let raw = ZellijBackend::default().version()?;
    let parsed = parse_version(&raw);
    Ok(ZellijCapabilities {
        meets_min_version: parsed.is_some_and(|v| v >= MIN_ZELLIJ_VERSION),
        binary_version: raw,
        parsed_version: parsed,
    })
}

/// Parse `"zellij 0.41.2"` (and tolerant of leading/trailing whitespace).
/// Returns None when the shape is unexpected so `doctor` can render the
/// raw string verbatim.
fn parse_version(raw: &str) -> Option<(u32, u32, u32)> {
    let trimmed = raw.trim();
    let after_prefix = trimmed.strip_prefix("zellij ").unwrap_or(trimmed);
    let mut parts = after_prefix
        .split(|c: char| !c.is_ascii_digit() && c != '.')
        .next()?
        .split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

/// Locate the presence-plugin wasm: the `RIMZ_PRESENCE_PLUGIN` override, else
/// the embedded plugin materialized under `$XDG_DATA_HOME/rimz/plugins/`, else
/// a development fallback beside the running executable. `None` leaves the
/// session in poll mode; `rimz doctor` names the missing artifact and the fix.
///
/// Canonical, because Zellij keys the user's one-time permission grant on the
/// exact path string it is handed: one real artifact must read as one string
/// however rimz was invoked (symlinked bin dir, relative exe), or the user is
/// re-prompted per spelling.
pub fn presence_plugin_path() -> Option<PathBuf> {
    if let Some(path) = env::var_os("RIMZ_PRESENCE_PLUGIN") {
        return PathBuf::from(path)
            .canonicalize()
            .ok()
            .filter(|path| path.is_file());
    }
    if let Some(path) = embedded_presence_plugin_path() {
        return Some(path);
    }
    env::current_exe()
        .ok()?
        .parent()?
        .join(PRESENCE_PLUGIN_FILE)
        .canonicalize()
        .ok()
        .filter(|path| path.is_file())
}

fn embedded_presence_plugin_path() -> Option<PathBuf> {
    match materialize_presence_plugin_bytes(EMBEDDED_PRESENCE_PLUGIN, &paths::data_home()) {
        Ok(Some(path)) => path.canonicalize().ok().filter(|path| path.is_file()),
        Ok(None) => None,
        Err(err) => {
            tracing::debug!(error = %err, "materializing embedded presence plugin failed");
            None
        }
    }
}

fn materialized_presence_plugin_path_under(data_root: &std::path::Path) -> PathBuf {
    data_root
        .join("rimz")
        .join("plugins")
        .join(PRESENCE_PLUGIN_FILE)
}

fn materialize_presence_plugin_bytes(
    bytes: &[u8],
    data_root: &std::path::Path,
) -> std::result::Result<Option<PathBuf>, atomic::AtomicErr> {
    if bytes.is_empty() {
        return Ok(None);
    }
    let path = materialized_presence_plugin_path_under(data_root);
    if fs::read(&path).is_ok_and(|existing| existing == bytes) {
        return Ok(Some(path));
    }
    atomic::write_bytes_atomically(&path, bytes)?;
    Ok(Some(path))
}

/// The `key=value,key=value` configuration the plugin reads at load. The
/// parse is Zellij's — split on `,` then `=` — so a `rimz` path containing
/// either separator cannot be expressed; it is omitted and the plugin falls
/// back to `rimz` on the host PATH rather than poke a mis-parsed argv.
/// Workspace ids are `ws_` + hex by construction, always expressible.
fn presence_plugin_configuration(opts: &super::PresencePluginOptions) -> String {
    let mut configuration = format!("workspace_id={}", opts.workspace_id.as_str());
    if opts.session_name.contains([',', '=']) {
        tracing::debug!(
            session = %opts.session_name,
            "session name contains a plugin-configuration separator; command-change shortcut is disabled",
        );
    } else {
        configuration.push_str(",session_name=");
        configuration.push_str(&opts.session_name);
    }
    let bin = opts.rimz_bin.to_string_lossy();
    if bin.contains([',', '=']) {
        tracing::debug!(
            rimz_bin = %bin,
            "rimz path contains a plugin-configuration separator; the plugin resolves `rimz` from PATH instead",
        );
    } else {
        configuration.push_str(",rimz_bin=");
        configuration.push_str(&bin);
    }
    configuration
}

/// `options` flags that forward a single click through the sidebar pane to the
/// renderer, gated on `parsed >= MIN_MOUSE_CLICK_THROUGH_VERSION`. Empty when
/// the version is older or unparseable — older Zellij does not know the flag, so
/// passing it would abort the launch; degrading to focus-then-click is the
/// floor.
fn mouse_click_through_args(enabled: bool, parsed: Option<(u32, u32, u32)>) -> Vec<String> {
    if enabled && parsed.is_some_and(|v| v >= MIN_MOUSE_CLICK_THROUGH_VERSION) {
        vec!["--mouse-click-through".to_owned(), "true".to_owned()]
    } else {
        Vec::new()
    }
}

fn versioned_bool_arg(
    flag: &str,
    value: bool,
    parsed: Option<(u32, u32, u32)>,
    min_version: (u32, u32, u32),
) -> Vec<String> {
    if parsed.is_some_and(|v| v >= min_version) {
        vec![flag.to_owned(), bool_value(value)]
    } else {
        Vec::new()
    }
}

fn bool_value(value: bool) -> String {
    if value { "true" } else { "false" }.to_owned()
}

/// Zellij `options` flags Rimz owns for its rooms. `mouse-click-through` is
/// version-gated separately because older supported Zellij builds reject the
/// unknown option; the rest are present at Rimz's Zellij floor.
fn zellij_options_args(
    config: &ZellijConfig,
    parsed_version: Option<(u32, u32, u32)>,
) -> Vec<String> {
    let mut args = vec![
        "--default-mode".to_owned(),
        config.default_mode.as_str().to_owned(),
        "--focus-follows-mouse".to_owned(),
        bool_value(config.focus_follows_mouse),
        "--pane-frames".to_owned(),
        bool_value(config.pane_frames),
        "--on-force-close".to_owned(),
        config.on_force_close.as_str().to_owned(),
        "--scroll-buffer-size".to_owned(),
        config.scroll_buffer_size.to_string(),
        "--show-startup-tips".to_owned(),
        bool_value(config.show_startup_tips),
        "--show-release-notes".to_owned(),
        bool_value(config.show_release_notes),
        "--copy-clipboard".to_owned(),
        config.copy_clipboard.as_str().to_owned(),
        "--copy-on-select".to_owned(),
        bool_value(config.copy_on_select),
        "--support-kitty-keyboard-protocol".to_owned(),
        bool_value(config.support_kitty_keyboard_protocol),
        "--osc8-hyperlinks".to_owned(),
        bool_value(config.osc8_hyperlinks),
        // Unconditional: `--session-serialization` predates Rimz's Zellij floor
        // (MIN_ZELLIJ_VERSION), so unlike `mouse-click-through` it needs no
        // version gate. Off keeps Zellij from minting a resurrectable corpse on
        // server death, so the next start births clean rather than resurrecting
        // suspended panes.
        "--session-serialization".to_owned(),
        bool_value(config.session_serialization),
    ];
    // Zellij's default is mouse_mode=true. On 0.44.3, passing
    // `--mouse-mode true` suppresses the terminal mouse-enable sequences, so
    // keep the enabled case implicit and emit only the user's opt-out.
    if !config.mouse_mode {
        args.extend(["--mouse-mode".to_owned(), "false".to_owned()]);
    }
    args.extend(mouse_click_through_args(
        config.mouse_click_through,
        parsed_version,
    ));
    args.extend(versioned_bool_arg(
        "--advanced-mouse-actions",
        config.advanced_mouse_actions,
        parsed_version,
        MIN_ADVANCED_MOUSE_ACTIONS_VERSION,
    ));
    args.extend(versioned_bool_arg(
        "--mouse-hover-effects",
        config.mouse_hover_effects,
        parsed_version,
        MIN_MOUSE_HOVER_EFFECTS_VERSION,
    ));
    args
}

#[derive(Debug, Default)]
pub struct ZellijBackend {
    /// Override for `XDG_RUNTIME_DIR`, where Zellij locates its server socket.
    /// `None` inherits the process env (production); integration tests set a
    /// private tempdir so each test drives its own Zellij server — the
    /// isolation that lets them run in parallel and across worktrees with no
    /// shared lock. Mirrors [`super::TmuxBackend`]'s `with_socket` seam.
    runtime_dir: Option<PathBuf>,
    /// Memoized `zellij --version` stdout ([`MuxBackend::version`]) — the
    /// birth and attach paths both probe the version gate, and one fork
    /// answers both. An instance lives one CLI command, so an upgraded binary
    /// is seen by the next command; the cache assumes the resolved binary
    /// (`RIMZ_ZELLIJ_BIN`, PATH) is stable for the instance's life. Only a
    /// successful probe is stored, so a transient failure retries.
    version: std::sync::OnceLock<String>,
}

impl ZellijBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pin every Zellij command this backend runs to `dir` as `XDG_RUNTIME_DIR`,
    /// so a test's server, sessions, and sockets never touch the user's.
    pub fn with_runtime_dir(dir: impl Into<PathBuf>) -> Self {
        Self {
            runtime_dir: Some(dir.into()),
            ..Self::default()
        }
    }

    /// Base `CommandSpec` for every Zellij invocation — the single chokepoint.
    /// The program honors `RIMZ_ZELLIJ_BIN` (the binary override the wakeup walk
    /// may point at a test shim; see `tests/fixtures/zellij-trace`), and the
    /// optional `XDG_RUNTIME_DIR` env scopes the server socket. Threading
    /// isolation through one field this way keeps it impossible for a stray
    /// command to escape to the user's default server.
    fn cmd(&self) -> CommandSpec {
        let program = env::var("RIMZ_ZELLIJ_BIN").unwrap_or_else(|_| "zellij".to_owned());
        let mut spec = CommandSpec::new(program);
        if let Some(dir) = &self.runtime_dir {
            spec = spec.env("XDG_RUNTIME_DIR", dir.to_string_lossy().into_owned());
        }
        spec
    }

    /// Probe the installed Zellij and resolve the session `options` flags for
    /// it. Empty version-gated flags on a probe failure or an unparseable
    /// version — never block a launch on optional mouse passthrough.
    fn zellij_options_args_probed(&self, config: &ZellijConfig) -> Vec<String> {
        let parsed = self.version().ok().as_deref().and_then(parse_version);
        zellij_options_args(config, parsed)
    }

    fn list_panes_with_session(&self, session: Option<&str>) -> Result<Vec<RawPane>> {
        self.list_panes_bounded(session, super::COMMAND_TIMEOUT)
    }

    /// `list-panes` with a caller-chosen per-attempt bound. The pre-attach health
    /// probe passes [`HEALTH_PROBE_TIMEOUT`] so a hung server cannot stall the
    /// launch; everyone else inherits [`super::COMMAND_TIMEOUT`].
    fn list_panes_bounded(&self, session: Option<&str>, timeout: Duration) -> Result<Vec<RawPane>> {
        let mut spec = self.cmd();
        if let Some(name) = session {
            spec = spec.args(["--session".to_owned(), name.to_owned()]);
        }
        spec = spec.args(["action", "list-panes", "-j", "-a"]);
        for attempt in 0..LIST_PANES_ATTEMPTS {
            if attempt > 0 {
                std::thread::sleep(LIST_PANES_RETRY_DELAY);
            }
            let output = spec.run_with_timeout(timeout)?;
            if is_transient_empty(&output.stdout) {
                continue;
            }
            return serde_json::from_slice::<Vec<RawPane>>(&output.stdout).map_err(|e| {
                MuxErr::Output {
                    program: "zellij".to_owned(),
                    reason: format!("parsing list-panes JSON: {e}"),
                }
            });
        }
        Err(MuxErr::Output {
            program: "zellij".to_owned(),
            reason: format!("list-panes returned no output after {LIST_PANES_ATTEMPTS} attempts"),
        })
    }

    /// Classify `name`'s live room from a bounded pane listing. A running
    /// (non-held) `rimz-sidebar` pane plus no held command pane is clean. A held
    /// sidebar means Zellij is waiting on the user (no heartbeats); a held command
    /// pane is the resurrection fingerprint — Zellij brought a serialized room
    /// back with `start_suspended` panes. Either inspected condition makes the
    /// room non-functional and safe to rebirth.
    ///
    /// A failed or timed-out listing is different: the room is uninspectable, not
    /// proven stale. Preserve it and let the caller surface the stuck-room path
    /// rather than force-deleting panes it could not see.
    fn session_cleanliness(&self, name: &str) -> Result<SessionCleanliness> {
        self.list_panes_bounded(Some(name), HEALTH_PROBE_TIMEOUT)
            .map(|panes| classify_session_panes(&panes))
    }

    /// Classify `name`'s liveness from `zellij list-sessions`. A present session
    /// always lists with exit code 0; the command only fails ("No active zellij
    /// sessions found.", exit 1) when there are none, so any failure here means
    /// the session is absent and a fresh birth should proceed.
    fn session_state(&self, name: &str) -> SessionState {
        let Ok(output) = self.cmd().args(["list-sessions", "--no-formatting"]).run() else {
            return SessionState::Absent;
        };
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| session_state_from_line(line, name))
            .unwrap_or(SessionState::Absent)
    }

    /// Force-delete a session (exited or live) so the next create births a clean
    /// one from the layout rather than resurrecting a stale serialized layout or
    /// attaching to a sidebar-less leftover. `--force` also kills a live session.
    /// A session that vanished between the liveness check and here is already in
    /// the state we want, so "not found" is success.
    fn delete_session(&self, name: &str) -> Result<()> {
        match self.cmd().args(["delete-session", name, "--force"]).run() {
            Ok(_) => Ok(()),
            Err(MuxErr::Command { stderr, .. })
                if stderr.to_ascii_lowercase().contains("not found") =>
            {
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    /// Create the background session from a layout that puts the `rimz-sidebar`
    /// pane on the left and focuses the user's terminal on the right. The layout
    /// doubles as the default tab template, so new tabs are born with a sidebar
    /// too. The sidebar pane is `close_on_exit`, so when its own process exits
    /// the pane closes — see the self-close loop in `sidebar_renderer::app`.
    ///
    /// Zellij parses `--default-layout` asynchronously, after the
    /// `--create-background` client returns, so the temp layout file must
    /// outlive the call. We hold it through a bounded wait for the sidebar pane
    /// to appear, then let it drop.
    fn create_session_with_sidebar(
        &self,
        opts: &SidebarPaneOptions,
        daemon: Option<&DaemonView>,
    ) -> Result<()> {
        // A daemon view leads only if it is born first, and resumed agents only
        // come back as command panes the birth layout spells out: Zellij can't
        // reorder tabs or add command panes after birth. So a room that leads
        // with a daemon and/or re-seeds prior agents is born from an explicit
        // multi-tab layout; a plain room uses the single working-tab template.
        let body = if daemon.is_some() || !opts.resume_panes.is_empty() {
            render_session_layout(opts, daemon, &opts.resume_panes)?
        } else {
            render_sidebar_layout(opts)?
        };
        let layout = TempLayoutFile::new(body)?;
        let mut option_args = vec![
            "attach".to_owned(),
            "--create-background".to_owned(),
            opts.session_name.clone(),
            "options".to_owned(),
        ];
        option_args.extend(self.zellij_options_args_probed(&opts.config.zellij));
        option_args.extend([
            "--default-cwd".to_owned(),
            opts.cwd.to_string_lossy().into_owned(),
            "--default-layout".to_owned(),
            layout.path().to_string_lossy().into_owned(),
        ]);
        let mut spec = self.cmd().args(option_args);
        // The identity pin rides the spawning client's environment: the
        // per-session server is forked from this command, and every pane is
        // forked from the server, so panes — and the agents and in-pane hook
        // children inside them — inherit the room's workspace transitively.
        // A daemon-routed hook child (Codex's per-user app-server) inherits
        // the daemon's env instead and recovers the pin from the in-pane
        // agent process (`resolve_participant_with_pin_recovery`). Zellij has
        // no post-birth `set-environment`, so birth is the one stamping
        // point; a pre-pin server keeps its env and its participants fall
        // back to the static ladder.
        for (key, value) in crate::workspace::pin_env(&opts.workspace_id, &opts.project_root) {
            spec = spec.env(key, value);
        }
        let spawn = || -> Result<bool> {
            let mut command = spec.to_command();
            command.current_dir(&opts.cwd);
            let output = command.output().map_err(|err| match err.kind() {
                std::io::ErrorKind::NotFound => MuxErr::NotInstalled {
                    program: spec.program.clone(),
                },
                _ => MuxErr::Io(err),
            })?;
            if output.status.success() {
                return Ok(true);
            }

            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let lower = stderr.to_ascii_lowercase();
            // A racing `rimz` may have created the session first; treat that as
            // success rather than re-injecting.
            if lower.contains("already exists")
                || (lower.contains("session") && lower.contains("exists"))
            {
                return Ok(false);
            }

            Err(MuxErr::Command {
                program: spec.program.clone(),
                args: spec.args.join(" "),
                stderr,
            })
        };

        let created = spawn()?;
        if self.wait_for_sidebar_layout(&opts.session_name) {
            drop(layout);
            return Ok(());
        }

        if created {
            tracing::warn!(
                session = %opts.session_name,
                "sidebar layout did not materialize within the ceiling; retrying session birth \
                 before dropping the temp layout",
            );
            self.delete_session(&opts.session_name)?;
            spawn()?;
            if self.wait_for_sidebar_layout(&opts.session_name) {
                drop(layout);
                return Ok(());
            }
        }

        if !created {
            tracing::warn!(
                session = %opts.session_name,
                "sidebar layout did not materialize within the ceiling; dropping the temp \
                 layout may leave the session sidebar-less — it self-heals on the next open_sidebar",
            );
        } else {
            tracing::warn!(
                session = %opts.session_name,
                "sidebar layout still did not materialize after retry; dropping the temp \
                 layout may leave the session sidebar-less — it self-heals on the next open_sidebar",
            );
        }
        drop(layout);
        Ok(())
    }

    /// `zellij --session <name> action <verb> …` — the session-scoped action
    /// form the pane listing and wakeup fanout use, so recovery works whether or
    /// not the caller is attached.
    fn zellij_action(&self, session: &str) -> CommandSpec {
        self.cmd().args([
            "--session".to_owned(),
            session.to_owned(),
            "action".to_owned(),
        ])
    }

    fn focus_terminal(&self, session: &str, raw_id: u64) -> Result<()> {
        self.zellij_action(session)
            .args(["focus-pane-id".to_owned(), format!("terminal_{raw_id}")])
            .run()
            .map(|_| ())
    }

    /// `zellij --session <name> action go-to-tab <index>` (1-based). Used to pull
    /// focus off a freshly `new-tab`'d daemon tab back to the leading working tab.
    fn go_to_tab(&self, session: &str, index: u32) -> Result<()> {
        self.zellij_action(session)
            .args(["go-to-tab".to_owned(), index.to_string()])
            .run()
            .map(|_| ())
    }

    /// Close a single pane by id (`close-pane --pane-id terminal_N`), terminating
    /// its process. Reconcile uses this to drop a duplicate or unresponsive
    /// sidebar pane without touching the rest of the tab.
    fn close_pane(&self, session: &str, pane: &PaneId) -> Result<()> {
        self.zellij_action(session)
            .args([
                "close-pane".to_owned(),
                "--pane-id".to_owned(),
                pane.raw().to_owned(),
            ])
            .run()
            .map(|_| ())
    }

    /// Inject a left-docked sidebar into a live tab without a rebirth: split a
    /// pane to the right, move it left, then resize it toward the layout width
    /// — trusting nothing `new-pane` prints. Take a before-set of the tab's
    /// pane ids, spawn the pane, *discover* the mounted pane by listing, then
    /// dock and resize it. On a mount that never lands or a dock that fails,
    /// undo — close the pane and kill the spawned serve pair — so a failed add
    /// never leaks a malformed pane or a paneless renderer.
    fn add_sidebar_to_tab(&self, opts: &SidebarPaneOptions, tab_id: u64) -> Result<()> {
        let before: std::collections::HashSet<u64> = self
            .list_panes_with_session(Some(&opts.session_name))?
            .iter()
            .filter(|pane| pane.is_terminal() && pane.tab_id == tab_id)
            .map(|pane| pane.id)
            .collect();
        // A `new-pane` failure is remembered, not fatal yet: concurrent action
        // clients can cross-talk responses, so the command can misreport while
        // the pane is still created — discovery gets its window either way.
        let (hint, spawn_err) = match self.new_sidebar_pane(opts, tab_id) {
            Ok(hint) => (hint, None),
            Err(err) => (None, Some(err)),
        };
        let Some(pane_id) =
            self.wait_for_mounted_sidebar(&opts.session_name, tab_id, &before, hint.as_deref())
        else {
            self.cleanup_failed_add(opts, hint.as_deref());
            return Err(spawn_err.unwrap_or_else(|| MuxErr::Output {
                program: "zellij".to_owned(),
                reason: format!("new-pane never mounted a sidebar pane in tab {tab_id}"),
            }));
        };
        if let Err(err) = self.dock_left(&opts.session_name, &pane_id) {
            self.cleanup_failed_add(opts, Some(&pane_id));
            return Err(err);
        }
        self.resize_sidebar_toward(&opts.session_name, tab_id, &pane_id, opts.width);
        Ok(())
    }

    /// Bounded poll for the sidebar pane an add just spawned to mount in
    /// `tab_id`. Returns its id (e.g. `terminal_58`), or `None` once
    /// [`MOUNT_POLL_TIMEOUT`] elapses — the mount was dropped.
    fn wait_for_mounted_sidebar(
        &self,
        session: &str,
        tab_id: u64,
        before: &std::collections::HashSet<u64>,
        hint: Option<&str>,
    ) -> Option<String> {
        let hint_raw = hint.and_then(parse_terminal_id);
        let deadline = Instant::now() + MOUNT_POLL_TIMEOUT;
        loop {
            if let Ok(panes) = self.list_panes_with_session(Some(session))
                && let Some(id) = mounted_sidebar_pane(&panes, tab_id, before, hint_raw)
            {
                return Some(format!("terminal_{id}"));
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(MOUNT_POLL_STEP);
        }
    }

    /// Move a pane to its tab's left column — the dock position the layout
    /// gives a sidebar at birth.
    fn dock_left(&self, session: &str, pane_id: &str) -> Result<()> {
        self.zellij_action(session)
            .args([
                "move-pane".to_owned(),
                "left".to_owned(),
                "--pane-id".to_owned(),
                pane_id.to_owned(),
            ])
            .run()
            .map(|_| ())
    }

    /// Converge one kept sidebar pane onto the layout's dock, in place and
    /// without touching its renderer: a bounded move-left loop (re-listing
    /// between steps — `move-pane left` swaps one position per call) until the
    /// pane reaches the left column or stops progressing, then a resize back
    /// toward the layout width when it is still past the trigger. Returns
    /// whether any repair was issued. Best-effort: geometry is cosmetic, so
    /// any failure just leaves the pane where it is for the next pass.
    fn converge_sidebar_geometry(
        &self,
        opts: &SidebarPaneOptions,
        tab_id: u64,
        raw_id: u64,
    ) -> bool {
        const REDOCK_MAX_STEPS: u32 = 4;
        let pane_raw = format!("terminal_{raw_id}");
        let mut repaired = false;
        let mut last_x = u64::MAX;
        for _ in 0..REDOCK_MAX_STEPS {
            let Ok(panes) = self.list_panes_with_session(Some(&opts.session_name)) else {
                break;
            };
            // Plugin panes share the integer id space (`plugin_1` beside
            // `terminal_1`), so the terminal filter is load-bearing here.
            let Some(pane) = panes
                .iter()
                .find(|pane| pane.is_terminal() && pane.tab_id == tab_id && pane.id == raw_id)
            else {
                break;
            };
            let Some(x) = pane.pane_x else { break };
            if x == 0 || x >= last_x {
                break; // docked, or no progress — stop rather than spin.
            }
            last_x = x;
            if self.dock_left(&opts.session_name, &pane_raw).is_err() {
                break;
            }
            repaired = true;
        }
        if let Some((cols, total)) = self.sidebar_and_tab_cols(&opts.session_name, tab_id, raw_id)
            && sidebar_width_off_spec(cols, total, opts.width)
        {
            self.resize_sidebar_toward(&opts.session_name, tab_id, &pane_raw, opts.width);
            repaired = true;
        }
        repaired
    }

    /// Undo a failed add: best-effort close the pane (a never-mounted id reads
    /// "not found" — fine), then kill the spawned serve pair still attributed
    /// to it, which a dropped mount leaves running with no pane to paint.
    fn cleanup_failed_add(&self, opts: &SidebarPaneOptions, pane_id: Option<&str>) {
        let Some(raw) = pane_id else {
            // No id to attribute by; the post-reconcile orphan reap catches a
            // pair whose pane the mux never lists.
            return;
        };
        let pane = PaneId::from_parts(MuxName::Zellij, raw);
        let _ = self.close_pane(&opts.session_name, &pane);
        let killed = super::recovery::kill_sidebar_serve_for_pane(
            opts.workspace_id.as_str(),
            &opts.session_name,
            &pane,
            MuxName::Zellij,
        );
        if killed > 0 {
            tracing::debug!(
                session = %opts.session_name,
                pane = raw,
                killed,
                "sidebar add cleanup: reaped the unmounted serve pair",
            );
        }
    }

    /// Whether `session` has at least one attached client. `list-clients`
    /// prints a header line plus one row per client, so header-only output
    /// reads detached. An unanswerable probe also reads detached — deferring
    /// an add for one run is recoverable, a leaked serve pair is not.
    fn session_has_attached_client(&self, session: &str) -> bool {
        self.zellij_action(session)
            .arg("list-clients")
            .run()
            .map(|output| {
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .skip(1)
                    .any(|line| !line.trim().is_empty())
            })
            .unwrap_or(false)
    }

    /// `new-pane` to the right of the tab's focus, titled and `close_on_exit` to
    /// match the layout, running the same `rimz sidebar serve` command. Returns
    /// the created pane id Zellij prints (e.g. `terminal_58`) — as a *hint*
    /// only: under concurrent action clients the stdout can carry another
    /// client's response or nothing, while the pane is still created.
    fn new_sidebar_pane(&self, opts: &SidebarPaneOptions, tab_id: u64) -> Result<Option<String>> {
        let args: Vec<String> = vec![
            "new-pane".to_owned(),
            "--direction".to_owned(),
            "right".to_owned(),
            "--tab-id".to_owned(),
            tab_id.to_string(),
            "--name".to_owned(),
            SIDEBAR_PANE_NAME.to_owned(),
            "--close-on-exit".to_owned(),
            "--cwd".to_owned(),
            opts.cwd.to_string_lossy().into_owned(),
            "--".to_owned(),
            opts.rimz_bin.to_string_lossy().into_owned(),
            "sidebar".to_owned(),
            "serve".to_owned(),
            "--mux".to_owned(),
            "zellij".to_owned(),
            "--workspace-id".to_owned(),
            opts.workspace_id.as_str().to_owned(),
            "--session-name".to_owned(),
            opts.session_name.clone(),
        ];
        let output = self.zellij_action(&opts.session_name).args(args).run()?;
        Ok(parse_new_pane_id(&String::from_utf8_lossy(&output.stdout)))
    }

    /// Shrink the reconcile heal path's freshly-split sidebar (born at ~50% —
    /// `new-pane` has no tiled-size flag) toward the configured width — the
    /// percentage of the tab at the `max_cols` cap — landing on the width
    /// *closest* to the target without ever finishing above the cap. Measures
    /// live tab geometry each step, so it is correct from any invoking
    /// terminal. The resize step is coarse, so the target usually falls
    /// between two reachable widths; stopping at the first width at or below
    /// it can overshoot, so when the prior width was closer we step back up
    /// one — but only when that prior width respects the cap, so a cap-bound
    /// target always lands at or below it (a final width one step over the
    /// cap reads as the cap never applying). Bounded and best-effort: it
    /// stops at the target, when a step makes no progress (hit a minimum), or
    /// after [`RESIZE_MAX_STEPS`] — never a dead loop. Width is cosmetic, so
    /// any failure just leaves the wider pane.
    fn resize_sidebar_toward(
        &self,
        session: &str,
        tab_id: u64,
        pane_id: &str,
        width: SidebarWidth,
    ) {
        const RESIZE_MAX_STEPS: u32 = 16;
        let Some(target_raw) = parse_terminal_id(pane_id) else {
            return;
        };
        let mut last_cols = u64::MAX;
        for _ in 0..RESIZE_MAX_STEPS {
            let Some((cols, total)) = self.sidebar_and_tab_cols(session, tab_id, target_raw) else {
                return;
            };
            if total == 0 {
                return;
            }
            let target = width.target_cols(total);
            if cols <= target {
                // Reached/overshot the target. If the previous, above-target
                // width was closer than this one, the last decrease overshot —
                // step back, but never to a width above the cap.
                if last_cols != u64::MAX
                    && last_cols <= width.cap_cols()
                    && last_cols.saturating_sub(target) < target.saturating_sub(cols)
                {
                    let _ = self.resize_sidebar_step(session, pane_id, "increase");
                }
                return;
            }
            if cols >= last_cols {
                return; // no progress (hit a minimum) — stop rather than spin.
            }
            last_cols = cols;
            if self
                .resize_sidebar_step(session, pane_id, "decrease")
                .is_err()
            {
                return;
            }
        }
    }

    fn resize_sidebar_step(&self, session: &str, pane_id: &str, direction: &str) -> Result<()> {
        self.zellij_action(session)
            .args([
                "resize".to_owned(),
                direction.to_owned(),
                "right".to_owned(),
                "--pane-id".to_owned(),
                pane_id.to_owned(),
            ])
            .run()
            .map(|_| ())
    }

    /// Current column width of `target_raw` and the total columns of its tab —
    /// the *extents* (`max(pane_x + pane_columns)`), not the sum, which would
    /// double-count vertically stacked panes and inflate the resize target.
    /// `None` when the pane has vanished or carries no geometry.
    fn sidebar_and_tab_cols(
        &self,
        session: &str,
        tab_id: u64,
        target_raw: u64,
    ) -> Option<(u64, u64)> {
        let panes = self.list_panes_with_session(Some(session)).ok()?;
        let current = panes
            .iter()
            .find(|pane| pane.is_terminal() && pane.tab_id == tab_id && pane.id == target_raw)
            .and_then(|pane| pane.pane_columns)?;
        Some((current, tab_extent_cols(&panes, tab_id)))
    }

    /// Block until Zellij has materialized the layout's sidebar pane alongside a
    /// second live terminal, so the caller's temp layout file stays on disk
    /// until Zellij has demonstrably parsed it. Returns `true` once that signal
    /// appears, `false` if the [`SIDEBAR_LAYOUT_TIMEOUT`] ceiling elapses first.
    ///
    /// The predicate gates on *our* `rimz-sidebar` pane (a default/fallback
    /// birth carries none) counted with the same `is_live_terminal` filter
    /// `list_panes` applies, so "materialized" here provably implies the
    /// caller's next `list_panes` returns the two panes — no held/exited pane
    /// slips the gate.
    fn wait_for_sidebar_layout(&self, session_name: &str) -> bool {
        let deadline = Instant::now() + SIDEBAR_LAYOUT_TIMEOUT;
        while Instant::now() < deadline {
            if let Ok(panes) = self.list_panes_with_session(Some(session_name))
                && panes.iter().any(is_sidebar_pane)
                && panes.iter().filter(|pane| pane.is_live_terminal()).count() >= 2
            {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// The session's tab names, in tab order. `query-tab-names` prints one name
    /// per line; the ANSI banner newer Zellij ships is stripped.
    fn tab_names(&self, session: &str) -> Result<Vec<String>> {
        for attempt in 0..TAB_NAMES_ATTEMPTS {
            if attempt > 0 {
                std::thread::sleep(TAB_NAMES_RETRY_DELAY);
            }
            let output = self.zellij_action(session).arg("query-tab-names").run()?;
            let names: Vec<String> = String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(strip_ansi)
                .map(|line| line.trim().to_owned())
                .filter(|line| !line.is_empty())
                .collect();
            if !names.is_empty() {
                return Ok(names);
            }
        }
        Err(MuxErr::Output {
            program: "zellij".to_owned(),
            reason: format!("query-tab-names returned no tabs after {TAB_NAMES_ATTEMPTS} attempts"),
        })
    }

    /// Whether `session` already holds a tab named `tab_name`. A Rimz background
    /// view is idempotent on its name, so a relaunch into a session that already
    /// carries it is skipped.
    fn session_has_named_tab(&self, session: &str, tab_name: &str) -> Result<bool> {
        Ok(self.tab_names(session)?.iter().any(|name| name == tab_name))
    }

    /// The fixed sidebar width carried by Zellij's `new_tab_template`.
    /// `rimz tab --layout` supplies its own layout and therefore bypasses that
    /// template, so it mirrors the template's width explicitly when Zellij can
    /// report it. A failure falls back to this command's birth verdict.
    fn new_tab_template_sidebar_cols(&self, session: &str) -> Result<Option<NonZeroU16>> {
        let output = self.zellij_action(session).arg("dump-layout").run()?;
        let layout = String::from_utf8_lossy(&output.stdout);
        Ok(new_tab_template_sidebar_cols(&layout))
    }
}

impl MuxBackend for ZellijBackend {
    fn name(&self) -> MuxName {
        MuxName::Zellij
    }

    fn ensure_session(&self, _opts: &SessionOptions) -> Result<()> {
        // Zellij creates sessions lazily, and `open_sidebar` owns first birth
        // by rendering the session from a layout (Zellij applies a layout only
        // at session creation). There is nothing to pre-create here.
        Ok(())
    }

    fn attach_command(&self, name: &str, config: &crate::config::MultiplexerConfig) -> CommandSpec {
        self.cmd()
            .args([
                "attach".to_owned(),
                "--create".to_owned(),
                name.to_owned(),
                "options".to_owned(),
            ])
            .args(self.zellij_options_args_probed(&config.zellij))
    }

    fn detach(&self, _name: &str) -> Result<()> {
        self.cmd().args(["action", "detach"]).run().map(|_| ())
    }

    fn kill_session(&self, name: &str) -> Result<()> {
        self.delete_session(name)
    }

    fn list_sessions(&self) -> Result<Vec<String>> {
        let output = self.cmd().arg("list-sessions").run()?;
        // Output lines look like `name [Created Ns ago]` for live sessions, or
        // `name [Created Ns ago] (EXITED - attach to resurrect)` for stopped
        // sessions. `list_sessions` is the live-session set used by `rimz list`
        // and `rimz reload`, so filter resurrectable corpses out here.
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(live_session_name_from_line)
            .collect())
    }

    fn list_panes(&self, opts: PaneListOptions) -> Result<Vec<PaneRef>> {
        let timeout = opts.command_timeout.unwrap_or(super::COMMAND_TIMEOUT);
        let raws = self.list_panes_bounded(opts.session_name.as_deref(), timeout)?;
        let session_name = opts.session_name.unwrap_or_default();
        Ok(raws
            .into_iter()
            .filter(RawPane::is_live_terminal)
            .map(|mut p| PaneRef {
                pane_id: PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", p.id)),
                session_name: session_name.clone(),
                view_id: Some(format!("tab_{}", p.tab_id)),
                view_kind: Some(ViewKind::Tab),
                view_name: p.tab_name.take(),
                is_focused: p.is_focused,
                pane_pid: p.pid(),
                pane_process_start: p.process_start(),
                command: p.pane_ref_command(),
                cwd: p.reported_cwd().map(str::to_owned),
                // Zellij's `list-panes -j` exposes no per-pane "tab is active"
                // or "session attached" signal, so pane visibility is unknown
                // here. `None` makes the renderer's visibility gate fall back
                // to always painting — the deliberate cross-backend floor.
            })
            .collect())
    }

    fn focused_client_panes(&self, opts: ClientFocusOptions) -> Result<Vec<PaneId>> {
        let timeout = opts.command_timeout.unwrap_or(super::COMMAND_TIMEOUT);
        let mut spec = self.cmd();
        if let Some(name) = opts.session_name {
            spec = spec.args(["--session".to_owned(), name]);
        }
        let output = spec
            .args(["action", "list-clients"])
            .run_with_timeout(timeout)?;
        Ok(parse_focused_client_panes(&output.stdout))
    }

    fn split_pane(&self, opts: SplitPaneOptions) -> Result<()> {
        let mut spec = self.cmd().args(["action", "new-pane"]);
        if let Some(target) = opts.target_pane_id {
            ensure_pane_backend(&target, MuxName::Zellij)?;
            // Zellij's CLI opens relative to the current focus and does not
            // expose a target-pane flag for `new-pane`.
        }
        if let Some(cwd) = opts.cwd {
            spec = spec.args(["--cwd".to_owned(), cwd]);
        }
        if let Some(command) = opts.command
            && let Some((program, args)) = command.split_first()
        {
            spec = spec
                .args(["--".to_owned(), program.clone()])
                .args(args.iter().cloned());
        }
        spec.run().map(|_| ())
    }

    fn focus_pane(&self, pane: &PaneId) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Zellij)?;
        // Zellij 0.41+: `focus-pane-id <raw>`. The earlier `focus-pane-with-id`
        // name was removed; the stub that referenced it never reached a
        // running binary.
        self.cmd()
            .args(["action", "focus-pane-id", pane.raw()])
            .run()
            .map(|_| ())
    }

    fn capture_pane(&self, pane: &PaneId, lines: Option<u16>, ansi: bool) -> Result<PaneCapture> {
        ensure_pane_backend(pane, MuxName::Zellij)?;
        let mut spec = self.cmd().args(["action", "dump-screen"]);
        if ansi {
            spec = spec.arg("-a");
        }
        if lines.is_some() {
            // The `-f`/`--full` flag dumps the entire scrollback. Zellij does
            // not expose a "last N lines" cap at the CLI level, so any non-None
            // request maps onto "include scrollback"; the caller can post-trim.
            spec = spec.arg("-f");
        }
        spec = spec.args(["-p".to_owned(), pane.raw().to_owned()]);
        let output = spec.run()?;
        let raw_text = String::from_utf8_lossy(&output.stdout).into_owned();
        let (raw_text, lines) = trim_capture(raw_text, lines);
        Ok(PaneCapture {
            pane_id: pane.clone(),
            raw_text,
            lines,
        })
    }

    fn send_keys(&self, pane: &PaneId, text: &str) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Zellij)?;
        self.cmd()
            .args(["action", "write-chars", "--pane-id", pane.raw(), text])
            .run()
            .map(|_| ())
    }

    fn open_sidebar(&self, opts: &SidebarPaneOptions, daemon: Option<&DaemonView>) -> Result<()> {
        // Zellij places a left pane only at session birth, so the sidebar is
        // injected only by (re)creating the session from a layout. `daemon`, when
        // present, leads the birth layout (the only way a tab can lead, since
        // Zellij can't reorder tabs after birth):
        //   - Absent: first birth.
        //   - Exited: `attach` would resurrect a stale serialized layout (wrong
        //             geometry, suspended command panes), so delete and rebirth.
        //   - Live + sidebar: healthy only when the caller still trusts a
        //             fresh current-protocol heartbeat. If launch reached this
        //             method after rejecting the heartbeat, the pane may be a
        //             stale renderer with an incompatible snapshot schema.
        //   - Live, no sidebar: the renderer self-closed or crashed (or a launch
        //             was skipped and the session was born by a plain `attach
        //             --create`). A sidebar-less rimz session is non-functional
        //             and cannot gain a left pane in place, so rebirth it.
        match self.session_state(&opts.session_name) {
            SessionState::Absent => self.create_session_with_sidebar(opts, daemon),
            SessionState::Exited => {
                self.delete_session(&opts.session_name)?;
                self.create_session_with_sidebar(opts, daemon)
            }
            SessionState::Live => match self.session_cleanliness(&opts.session_name) {
                Ok(SessionCleanliness::Clean) if !opts.replace_existing => Ok(()),
                Ok(_) => {
                    self.delete_session(&opts.session_name)?;
                    self.create_session_with_sidebar(opts, daemon)
                }
                Err(err) => {
                    tracing::warn!(
                        session = %opts.session_name,
                        error = %err,
                        "live zellij room could not be inspected; leaving it untouched",
                    );
                    Err(err)
                }
            },
        }
    }

    fn probe_session_health(&self, name: &str) -> Result<SessionHealth> {
        Ok(match self.session_state(name) {
            // Nothing to attach to — a fresh birth will produce a clean room.
            SessionState::Absent => SessionHealth::Healthy,
            // `attach --create` would resurrect a serialized, suspended layout.
            SessionState::Exited => SessionHealth::Stuck,
            SessionState::Live => match self.session_cleanliness(name) {
                Ok(SessionCleanliness::Clean) => SessionHealth::Healthy,
                Ok(
                    SessionCleanliness::MissingSidebar | SessionCleanliness::SuspendedCommandPane,
                )
                | Err(_) => SessionHealth::Stuck,
            },
        })
    }

    fn ensure_clean_session(
        &self,
        opts: &SidebarPaneOptions,
        daemon: Option<&DaemonView>,
    ) -> Result<SessionHealth> {
        let state = self.session_state(&opts.session_name);
        // A clean, live room is left untouched — never rebirth working panes.
        if matches!(state, SessionState::Live) {
            match self.session_cleanliness(&opts.session_name) {
                Ok(SessionCleanliness::Clean) => return Ok(SessionHealth::Healthy),
                Ok(
                    SessionCleanliness::MissingSidebar | SessionCleanliness::SuspendedCommandPane,
                ) => {}
                Err(err) => {
                    tracing::warn!(
                        session = %opts.session_name,
                        error = %err,
                        "live zellij room could not be inspected; reset confirmation is required",
                    );
                    return Ok(SessionHealth::Stuck);
                }
            }
        }
        // Absent → first birth; Exited / inspected Live-but-suspended → delete
        // and rebirth from the layout so the room comes up clean and RUNNING
        // (with serialization off, a rebirth can never resurrect). An
        // uninspectable live room returns Stuck above so the caller offers a
        // reset before any destructive action. A rebirth that still fails to
        // talk to Zellij reads as Stuck so the caller offers a reset.
        let rebirth = || -> Result<()> {
            if !matches!(state, SessionState::Absent) {
                self.delete_session(&opts.session_name)?;
            }
            self.create_session_with_sidebar(opts, daemon)
        };
        match rebirth() {
            Ok(()) => Ok(SessionHealth::Reborn),
            Err(err) => {
                tracing::warn!(
                    session = %opts.session_name,
                    error = %err,
                    "session rebirth failed; a destructive reset is required",
                );
                Ok(SessionHealth::Stuck)
            }
        }
    }

    fn purge_resurrection_cache(&self, name: &str) -> Vec<PathBuf> {
        // `delete-session --force` already drops the serialized session, but a
        // crashed server can leave the cache behind with no live session to
        // delete, so reset removes it directly as well.
        super::recovery::purge_zellij_session_cache_in(&crate::ledger::paths::cache_home(), name)
    }

    fn reconcile_sidebars(
        &self,
        opts: &SidebarPaneOptions,
        live: &SidebarLiveness,
    ) -> Result<SidebarRecovery> {
        // Zellij docks the sidebar left only at session birth, but a left pane
        // can still be reached in a live session: close a stray sidebar by id,
        // and add one by splitting right, moving it left, and resizing it to the
        // layout width. This never rebirths the session, so the working panes
        // survive.
        let panes = self.list_panes_with_session(Some(&opts.session_name))?;
        let views = views_with_sidebars(&panes);
        let plan = super::plan_reconcile(&views, live);
        // Kept sidebars (not planned for closing) whose geometry sits off the
        // layout's dock — the residue of a mis-mounted add — converge in place
        // this pass, renderer untouched.
        let closing: std::collections::HashSet<&PaneId> = plan.close.iter().collect();
        let off_spec: Vec<(u64, u64)> = panes
            .iter()
            .filter(|pane| pane.is_live_terminal() && is_sidebar_pane(pane))
            .filter(|pane| {
                let id = PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", pane.id));
                !closing.contains(&id)
            })
            .filter(|pane| sidebar_geometry_off_spec(pane, &panes, opts.width))
            .map(|pane| (pane.tab_id, pane.id))
            .collect();
        if plan.close.is_empty() && plan.add.is_empty() && off_spec.is_empty() {
            return Ok(SidebarRecovery::default());
        }

        // Adding (and closing) a pane shifts focus, so remember each tab's
        // focused (working) pane to restore afterwards, and the user's own
        // invoking pane to return the visible tab to where they ran `rimz reload`.
        let focused_in_tab: std::collections::HashMap<u64, u64> = panes
            .iter()
            .filter(|pane| pane.is_focused && !pane.is_plugin)
            .map(|pane| (pane.tab_id, pane.id))
            .collect();

        let mut report = SidebarRecovery::default();
        // Close duplicate / unresponsive sidebar panes first, so a view that lost
        // its only live sidebar reads as missing and gains exactly one fresh one.
        for pane in &plan.close {
            match self.close_pane(&opts.session_name, pane) {
                Ok(()) => report.closed += 1,
                Err(err) => tracing::warn!(
                    session = %opts.session_name,
                    pane = %pane.as_str(),
                    error = %err,
                    "sidebar reconcile: closing a stray sidebar pane failed; leaving it",
                ),
            }
        }
        // In-place adds and geometry moves both need an attached client: a
        // detached session's screen thread drops the mount while the spawned
        // serve pair keeps running, so adding there only leaks (the closes
        // above are safe detached). An unanswerable probe reads detached —
        // deferring one run is recoverable, a leaked pair is not. tmux splits
        // fine detached, so the gate is Zellij-internal.
        let attached = (!plan.add.is_empty() || !off_spec.is_empty())
            && self.session_has_attached_client(&opts.session_name);
        if attached {
            for (tab_id, raw_id) in &off_spec {
                if self.converge_sidebar_geometry(opts, *tab_id, *raw_id) {
                    report.redocked += 1;
                }
            }
        }
        if !plan.add.is_empty() && !attached {
            report.deferred = plan.add.len();
            tracing::info!(
                session = %opts.session_name,
                deferred = report.deferred,
                "sidebar reconcile: no attached client; deferring in-place adds",
            );
        } else {
            let mut tabs_with_sidebar = if plan.add.is_empty() {
                Some(std::collections::HashSet::new())
            } else {
                match self.list_panes_with_session(Some(&opts.session_name)) {
                    Ok(panes) => Some(tabs_with_sidebars(&panes)),
                    Err(err) => {
                        tracing::warn!(
                            session = %opts.session_name,
                            error = %err,
                            "sidebar reconcile: cannot verify sidebar absence before add; skipping adds",
                        );
                        None
                    }
                }
            };
            for tab in &plan.add {
                let Ok(tab_id) = tab.parse::<u64>() else {
                    report.failed += 1;
                    continue;
                };
                let Some(occupied_tabs) = tabs_with_sidebar.as_mut() else {
                    report.failed += 1;
                    continue;
                };
                if occupied_tabs.contains(tab) {
                    tracing::warn!(
                        session = %opts.session_name,
                        tab = tab_id,
                        "sidebar reconcile: add skipped because the tab still has a sidebar",
                    );
                    report.failed += 1;
                    continue;
                }
                match self.add_sidebar_to_tab(opts, tab_id) {
                    Ok(()) => {
                        report.recovered += 1;
                        occupied_tabs.insert(tab.clone());
                        if let Some(work) = focused_in_tab.get(&tab_id) {
                            let _ = self.focus_terminal(&opts.session_name, *work);
                        }
                    }
                    Err(err) => {
                        tracing::warn!(
                            session = %opts.session_name,
                            tab = tab_id,
                            error = %err,
                            "sidebar reconcile: in-place add failed; leaving the tab without a sidebar",
                        );
                        report.failed += 1;
                    }
                }
            }
        }
        if let Some(own) = own_zellij_pane_id() {
            let _ = self.focus_terminal(&opts.session_name, own);
        }
        Ok(report)
    }

    fn open_background_view(&self, opts: &BackgroundViewOptions) -> Result<BackgroundViewLaunch> {
        let session = &opts.sidebar.session_name;
        // Idempotent on the tab name. The lead position is owned by session birth
        // ([`Self::open_sidebar`] with a `daemon`): `rimz start` births the session
        // with this tab already leading, so the common case is a no-op here. A
        // failed query propagates rather than risk a duplicate launch.
        if self.session_has_named_tab(session, &opts.name)? {
            return Ok(BackgroundViewLaunch::AlreadyRunning);
        }
        // Late add: the session was born without the daemon tab (e.g. a host
        // became available after first start) and now carries one or more working
        // tabs. Zellij can't move a tab to the front, so this appended tab does
        // *not* lead — leading is a birth-time property. `--layout` gives the tab
        // its `sidebar | hosts…` shape directly (bypassing the tab template, so the
        // sidebar is spelled out); `new-tab` is synchronous (it prints the tab id),
        // so the temp layout can drop once it returns. Each pane carries its own
        // `cwd`, so no tab-level `--cwd` is needed.
        let layout = TempLayoutFile::new(render_background_view_layout(opts)?)?;
        self.zellij_action(session)
            .args([
                "new-tab".to_owned(),
                "--layout".to_owned(),
                layout.path().to_string_lossy().into_owned(),
                "--name".to_owned(),
                opts.name.clone(),
            ])
            .run()?;
        drop(layout);
        // `new-tab` focuses the tab it creates. Return focus to the leading tab so
        // the imminent `attach` lands on a working pane, not this freshly-added
        // daemon tab. Best-effort: a focus hiccup never sinks a launch.
        if let Err(err) = self.go_to_tab(session, 1) {
            tracing::warn!(
                session = %session,
                error = %err,
                "could not return focus off the freshly-added daemon tab",
            );
        }
        Ok(BackgroundViewLaunch::Launched)
    }

    fn open_tab(&self, opts: &TabOptions) -> Result<()> {
        let template_sidebar_cols = self
            .new_tab_template_sidebar_cols(&opts.session_name)
            .ok()
            .flatten();
        let layout = TempLayoutFile::new(render_tab_layout(opts, template_sidebar_cols)?)?;
        self.zellij_action(&opts.session_name)
            .args([
                "new-tab".to_owned(),
                "--layout".to_owned(),
                layout.path().to_string_lossy().into_owned(),
                "--name".to_owned(),
                opts.title.clone(),
            ])
            .run()?;
        drop(layout);
        if !opts.focus
            && let Err(err) = self.go_to_tab(&opts.session_name, 1)
        {
            tracing::warn!(
                session = %opts.session_name,
                error = %err,
                "could not return focus after opening an unfocused tab",
            );
        }
        Ok(())
    }

    fn wake_sidebar(&self, session_name: &str, bytes: &[u8]) -> Result<()> {
        // Per-instance socket fanout is the channel of record. The broadcast
        // `zellij pipe` here is a latency optimization on top. Program (and any
        // `RIMZ_ZELLIJ_BIN` test-shim override) and runtime dir both come from
        // `self.cmd()`, the single chokepoint.
        let payload = String::from_utf8_lossy(bytes).to_string();
        self.cmd()
            .args([
                "--session",
                session_name,
                "pipe",
                "--name",
                "rimz::feed",
                "--",
                &payload,
            ])
            .run()
            .map(|_| ())
    }

    fn ensure_presence_plugin(&self, opts: &super::PresencePluginOptions) -> Result<()> {
        let parsed = self.version().ok().as_deref().and_then(parse_version);
        if parsed.is_none_or(|v| v < PRESENCE_PLUGIN_MIN_ZELLIJ) {
            tracing::debug!(
                session = %opts.session_name,
                version = ?parsed,
                "zellij below the presence-plugin floor; the producer keeps its pane poll",
            );
            return Ok(());
        }
        let url = format!("file:{}", opts.wasm.display());
        let configuration = presence_plugin_configuration(opts);
        if opts.converge {
            // Reload a *running* plugin in place onto the current wasm —
            // `start-or-reload-plugin` converges a pipe-launched instance
            // (verified on 0.44.3: one instance throughout). It needs a
            // connected client; with none the server drops it silently (exit
            // 0 regardless), and the pipe below still ensures a plugin is
            // loaded — the upgrade then lands on the next attached reload.
            self.zellij_action(&opts.session_name)
                .args([
                    "start-or-reload-plugin".to_owned(),
                    url.clone(),
                    "--configuration".to_owned(),
                    configuration.clone(),
                ])
                .run()?;
        }
        // `zellij pipe --plugin` launches the plugin if absent — the one load
        // verb that works on a clientless session (`start-or-reload-plugin`
        // refuses without a connected client) — and routes a no-op message to
        // it when running, so the call is idempotent per (url, configuration).
        let result = self
            .cmd()
            .args([
                "--session",
                &opts.session_name,
                "pipe",
                "--plugin",
                &url,
                "--plugin-configuration",
                &configuration,
                "--name",
                PRESENCE_BOOT_PIPE,
                "--",
                "load",
            ])
            .run_with_timeout(PRESENCE_PIPE_TIMEOUT);
        match result {
            Ok(_) => Ok(()),
            // The held-CLI kill: the launch is delivered, the plugin is
            // waiting on the user's one-time permission answer (or the
            // session has no client yet to surface it). Expected, not an
            // error — pokes start once the grant lands.
            Err(MuxErr::Timeout { .. }) => {
                tracing::debug!(
                    session = %opts.session_name,
                    "presence boot pipe held past its deadline (permission pending); launch delivered",
                );
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    fn version(&self) -> Result<String> {
        if let Some(cached) = self.version.get() {
            return Ok(cached.clone());
        }
        let spec = self.cmd().arg("--version");
        let output = spec.to_command().output().map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => MuxErr::NotInstalled {
                program: spec.program.clone(),
            },
            _ => MuxErr::Io(err),
        })?;
        let raw = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        // First writer wins on a probe race; both raced probes read one binary.
        Ok(self.version.get_or_init(|| raw).clone())
    }
}

/// Whether `list-panes` stdout is the transient empty race rather than a real
/// answer. Zellij spells "zero panes" as `[]`, so empty (or whitespace-only)
/// output means the action client raced the session server and is worth a
/// retry — not an EOF parse error.
fn is_transient_empty(stdout: &[u8]) -> bool {
    stdout.iter().all(u8::is_ascii_whitespace)
}

/// Liveness of a Zellij session, as reported by `zellij list-sessions`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionState {
    /// No session by that name.
    Absent,
    /// Running and attachable.
    Live,
    /// Present but exited — `attach` would resurrect a stale serialized layout.
    Exited,
}

/// Cleanliness of a live room after a successful pane inspection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionCleanliness {
    /// Sidebar and command panes are running.
    Clean,
    /// The sidebar is absent or held at a "Waiting to run" prompt.
    MissingSidebar,
    /// At least one non-sidebar command pane is held at a "Waiting to run" prompt.
    SuspendedCommandPane,
}

/// Parse one `list-sessions` line for `name`. Lines look like
/// `name [Created 6m ago]` (live) or
/// `name [Created 6m ago] (EXITED - attach to resurrect)`. `strip_ansi` guards
/// against a colorized line even though `--no-formatting` should preclude one.
fn session_state_from_line(line: &str, name: &str) -> Option<SessionState> {
    let clean = strip_ansi(line);
    if clean.split_whitespace().next()? != name {
        return None;
    }
    Some(if clean.contains("EXITED") {
        SessionState::Exited
    } else {
        SessionState::Live
    })
}

fn live_session_name_from_line(line: &str) -> Option<String> {
    let clean = strip_ansi(line);
    let name = clean.split_whitespace().next()?;
    matches!(
        session_state_from_line(&clean, name),
        Some(SessionState::Live)
    )
    .then(|| name.to_owned())
}

fn new_tab_template_sidebar_cols(layout: &str) -> Option<NonZeroU16> {
    let tokens = tokenize_zellij_layout(layout)?;
    let template_open = tokens
        .iter()
        .enumerate()
        .find_map(|(index, token)| match &token.kind {
            KdlTokenKind::Ident(name) if name == "new_tab_template" => tokens[index + 1..]
                .iter()
                .position(|token| matches!(token.kind, KdlTokenKind::LBrace))
                .map(|offset| index + 1 + offset),
            _ => None,
        })?;
    let template_close = matching_brace(&tokens, template_open)?;
    sidebar_cols_from_template_tokens(&tokens[template_open + 1..template_close])
}

fn sidebar_cols_from_template_tokens(tokens: &[KdlToken]) -> Option<NonZeroU16> {
    for (index, token) in tokens.iter().enumerate() {
        let KdlTokenKind::Ident(node) = &token.kind else {
            continue;
        };
        if node != "pane" {
            continue;
        }

        let line = token.line;
        let mut name_is_sidebar = false;
        let mut size = None;
        let mut cursor = index + 1;
        while let Some(token) = tokens.get(cursor) {
            if token.line != line
                || matches!(token.kind, KdlTokenKind::LBrace | KdlTokenKind::RBrace)
            {
                break;
            }
            let Some((key, value)) = kdl_property_on_line(tokens, cursor, line) else {
                cursor += 1;
                continue;
            };
            match (key, value) {
                ("name", KdlTokenKind::String(value)) if value == SIDEBAR_PANE_NAME => {
                    name_is_sidebar = true;
                }
                ("size", KdlTokenKind::Number(value)) => {
                    size = u16::try_from(*value).ok().and_then(NonZeroU16::new);
                }
                _ => {}
            }
            cursor += 3;
        }
        if name_is_sidebar && let Some(size) = size {
            return Some(size);
        }
    }
    None
}

fn kdl_property_on_line(
    tokens: &[KdlToken],
    index: usize,
    line: usize,
) -> Option<(&str, &KdlTokenKind)> {
    let key = match &tokens.get(index)?.kind {
        KdlTokenKind::Ident(key) => key.as_str(),
        _ => return None,
    };
    let equals = tokens.get(index + 1)?;
    let value = tokens.get(index + 2)?;
    (equals.line == line && value.line == line && matches!(equals.kind, KdlTokenKind::Equals))
        .then_some((key, &value.kind))
}

fn matching_brace(tokens: &[KdlToken], open: usize) -> Option<usize> {
    if !matches!(tokens.get(open)?.kind, KdlTokenKind::LBrace) {
        return None;
    }
    let mut depth = 0_u32;
    for (index, token) in tokens.iter().enumerate().skip(open) {
        match token.kind {
            KdlTokenKind::LBrace => depth += 1,
            KdlTokenKind::RBrace => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

#[derive(Debug, PartialEq, Eq)]
struct KdlToken {
    kind: KdlTokenKind,
    line: usize,
}

#[derive(Debug, PartialEq, Eq)]
enum KdlTokenKind {
    Ident(String),
    String(String),
    Number(i64),
    Equals,
    LBrace,
    RBrace,
}

/// Tokenize the subset of Zellij-generated KDL needed from `dump-layout`.
/// Strings and braces are honored so the template walk never matches text
/// inside argv, cwd, or command values; unsupported shapes degrade to `None`.
fn tokenize_zellij_layout(input: &str) -> Option<Vec<KdlToken>> {
    let mut tokens = Vec::new();
    let mut chars = input.char_indices().peekable();
    let mut line = 1;
    while let Some((_, ch)) = chars.peek().copied() {
        match ch {
            '\n' => {
                line += 1;
                chars.next();
            }
            ch if ch.is_whitespace() => {
                chars.next();
            }
            '/' => {
                chars.next();
                if chars.peek().is_some_and(|(_, next)| *next == '/') {
                    for (_, skipped) in chars.by_ref() {
                        if skipped == '\n' {
                            line += 1;
                            break;
                        }
                    }
                } else {
                    tokens.push(KdlToken {
                        kind: KdlTokenKind::Ident("/".to_owned()),
                        line,
                    });
                }
            }
            '{' => {
                tokens.push(KdlToken {
                    kind: KdlTokenKind::LBrace,
                    line,
                });
                chars.next();
            }
            '}' => {
                tokens.push(KdlToken {
                    kind: KdlTokenKind::RBrace,
                    line,
                });
                chars.next();
            }
            '=' => {
                tokens.push(KdlToken {
                    kind: KdlTokenKind::Equals,
                    line,
                });
                chars.next();
            }
            '"' => {
                tokens.push(KdlToken {
                    kind: KdlTokenKind::String(read_kdl_string(&mut chars, &mut line)?),
                    line,
                });
            }
            '-' | '+' | '0'..='9' => {
                tokens.push(KdlToken {
                    kind: read_kdl_number_or_ident(&mut chars),
                    line,
                });
            }
            _ => {
                tokens.push(KdlToken {
                    kind: KdlTokenKind::Ident(read_kdl_ident(&mut chars)),
                    line,
                });
            }
        }
    }
    Some(tokens)
}

fn read_kdl_string(
    chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
    line: &mut usize,
) -> Option<String> {
    let (_, quote) = chars.next()?;
    if quote != '"' {
        return None;
    }
    let mut value = String::new();
    while let Some((_, ch)) = chars.next() {
        match ch {
            '"' => return Some(value),
            '\\' => {
                if let Some((_, escaped)) = chars.next() {
                    if escaped == '\n' {
                        *line += 1;
                    }
                    value.push(escaped);
                } else {
                    return None;
                }
            }
            '\n' => {
                *line += 1;
                value.push(ch);
            }
            _ => value.push(ch),
        }
    }
    None
}

fn read_kdl_number_or_ident(
    chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
) -> KdlTokenKind {
    let raw = read_kdl_ident(chars);
    raw.parse::<i64>()
        .map(KdlTokenKind::Number)
        .unwrap_or(KdlTokenKind::Ident(raw))
}

fn read_kdl_ident(chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>) -> String {
    let mut value = String::new();
    while let Some((_, ch)) = chars.peek().copied() {
        if ch.is_whitespace() || matches!(ch, '{' | '}' | '=' | '"') {
            break;
        }
        value.push(ch);
        chars.next();
    }
    value
}

/// A live, non-plugin sidebar pane is one Zellij still titles with the layout's
/// [`SIDEBAR_PANE_NAME`] — the same signal `classify_session_panes` trusts.
fn is_sidebar_pane(pane: &RawPane) -> bool {
    !pane.is_plugin && pane.title.as_deref() == Some(SIDEBAR_PANE_NAME)
}

/// Group a pane list into per-tab [`ViewSidebars`] for the reconcile planner:
/// each tab's sidebar panes (as normalized [`PaneId`]s) and whether it holds a
/// user-working terminal pane. Managed daemon hosts in `rimzd` are not work.
/// First-seen tab order; pane order within a tab preserved.
fn views_with_sidebars(panes: &[RawPane]) -> Vec<ViewSidebars> {
    let mut views: Vec<ViewSidebars> = Vec::new();
    let mut index: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    for pane in panes.iter().filter(|pane| pane.is_terminal()) {
        let slot = *index.entry(pane.tab_id).or_insert_with(|| {
            views.push(ViewSidebars {
                view: pane.tab_id.to_string(),
                sidebar_panes: Vec::new(),
                has_working: false,
                has_daemon_host: false,
            });
            views.len() - 1
        });
        if is_sidebar_pane(pane) {
            views[slot].sidebar_panes.push(PaneId::from_parts(
                MuxName::Zellij,
                format!("terminal_{}", pane.id),
            ));
        } else if is_daemon_host_pane(pane) {
            views[slot].has_daemon_host = true;
        } else {
            views[slot].has_working = true;
        }
    }
    views
}

fn tabs_with_sidebars(panes: &[RawPane]) -> std::collections::HashSet<String> {
    views_with_sidebars(panes)
        .into_iter()
        .filter(|view| !view.sidebar_panes.is_empty())
        .map(|view| view.view)
        .collect()
}

/// A managed daemon-host pane: any pane in the `rimzd` tab, or one whose
/// command carries a host marker. The tab name is the signal that works on
/// Zellij 0.44 — its pane list emits no live command fields, and the
/// `reported_command` ladder bottoms out at `terminal_command`, the spawn
/// command, so a host that re-execs (the Claude remote-control host does)
/// keeps its marker only there.
fn is_daemon_host_pane(pane: &RawPane) -> bool {
    pane.tab_name.as_deref() == Some(crate::remote_control::VIEW_NAME)
        || pane
            .reported_command()
            .is_some_and(crate::remote_control::command_is_host)
}

fn classify_session_panes(panes: &[RawPane]) -> SessionCleanliness {
    if !has_healthy_sidebar(panes) {
        return SessionCleanliness::MissingSidebar;
    }
    if has_suspended_command_pane(panes) {
        return SessionCleanliness::SuspendedCommandPane;
    }
    SessionCleanliness::Clean
}

fn has_healthy_sidebar(panes: &[RawPane]) -> bool {
    let mut found = false;
    for pane in panes.iter().filter(|pane| is_sidebar_pane(pane)) {
        found = true;
        if pane.is_held {
            return false;
        }
    }
    found
}

/// Any non-sidebar terminal pane Zellij is holding at a "Waiting to run" prompt —
/// the fingerprint of a resurrected (serialized) room, where every command pane
/// comes back `start_suspended`. A clean rebirth has none: every command runs.
fn has_suspended_command_pane(panes: &[RawPane]) -> bool {
    panes
        .iter()
        .any(|pane| pane.is_terminal() && !is_sidebar_pane(pane) && pane.is_held)
}

/// `ZELLIJ_PANE_ID` is the bare integer of the pane the caller runs in. `rimz
/// reload` runs in the user's pane, so refocusing it restores their visible tab.
fn own_zellij_pane_id() -> Option<u64> {
    env::var("ZELLIJ_PANE_ID").ok()?.trim().parse().ok()
}

fn parse_terminal_id(pane_id: &str) -> Option<u64> {
    pane_id.strip_prefix("terminal_")?.parse().ok()
}

/// Strict parse of `new-pane` stdout into a pane-id hint: exactly
/// `terminal_<digits>` after trimming, else `None`. Concurrent `zellij action`
/// clients can receive each other's responses, so anything looser would take
/// another command's output (an empty body, a JSON blob) for a pane id.
fn parse_new_pane_id(stdout: &str) -> Option<String> {
    let trimmed = stdout.trim();
    parse_terminal_id(trimmed)?;
    Some(trimmed.to_owned())
}

/// A tab's total width in columns: the extents (`max(pane_x + pane_columns)`)
/// over its terminal panes. A missing `pane_x` reads as `0`, degrading toward
/// the widest single pane rather than the stacked-pane-inflated sum.
fn tab_extent_cols(panes: &[RawPane], tab_id: u64) -> u64 {
    panes
        .iter()
        .filter(|pane| pane.is_terminal() && pane.tab_id == tab_id)
        .filter_map(|pane| Some(pane.pane_x.unwrap_or(0) + pane.pane_columns?))
        .max()
        .unwrap_or(0)
}

/// Whether a sidebar `cols` wide in a `total`-wide tab is past the resize
/// trigger: over [`SIDEBAR_RESIZE_TRIGGER_PERCENT`] of the tab *and* over the
/// `max_cols` cap — a pane born fixed at the cap is a deliberate width verdict
/// on any client, never a mis-mount.
fn sidebar_width_off_spec(cols: u64, total: u64, width: SidebarWidth) -> bool {
    total > 0 && cols * 100 > total * SIDEBAR_RESIZE_TRIGGER_PERCENT && cols > width.cap_cols()
}

/// Whether a kept sidebar pane sits off the layout's dock: not in the left
/// column, or past the width trigger ([`sidebar_width_off_spec`]). The
/// mis-mounted shape (right side, ~50%) trips both; a healthy layout-born pane
/// (left, percentage at the cap) trips neither. Unknown geometry never reads
/// off-spec.
fn sidebar_geometry_off_spec(pane: &RawPane, panes: &[RawPane], width: SidebarWidth) -> bool {
    if pane.pane_x.is_some_and(|x| x != 0) {
        return true;
    }
    pane.pane_columns.is_some_and(|cols| {
        sidebar_width_off_spec(cols, tab_extent_cols(panes, pane.tab_id), width)
    })
}

/// The mounted sidebar pane an add produced: a live, sidebar-titled terminal
/// pane in `tab_id` matching the stdout hint, or — the hint being unreliable —
/// the lowest such id absent from the before-set, so a cross-talk duplicate
/// resolves deterministically and reconcile closes the rest.
fn mounted_sidebar_pane(
    panes: &[RawPane],
    tab_id: u64,
    before: &std::collections::HashSet<u64>,
    hint: Option<u64>,
) -> Option<u64> {
    let ids: Vec<u64> = panes
        .iter()
        .filter(|pane| pane.tab_id == tab_id && pane.is_live_terminal() && is_sidebar_pane(pane))
        .map(|pane| pane.id)
        .collect();
    if let Some(raw) = hint
        && ids.contains(&raw)
    {
        return Some(raw);
    }
    ids.into_iter().filter(|id| !before.contains(id)).min()
}

/// Subset of fields `zellij action list-panes -j -a` emits. We deserialize
/// only what we route into `PaneRef`; serde silently ignores everything else.
#[derive(Debug, Deserialize)]
struct RawPane {
    id: u64,
    is_plugin: bool,
    #[serde(default)]
    is_held: bool,
    /// Command has exited but Zellij still shows the pane (e.g. hold-on-close).
    /// A dead pane, not a live process — excluded from the pane listing.
    #[serde(default)]
    exited: bool,
    #[serde(default)]
    is_suppressed: bool,
    #[serde(default)]
    is_focused: bool,
    tab_id: u64,
    /// Name of the tab the pane lives in. Routed into [`PaneRef::view_name`];
    /// also how the `rimzd` daemon view is recognised on Zellij, whose pane list
    /// reports no command fields a classifier could read instead.
    #[serde(default)]
    tab_name: Option<String>,
    /// Column width of the pane, used by in-place sidebar recovery to resize a
    /// freshly-split sidebar toward the layout's width percentage.
    #[serde(default)]
    pane_columns: Option<u64>,
    /// Column offset of the pane's left edge — `0` is the left column. Drives
    /// the tab-width extents math and the off-spec redock in sidebar recovery.
    #[serde(default)]
    pane_x: Option<u64>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    terminal_command: Option<String>,
    #[serde(default)]
    pane_command: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    pane_cwd: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    pane_pid: Option<u32>,
    #[serde(default)]
    pid: Option<u32>,
    #[serde(default)]
    pane_process_start: Option<Value>,
    #[serde(default)]
    process_start: Option<Value>,
}

impl RawPane {
    /// A real terminal pane: not a plugin and not suppressed (floating/hidden).
    /// The single definition of "counts as a pane" shared by the pane listing,
    /// sidebar recovery, and column math.
    fn is_terminal(&self) -> bool {
        !self.is_plugin && !self.is_suppressed
    }

    /// A terminal pane hosting a live command. Excludes held/exited corpses so a
    /// dead command never renders a row. Zellij can omit command fields for a
    /// live implicit shell pane, so the projection preserves that as `None`; the
    /// producer's frame rotation repairs raced-null fields from the last good
    /// observation when one exists.
    fn is_live_terminal(&self) -> bool {
        self.is_terminal() && !self.is_held && !self.exited
    }

    /// The command the pane reports, by the field ladder Zellij has emitted
    /// across versions: `pane_command` (the foreground program) wins, falling
    /// back through the older `command` to the newer full `terminal_command`.
    /// A present-but-empty field falls through rather than masking a later
    /// one. The one ladder shared by the `PaneRef` projection and the raw-pane
    /// classifiers, so the two layers always agree on a pane.
    fn reported_command(&self) -> Option<&str> {
        [
            self.pane_command.as_deref(),
            self.command.as_deref(),
            self.terminal_command.as_deref(),
        ]
        .into_iter()
        .flatten()
        .find(|value| !value.is_empty())
    }

    /// The command the pane's `PaneRef` carries. A title-identified sidebar
    /// wins: Zellij can omit command fields for the layout pane, and it must
    /// still be filtered as chrome rather than rendered as an anonymous
    /// process row. Otherwise the reported-command ladder decides.
    fn pane_ref_command(&self) -> Option<String> {
        if is_sidebar_pane(self) {
            return Some(SIDEBAR_PANE_NAME.to_owned());
        }
        self.reported_command().map(str::to_owned)
    }

    /// The cwd the pane reports; `pane_cwd` wins, falling back to `cwd`, with
    /// a present-but-empty field falling through like the command ladder.
    fn reported_cwd(&self) -> Option<&str> {
        [self.pane_cwd.as_deref(), self.cwd.as_deref()]
            .into_iter()
            .flatten()
            .find(|value| !value.is_empty())
    }

    fn pid(&self) -> Option<u32> {
        self.pane_pid.or(self.pid)
    }

    fn process_start(&self) -> Option<Timestamp> {
        self.pane_process_start
            .as_ref()
            .or(self.process_start.as_ref())
            .and_then(timestamp_from_json)
    }
}

fn timestamp_from_json(value: &Value) -> Option<Timestamp> {
    if let Some(seconds) = value.as_i64() {
        return Timestamp::from_second(seconds).ok();
    }
    if let Some(raw) = value.as_str() {
        if let Ok(seconds) = raw.parse::<i64>() {
            return Timestamp::from_second(seconds).ok();
        }
        return raw.parse::<Timestamp>().ok();
    }
    None
}

/// Defensive ANSI strip for `list-sessions` output. Zellij ships a colored
/// banner in newer versions; the parser only cares about the bare name.
///
/// Handles the CSI subset (`ESC [ params final`) Zellij emits. The
/// introducer `[` lives at 0x5b which overlaps the final-byte range
/// (0x40..=0x7e), so we must consume the introducer first and only then
/// scan for the final byte.
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if let Some('[') = chars.next() {
                for ch in chars.by_ref() {
                    if matches!(ch, '\x40'..='\x7e') {
                        break;
                    }
                }
            }
            // Non-CSI escape (single byte after ESC) or end-of-string: nothing
            // to skip; the next iteration resumes the scan.
        } else {
            out.push(c);
        }
    }
    out
}

fn parse_focused_client_panes(stdout: &[u8]) -> Vec<PaneId> {
    let mut panes = Vec::new();
    for line in String::from_utf8_lossy(stdout).lines() {
        let clean = strip_ansi(line);
        let mut cols = clean.split_whitespace();
        let Some(first) = cols.next() else {
            continue;
        };
        let Some(raw_pane) = cols.next() else {
            continue;
        };
        if first == "CLIENT_ID" || raw_pane == "ZELLIJ_PANE_ID" {
            continue;
        }
        if !raw_pane.starts_with("terminal_") {
            continue;
        }
        let pane = PaneId::from_parts(MuxName::Zellij, raw_pane);
        if !panes.iter().any(|known| known == &pane) {
            panes.push(pane);
        }
    }
    panes
}

fn trim_capture(raw_text: String, max_lines: Option<u16>) -> (String, Vec<String>) {
    let mut lines: Vec<String> = raw_text.lines().map(str::to_owned).collect();
    if let Some(max_lines) = max_lines {
        let keep = max_lines as usize;
        if keep == 0 {
            lines.clear();
        } else if lines.len() > keep {
            lines = lines.split_off(lines.len() - keep);
        }
    }

    let mut trimmed = lines.join("\n");
    if raw_text.ends_with('\n') && !trimmed.is_empty() {
        trimmed.push('\n');
    }
    (trimmed, lines)
}

#[cfg(test)]
mod tests;
