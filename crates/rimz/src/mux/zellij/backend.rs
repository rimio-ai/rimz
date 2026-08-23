//! Zellij [`MuxBackend`](crate::mux::MuxBackend) trait implementation.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::super::mount_proof::{prove_sidebar_mount, sidebar_build_identity};
use super::ZellijBackend;
use super::layout::{TempLayoutFile, render_background_view_layout, render_tab_layout};
use super::pane_topology::{PaneTopologyCache, PaneTopologyPane, ZellijPaneId};
use super::parse::{
    classify_session_not_found, is_no_active_sessions, is_session_not_found, is_transient_empty,
    live_session_name_from_line, parse_client_view, trim_capture,
};
use super::raw_pane::{
    floating_panes_in_anchor_view, is_daemon_host_pane, is_sidebar_pane, sidebar_geometry_off_spec,
    tab_view_cols,
};
use super::sidebar::DockOutcome;
use crate::ids::{MuxName, PaneId, WorkspaceId};
use crate::mux::{
    BRACKET_PASTE_CLOSE, BRACKET_PASTE_OPEN, BackgroundViewLaunch, BackgroundViewOptions,
    CachedPaneRoster, ClientFocusOptions, ClientView, CommandSpec, DaemonView, MuxBackend, MuxErr,
    NamedKey, PaneCapture, PaneListOptions, PaneListing, ReconcileAddOutcome, ReconcilePane,
    ReconcilePaneRole, Result, SessionHealth, SessionLiveness, SessionOptions, SidebarLiveness,
    SidebarPaneOptions, SidebarRecovery, SplitDirection, SplitPaneOptions, SplitPlacement,
    SplitTarget, TabOptions, WidthStep, ensure_pane_backend, execute_reconcile_plan,
    group_reconcile_panes, memoized_version, paste_payload,
};
use crate::store::RuntimePaths;
use serde::Deserialize;

/// Prefix `command` with an `env KEY=VALUE …` shim so a freshly split Zellij
/// pane inherits the requested vars; Zellij's `new-pane` has no env flag of its
/// own. An empty env map returns the command unchanged.
fn env_prefixed(env: &BTreeMap<String, String>, command: Vec<String>) -> Vec<String> {
    if env.is_empty() {
        return command;
    }
    let mut wrapped = Vec::with_capacity(command.len() + env.len() + 1);
    wrapped.push("env".to_owned());
    wrapped.extend(env.iter().map(|(key, value)| format!("{key}={value}")));
    wrapped.extend(command);
    wrapped
}

#[derive(Clone, Debug)]
struct FocusRestoreTarget {
    pane: PaneId,
    tab_position: u64,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawTab {
    pub(super) name: String,
    #[serde(default)]
    selectable_tiled_panes_count: u64,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawListedPane {
    pub(super) id: u64,
    #[serde(default)]
    pub(super) is_plugin: bool,
    #[serde(default)]
    is_held: bool,
    #[serde(default)]
    exited: bool,
    #[serde(default)]
    is_suppressed: bool,
    #[serde(default)]
    is_floating: bool,
    /// Stable Zellij tab id accepted by `new-pane --tab-id`.
    #[serde(default)]
    tab_id: Option<u64>,
    /// Current on-screen tab position. Older Zellij versions exposed only
    /// `tab_id`, where that value also served as the position.
    #[serde(default)]
    tab_position: Option<u64>,
    #[serde(default)]
    tab_name: Option<String>,
    #[serde(default)]
    pane_columns: Option<u64>,
    #[serde(default)]
    pane_x: Option<u64>,
    #[serde(default)]
    pub(super) title: Option<String>,
    #[serde(default)]
    terminal_command: Option<String>,
}

impl From<RawListedPane> for PaneTopologyPane {
    fn from(pane: RawListedPane) -> Self {
        Self {
            id: pane.id,
            is_plugin: pane.is_plugin,
            is_held: pane.is_held,
            exited: pane.exited,
            is_suppressed: pane.is_suppressed,
            is_floating: pane.is_floating,
            tab_position: pane.tab_position.or(pane.tab_id).unwrap_or_default(),
            tab_name: pane.tab_name,
            pane_columns: pane.pane_columns,
            pane_x: pane.pane_x,
            title: pane.title,
            pane_command: None,
            pane_cwd: None,
            pane_pid: None,
            terminal_command: pane.terminal_command,
        }
    }
}

fn merge_topology_enrichment(cache: &mut PaneTopologyCache, prior: PaneTopologyCache) {
    let enrichment = prior
        .panes
        .into_iter()
        .map(|pane| {
            (
                pane.native_id(),
                (
                    pane.pane_command,
                    pane.pane_cwd,
                    pane.pane_pid,
                    pane.pane_columns,
                    pane.pane_x,
                ),
            )
        })
        .collect::<HashMap<_, _>>();
    for pane in &mut cache.panes {
        let Some((command, cwd, pid, columns, x)) = enrichment.get(&pane.native_id()) else {
            continue;
        };
        if pane.pane_command.is_none() {
            pane.pane_command.clone_from(command);
        }
        if pane.pane_cwd.is_none() {
            pane.pane_cwd.clone_from(cwd);
        }
        if pane.pane_pid.is_none() {
            pane.pane_pid = *pid;
        }
        if pane.pane_columns.is_none() {
            pane.pane_columns = *columns;
        }
        if pane.pane_x.is_none() {
            pane.pane_x = *x;
        }
    }
}

impl ZellijBackend {
    fn restore_background_split_focus(
        &self,
        placement: SplitPlacement,
        focus: bool,
        workspace_id: Option<WorkspaceId>,
        target: &SplitTarget,
    ) {
        if placement == SplitPlacement::Stacked || focus {
            return;
        }
        let Some(target_pane) = target.pane_id() else {
            return;
        };
        let session_name = target.session_name().unwrap_or_default();
        if let Some(workspace_id) = workspace_id
            && let Ok(runtime) = self.runtime_paths_for_workspace(workspace_id)
        {
            let _ = crate::sidebar::focus_anchor::execute_action(
                self,
                &runtime,
                session_name,
                target_pane.clone(),
                crate::sidebar::focus_anchor::FocusOrigin::User,
                None,
                Default::default(),
            );
        } else {
            let _ = self.focus_pane(target_pane, Some(session_name));
        }
    }

    pub(super) fn tab_id_for_pane(&self, session_name: &str, pane: &PaneId) -> Result<u64> {
        self.tab_id_for_pane_within(session_name, pane, super::super::COMMAND_TIMEOUT)
    }

    fn tab_id_for_pane_within(
        &self,
        session_name: &str,
        pane: &PaneId,
        timeout: Duration,
    ) -> Result<u64> {
        let pane_id = ZellijPaneId::try_from(pane)
            .ok()
            .and_then(ZellijPaneId::terminal_id)
            .ok_or_else(|| MuxErr::Output {
                program: "zellij".to_owned(),
                reason: format!("target pane `{pane}` has no numeric Zellij id"),
            })?;
        let listed = self.raw_listed_panes(session_name, timeout)?;
        listed
            .into_iter()
            .find(|candidate| !candidate.is_plugin && candidate.id == pane_id)
            .and_then(|candidate| candidate.tab_id.or(candidate.tab_position))
            .ok_or_else(|| MuxErr::Output {
                program: "zellij".to_owned(),
                reason: format!("target pane `{pane}` is absent from session `{session_name}`"),
            })
    }

    pub(super) fn raw_listed_panes(
        &self,
        session_name: &str,
        timeout: Duration,
    ) -> Result<Vec<RawListedPane>> {
        let output = self
            .zellij_action(session_name)
            .args(["list-panes", "--all", "--json"])
            .run_with_timeout(timeout)?;
        serde_json::from_slice(&output.stdout).map_err(|err| MuxErr::Output {
            program: "zellij".to_owned(),
            reason: format!("parsing `list-panes --all --json`: {err}"),
        })
    }

    pub(super) fn authoritative_pane_listing(
        &self,
        session_name: &str,
        runtime_paths: Option<&RuntimePaths>,
        workspace_id: Option<&WorkspaceId>,
        timeout: Duration,
    ) -> Result<PaneTopologyCache> {
        let observed_at_ms = crate::sidebar::timing::unix_now_ms();
        let listed = self.raw_listed_panes(session_name, timeout)?;
        let mut cache = PaneTopologyCache {
            session_name: session_name.to_owned(),
            produced_at_ms: observed_at_ms,
            writer: None,
            focused_pane: None,
            clients: None,
            panes: listed.into_iter().map(Into::into).collect(),
        };
        let runtime = runtime_paths
            .cloned()
            .or_else(|| workspace_id.and_then(|id| self.runtime_paths_for_authoritative(id)));
        if let Some(runtime) = runtime
            && let Some(prior) =
                crate::sidebar::cache::read_pane_topology_cache(&runtime, session_name)
        {
            merge_topology_enrichment(&mut cache, prior);
        }
        Ok(cache)
    }

    fn runtime_paths_for_authoritative(
        &self,
        workspace_id: &WorkspaceId,
    ) -> Option<crate::store::RuntimePaths> {
        match &self.runtime_dir {
            Some(dir) => crate::store::RuntimePaths::under(workspace_id.clone(), dir),
            None => crate::store::RuntimePaths::for_workspace(workspace_id.clone()),
        }
        .ok()
    }

    fn focus_restore_target(
        &self,
        session_name: &str,
        workspace_id: &WorkspaceId,
    ) -> Option<FocusRestoreTarget> {
        let mut viewed = self
            .client_view(ClientFocusOptions {
                session_name: Some(session_name.to_owned()),
                command_timeout: None,
            })
            .map(|view| view.viewed_panes)
            .ok()?;
        viewed.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        viewed.dedup();
        let [pane] = viewed.as_slice() else {
            return None;
        };
        let panes = self
            .topology_panes_for_workspace(
                session_name,
                workspace_id,
                Some(crate::sidebar::timing::unix_now_ms()),
                super::super::COMMAND_TIMEOUT,
            )
            .ok()?;
        panes.iter().find_map(|candidate| {
            (candidate.is_live_terminal() && parse_zellij_raw(pane) == Some(candidate.id))
                .then_some(FocusRestoreTarget {
                    pane: pane.clone(),
                    tab_position: candidate.tab_position,
                })
        })
    }

    fn restore_attached_client_focus(
        &self,
        session_name: &str,
        workspace_id: &WorkspaceId,
        restore: &FocusRestoreTarget,
    ) -> Result<()> {
        let runtime = self.runtime_paths_for_workspace(workspace_id.clone())?;
        execute_focus_restoration(
            self,
            &runtime,
            session_name,
            &restore.pane,
            restore.tab_position,
        )
        .map_err(focus_action_error)
    }

    fn run_new_tab_confirmed(&self, session: &str, args: &[String], tab_name: &str) -> Result<()> {
        let tabs = self.list_tabs(session)?;
        let config = crate::config::MachineConfig::load_lenient();
        let theme = &config.theme;
        let (before, before_materialized) = named_tab_counts(&tabs, tab_name, theme);
        for attempt in 0..super::NEW_TAB_ATTEMPTS {
            if attempt > 0 {
                let tabs = self.list_tabs(session)?;
                let (named, materialized) = named_tab_counts(&tabs, tab_name, theme);
                if named > before {
                    self.wait_for_named_tab_materialized(
                        session,
                        tab_name,
                        before_materialized,
                        materialized,
                        theme,
                    )?;
                    return Ok(());
                }
            }
            self.zellij_action(session)
                .args(args.iter().cloned())
                .run()?;
            let deadline = Instant::now() + super::NEW_TAB_CONFIRM_WINDOW;
            loop {
                let tabs = self.list_tabs(session)?;
                let (named, materialized) = named_tab_counts(&tabs, tab_name, theme);
                if named > before {
                    self.wait_for_named_tab_materialized(
                        session,
                        tab_name,
                        before_materialized,
                        materialized,
                        theme,
                    )?;
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(super::NEW_TAB_CONFIRM_STEP);
            }
        }
        Err(MuxErr::Output {
            program: "zellij".to_owned(),
            reason: format!(
                "new-tab '{tab_name}' did not appear after {} attempts",
                super::NEW_TAB_ATTEMPTS
            ),
        })
    }

    fn wait_for_named_tab_materialized(
        &self,
        session: &str,
        tab_name: &str,
        before_materialized: usize,
        mut last_count: usize,
        theme: &crate::config::ThemeConfig,
    ) -> Result<()> {
        let deadline = Instant::now() + super::NEW_TAB_MATERIALIZE_WINDOW;
        loop {
            if last_count > before_materialized {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(MuxErr::Output {
                    program: "zellij".to_owned(),
                    reason: format!(
                        "new-tab '{tab_name}' appeared but its layout panes did not materialize; \
                         materialized named tabs stayed at {last_count}"
                    ),
                });
            }
            std::thread::sleep(super::NEW_TAB_MATERIALIZE_STEP);
            last_count = named_tab_counts(&self.list_tabs(session)?, tab_name, theme).1;
        }
    }

    pub(super) fn list_tabs(&self, session: &str) -> Result<Vec<RawTab>> {
        for attempt in 0..super::LIST_TABS_ATTEMPTS {
            if attempt > 0 {
                std::thread::sleep(super::LIST_TABS_RETRY_DELAY);
            }
            let output = self
                .zellij_action(session)
                .args(["list-tabs", "--json", "--panes"])
                .run()
                .map_err(|err| classify_session_not_found(err, session))?;
            if is_session_not_found(&output.stdout) || is_session_not_found(&output.stderr) {
                return Err(MuxErr::SessionNotFound {
                    session: session.to_owned(),
                });
            }
            if is_transient_empty(&output.stdout) {
                continue;
            }
            let tabs = serde_json::from_slice::<Vec<RawTab>>(&output.stdout).map_err(|e| {
                MuxErr::Output {
                    program: "zellij".to_owned(),
                    reason: format!("parsing list-tabs JSON: {e}"),
                }
            })?;
            if tabs.is_empty() {
                continue;
            }
            return Ok(tabs);
        }
        Err(MuxErr::Output {
            program: "zellij".to_owned(),
            reason: format!(
                "list-tabs returned no output after {} attempts",
                super::LIST_TABS_ATTEMPTS
            ),
        })
    }
}

fn named_tab_counts(
    tabs: &[RawTab],
    tab_name: &str,
    theme: &crate::config::ThemeConfig,
) -> (usize, usize) {
    tabs.iter()
        .filter(|tab| crate::theme::strip_status_glyph_suffix(&tab.name, theme) == tab_name)
        .fold((0, 0), |(named, materialized), tab| {
            (
                named + 1,
                materialized + usize::from(tab.selectable_tiled_panes_count > 0),
            )
        })
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

    fn attach_existing_command(&self, name: &str) -> CommandSpec {
        self.cmd().args(["attach", name])
    }

    fn attach_readonly_command(&self, name: &str) -> CommandSpec {
        // Zellij has no read-only attach; broadcast ttyd drops all client input.
        self.attach_existing_command(name)
    }

    fn detach(&self, _name: &str) -> Result<()> {
        self.cmd().args(["action", "detach"]).run().map(|_| ())
    }

    fn kill_session(&self, name: &str) -> Result<()> {
        self.delete_session(name)
    }

    fn list_sessions_within(&self, timeout: std::time::Duration) -> Result<Vec<String>> {
        let output = match self.cmd().arg("list-sessions").run_with_timeout(timeout) {
            Ok(output) => output,
            Err(MuxErr::Command { ref stderr, .. }) if is_no_active_sessions(stderr.as_bytes()) => {
                return Ok(Vec::new());
            }
            Err(err) => return Err(err),
        };
        // Output lines look like `name [Created Ns ago]` for live sessions, or
        // `name [Created Ns ago] (EXITED - attach to resurrect)` for stopped
        // sessions. `list_sessions` is the live-session set used by `rimz list`
        // and `rimz reload`, so filter resurrectable corpses out here.
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(live_session_name_from_line)
            .collect())
    }

    fn session_liveness(&self, name: &str) -> Result<SessionLiveness> {
        self.session_state_checked(name)
    }

    fn cached_pane_roster(
        &self,
        session: &str,
        workspace_id: &WorkspaceId,
    ) -> Option<CachedPaneRoster> {
        let runtime = self.runtime_paths_for_authoritative(workspace_id)?;
        let cache = Self::fresh_cached_topology(
            &runtime,
            session,
            crate::sidebar::timing::unix_now_ms(),
            None,
        )?;
        Some(CachedPaneRoster {
            pane_ids: cache
                .panes
                .into_iter()
                .filter(|pane| !pane.is_plugin)
                .map(|pane| PaneId::from(pane.native_id()))
                .collect(),
            observed_at_ms: cache.produced_at_ms,
        })
    }

    fn list_panes(&self, opts: PaneListOptions) -> Result<PaneListing> {
        let timeout = opts
            .command_timeout
            .unwrap_or(super::super::COMMAND_TIMEOUT);
        let session_name = opts.session_name.unwrap_or_default();
        self.read_topology(
            (!session_name.is_empty()).then_some(session_name.as_str()),
            opts.runtime_paths.as_ref(),
            opts.workspace_id.as_ref(),
            opts.min_topology_produced_at_ms,
            opts.consistency,
            timeout,
        )
        .map(|cache| cache.into_pane_listing(session_name))
    }

    fn client_view(&self, opts: ClientFocusOptions) -> Result<ClientView> {
        let timeout = opts
            .command_timeout
            .unwrap_or(super::super::COMMAND_TIMEOUT);
        let mut spec = self.cmd();
        if let Some(name) = opts.session_name {
            spec = spec.args(["--session".to_owned(), name]);
        }
        let output = spec
            .args(["action", "list-clients"])
            .run_with_timeout(timeout)?;
        Ok(parse_client_view(&output.stdout))
    }

    fn split_pane(&self, opts: SplitPaneOptions) -> Result<()> {
        let focus_workspace = opts
            .env
            .get(crate::workspace::ENV_WORKSPACE_ID)
            .and_then(|value| value.parse::<WorkspaceId>().ok());
        let target = opts.target;
        let session_name = target.session_name();
        let target_pane = target.pane_id();
        if let Some(target_pane) = target_pane {
            ensure_pane_backend(target_pane, MuxName::Zellij)?;
        }
        let anchored_stack = opts.placement == SplitPlacement::Stacked && target_pane.is_some();
        let no_focus = !opts.focus
            && self
                .version()
                .ok()
                .as_deref()
                .and_then(super::parse_version)
                .is_some_and(|version| version >= super::MIN_NO_FOCUS_ZELLIJ_VERSION);
        // Zellij gives `--tab-id` precedence over the CLI pane context. On
        // 0.45+, `--no-focus` lets both tab-targeted and pane-targeted spawns
        // preserve every attached client's view. On 0.44 an anchored stack
        // uses `--near-current-pane` and lets `ZELLIJ_PANE_ID` imply the tab;
        // directional spawns silently no-op with that flag and keep resolving
        // a stable tab id.
        let target_tab_id = match (&target, opts.placement) {
            (
                SplitTarget::SessionPane {
                    session_name,
                    pane_id,
                },
                SplitPlacement::Directional(_),
            ) => Some(self.tab_id_for_pane(session_name, pane_id)?),
            _ => None,
        };
        let mut spec = match session_name {
            Some(session) => self.zellij_action(session).arg("new-pane"),
            None => self.cmd().args(["action", "new-pane"]),
        };
        match opts.placement {
            SplitPlacement::Stacked => {
                spec = spec.arg("--stacked");
                if anchored_stack && !no_focus {
                    spec = spec.arg("--near-current-pane");
                }
            }
            SplitPlacement::Directional(direction) => {
                let direction = match direction {
                    SplitDirection::Right => "right",
                    SplitDirection::Down => "down",
                };
                spec = spec.args(["--direction", direction]);
            }
        }
        if let Some(target_pane) = target_pane {
            let pane_id = ZellijPaneId::try_from(target_pane)
                .map_err(|err| MuxErr::Output {
                    program: "zellij".to_owned(),
                    reason: err.to_string(),
                })?
                .terminal_id()
                .ok_or_else(|| MuxErr::Output {
                    program: "zellij".to_owned(),
                    reason: format!("target pane `{target_pane}` is not a terminal pane"),
                })?;
            spec = spec.env("ZELLIJ_PANE_ID", pane_id.to_string());
        }
        if let Some(tab_id) = target_tab_id {
            spec = spec.args(["--tab-id".to_owned(), tab_id.to_string()]);
        }
        if no_focus {
            spec = spec.arg("--no-focus");
        }
        if opts.close_on_exit {
            spec = spec.arg("--close-on-exit");
        }
        if let Some(title) = opts.title {
            spec = spec.args(["--name".to_owned(), title]);
        }
        if let Some(cwd) = opts.cwd {
            spec = spec.args(["--cwd".to_owned(), cwd]);
        }
        if let Some(command) = opts.command {
            // Zellij's `new-pane` has no env flag, so inject the requested vars
            // through an `env KEY=VALUE …` prefix — the cross-backend match for
            // tmux's native `-e` (see the backend env-injection parity tests).
            let command = env_prefixed(&opts.env, command);
            if let Some((program, args)) = command.split_first() {
                spec = spec
                    .args(["--".to_owned(), program.clone()])
                    .args(args.iter().cloned());
            }
        }
        spec.run()?;
        self.restore_background_split_focus(opts.placement, opts.focus, focus_workspace, &target);
        Ok(())
    }

    fn focus_pane(&self, pane: &PaneId, session: Option<&str>) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Zellij)?;
        let target = ZellijPaneId::try_from(pane)
            .map_err(|err| MuxErr::Output {
                program: "zellij".to_owned(),
                reason: err.to_string(),
            })?
            .action_target();
        // Zellij 0.41+: `focus-pane-id <raw>`. The earlier `focus-pane-with-id`
        // name was removed; the stub that referenced it never reached a
        // running binary.
        let spec = match session {
            Some(session) => self.zellij_action(session).arg("focus-pane-id"),
            None => self.cmd().args(["action", "focus-pane-id"]),
        };
        spec.arg(target).run().map(|_| ())
    }

    fn sidebar_width_step(
        &self,
        runtime: &RuntimePaths,
        session: &str,
        pane: &PaneId,
        min_observed_at_ms: Option<u64>,
    ) -> Result<WidthStep> {
        ensure_pane_backend(pane, MuxName::Zellij)?;
        let pane_id = ZellijPaneId::try_from(pane)
            .ok()
            .and_then(ZellijPaneId::terminal_id)
            .ok_or_else(|| MuxErr::Output {
                program: "zellij".to_owned(),
                reason: format!("target pane `{pane}` has no numeric topology id"),
            })?;
        let cache = Self::fresh_cached_topology(
            runtime,
            session,
            crate::sidebar::timing::unix_now_ms(),
            min_observed_at_ms,
        )
        .ok_or_else(|| MuxErr::Output {
            program: "zellij".to_owned(),
            reason: format!("fresh pane topology is unavailable for session `{session}`"),
        })?;
        let tab_position = cache
            .panes
            .iter()
            .find(|candidate| !candidate.is_plugin && candidate.id == pane_id)
            .map(|candidate| candidate.tab_position)
            .ok_or_else(|| MuxErr::Output {
                program: "zellij".to_owned(),
                reason: format!("target pane `{pane}` is absent from the topology cache"),
            })?;
        let view_cols =
            tab_view_cols(&cache.panes, tab_position).ok_or_else(|| MuxErr::Output {
                program: "zellij".to_owned(),
                reason: format!("tab {tab_position} has no tiled topology width"),
            })?;
        let cols = u16::try_from(crate::mux::width::zellij_resize_step_cols(view_cols))
            .unwrap_or(u16::MAX);
        let band_cols = u16::try_from(crate::mux::width::zellij_resize_stop_step_cols(view_cols))
            .unwrap_or(u16::MAX);
        Ok(WidthStep {
            cols,
            band_cols,
            exact: false,
            view_cols: u16::try_from(view_cols).unwrap_or(0),
        })
    }

    fn nudge_sidebar_width(
        &self,
        session: &str,
        pane: &PaneId,
        current_cols: u16,
        target_cols: u16,
    ) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Zellij)?;
        if current_cols == target_cols {
            return Ok(());
        }
        let target = ZellijPaneId::try_from(pane)
            .map_err(|err| MuxErr::Output {
                program: "zellij".to_owned(),
                reason: err.to_string(),
            })?
            .action_target();
        self.resize_sidebar_step(
            session,
            &target,
            if current_cols < target_cols {
                "increase"
            } else {
                "decrease"
            },
        )
    }

    fn record_sidebar_width_default(&self, _session: &str, _cols: u16) -> Result<()> {
        // Zellij birth layouts read the room-runtime override when generated.
        Ok(())
    }

    fn capture_pane(&self, pane: &PaneId, lines: Option<u16>, ansi: bool) -> Result<PaneCapture> {
        ensure_pane_backend(pane, MuxName::Zellij)?;
        let target = ZellijPaneId::try_from(pane)
            .map_err(|err| MuxErr::Output {
                program: "zellij".to_owned(),
                reason: err.to_string(),
            })?
            .action_target();
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
        spec = spec.args(["-p".to_owned(), target]);
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
        let target = ZellijPaneId::try_from(pane)
            .map_err(|err| MuxErr::Output {
                program: "zellij".to_owned(),
                reason: err.to_string(),
            })?
            .action_target();
        self.cmd()
            .args(["action", "write-chars", "--pane-id", &target, text])
            .run()
            .map(|_| ())
    }

    fn send_key(&self, pane: &PaneId, key: NamedKey) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Zellij)?;
        let target = ZellijPaneId::try_from(pane)
            .map_err(|err| MuxErr::Output {
                program: "zellij".to_owned(),
                reason: err.to_string(),
            })?
            .action_target();
        let bytes = key.write_bytes().iter().map(u8::to_string);
        self.cmd()
            .args(["action", "write", "--pane-id", &target])
            .args(bytes)
            .run()
            .map(|_| ())
    }

    fn paste_text(&self, pane: &PaneId, text: &str) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Zellij)?;
        let payload = paste_payload(text);
        let target = ZellijPaneId::try_from(pane)
            .map_err(|err| MuxErr::Output {
                program: "zellij".to_owned(),
                reason: err.to_string(),
            })?
            .action_target();
        // Open marker + text + close marker as one decimal byte list, mirroring
        // the existing `send_key` byte-write mechanism. Raw bytes pass through
        // untouched, so a marker-looking byte inside `text` is never re-parsed.
        let bytes = BRACKET_PASTE_OPEN
            .bytes()
            .chain(payload.bytes())
            .chain(BRACKET_PASTE_CLOSE.bytes())
            .map(|byte| byte.to_string());
        self.cmd()
            .args(["action", "write", "--pane-id", &target])
            .args(bytes)
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
            SessionLiveness::Absent => self.create_session_with_sidebar(opts, daemon),
            SessionLiveness::Exited => {
                self.delete_session(&opts.session_name)?;
                self.create_session_with_sidebar(opts, daemon)
            }
            SessionLiveness::Live => {
                match self.inspect_session_panes(&opts.session_name, &opts.workspace_id) {
                    Ok(()) => {
                        self.delete_session(&opts.session_name)?;
                        self.create_session_with_sidebar(opts, daemon)
                    }
                    Err(err) => {
                        tracing::warn!(
                            session = %opts.session_name,
                            tags.operation = "zellij.room_inspect",
                            error = &err as &dyn std::error::Error,
                            "live zellij room could not be inspected; leaving it untouched",
                        );
                        Err(err)
                    }
                }
            }
        }
    }

    fn probe_session_health(&self, name: &str) -> Result<SessionHealth> {
        Ok(match self.session_state(name) {
            // Nothing to attach to — a fresh birth will produce a clean room.
            SessionLiveness::Absent => SessionHealth::Healthy,
            // `attach --create` would resurrect a serialized, suspended layout.
            SessionLiveness::Exited => SessionHealth::Stuck,
            // `list-sessions` liveness is the attach gate truth; attach live rooms as-is.
            SessionLiveness::Live => SessionHealth::Healthy,
        })
    }

    fn ensure_clean_session(
        &self,
        opts: &SidebarPaneOptions,
        daemon: Option<&DaemonView>,
    ) -> Result<SessionHealth> {
        let state = self.session_state(&opts.session_name);
        // A live room is trusted from `list-sessions` alone: attach as-is, never
        // inspect panes. A stale topology cache is not evidence the room is
        // stuck.
        if matches!(state, SessionLiveness::Live) {
            return Ok(SessionHealth::Healthy);
        }
        // Absent → first birth; Exited → delete and rebirth from the layout so
        // the room comes up clean and RUNNING (with serialization off, a rebirth
        // can never resurrect). A rebirth that still fails to talk to Zellij
        // reads as Stuck so the caller runs or reports the reset path.
        let rebirth = || -> Result<()> {
            if !matches!(state, SessionLiveness::Absent) {
                self.delete_session(&opts.session_name)?;
            }
            self.create_session_with_sidebar(opts, daemon)
        };
        match rebirth() {
            Ok(()) => Ok(SessionHealth::Reborn),
            Err(
                err @ (MuxErr::SocketPathTooLong { .. } | MuxErr::SocketPathReportedTooLong { .. }),
            ) => Err(err),
            Err(err) => {
                tracing::warn!(
                    session = %opts.session_name,
                    tags.operation = "zellij.session_rebirth",
                    error = &err as &dyn std::error::Error,
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
        super::session::purge_zellij_session_cache_in(&crate::store::paths::cache_home(), name)
    }

    fn resurrection_cache_paths(&self, name: &str) -> Vec<PathBuf> {
        super::session::zellij_session_cache_paths_in(&crate::store::paths::cache_home(), name)
    }

    fn reconcile_sidebars(
        &self,
        opts: &SidebarPaneOptions,
        live: &SidebarLiveness,
    ) -> Result<SidebarRecovery> {
        // Zellij docks the sidebar left only at session birth, but a left pane
        // can still be reached in a live session: close a stray sidebar by id,
        // or mount one through a stable tab id before moving it left and sizing
        // it to the tab's live target. This never rebirths the session, so
        // working panes survive.
        let listing = self.topology_listing(
            Some(&opts.session_name),
            None,
            Some(&opts.workspace_id),
            live.topology_floor_ms,
            crate::sidebar::timing::RECONCILE_LIST_TIMEOUT,
        )?;
        let panes = listing.panes;
        let views = group_reconcile_panes(panes.iter().filter_map(reconcile_pane));
        let plan = super::super::plan_reconcile(&views, live);
        let planned_closes = plan.close_panes();
        // Kept sidebars (not planned for closing) whose geometry sits off the
        // layout's dock — the residue of a mis-mounted add — converge in place
        // this pass, renderer untouched.
        let width_floor = live.topology_floor_ms;
        let off_spec = off_spec_sidebars(&panes, &planned_closes, width_floor.map(|_| opts.target));
        if plan.is_empty() && off_spec.is_empty() {
            return Ok(SidebarRecovery::default());
        }

        // Structural repair is scoped to the attached client view. Hidden tabs
        // have no RimZ focus state and repair themselves when later viewed.
        let restoration = self
            .client_view(ClientFocusOptions {
                session_name: Some(opts.session_name.clone()),
                command_timeout: Some(crate::sidebar::timing::RECONCILE_LIST_TIMEOUT),
            })
            .ok()
            .and_then(|view| client_restoration_target(&panes, &view));

        let mut report = SidebarRecovery::default();
        // In-place adds and geometry moves both need an attached client: a
        // detached session's screen thread drops the mount while the spawned
        // serve pair keeps running, so adding there only leaks (the closes
        // above are safe detached). An unanswerable probe reads detached —
        // deferring one run is recoverable, a leaked pair is not. tmux splits
        // fine detached, so the gate is Zellij-internal.
        let needs_attached = plan.has_adds() || !off_spec.is_empty();
        let attached = !needs_attached || self.session_has_attached_client(&opts.session_name);
        if attached {
            for (tab_position, raw_id) in &off_spec {
                repair_sidebar_geometry(
                    self,
                    opts,
                    *tab_position,
                    *raw_id,
                    width_floor,
                    &mut report,
                );
            }
        }
        if needs_attached && !attached {
            report.deferred += off_spec.len();
        }
        let build = sidebar_build_identity(opts)?;
        let failure = execute_reconcile_plan(
            plan,
            &mut report,
            !attached,
            |view| {
                let tab_position = view.parse::<u64>().map_err(|err| MuxErr::Output {
                    program: "zellij".to_owned(),
                    reason: format!("invalid sidebar tab position `{view}`: {err}"),
                })?;
                let added = self.add_sidebar_to_tab(opts, tab_position, width_floor)?;
                if !prove_sidebar_mount(opts, MuxName::Zellij, &added.pane, &build, || {
                    if let Some(raw_id) = ZellijPaneId::try_from(&added.pane)
                        .ok()
                        .and_then(ZellijPaneId::terminal_id)
                    {
                        self.cleanup_failed_add(opts, raw_id);
                    }
                }) {
                    return Err(MuxErr::Output {
                        program: "zellij".to_owned(),
                        reason: format!(
                            "sidebar {} mounted in tab {tab_position} without a current-build heartbeat",
                            added.pane
                        ),
                    });
                }
                Ok(match added.dock {
                    DockOutcome::Docked => ReconcileAddOutcome::Verified,
                    DockOutcome::Misdocked => ReconcileAddOutcome::VerifiedMisdocked,
                })
            },
            |pane| self.close_pane(&opts.session_name, pane),
        );
        if needs_attached && !attached {
            tracing::info!(
                session = %opts.session_name,
                deferred = report.deferred,
                "sidebar reconcile: no attached client; deferring in-place adds and geometry repairs",
            );
        }
        if let Some(failure) = failure {
            tracing::warn!(
                session = %opts.session_name,
                view = %failure.view,
                tags.operation = "zellij.reconcile.transaction",
                error = &failure.error as &dyn std::error::Error,
                "sidebar repair aborted; leaving remaining views unchanged",
            );
        }
        if let Some(restoration) = restoration {
            restore_client_view(self, opts, restoration);
        }
        Ok(report)
    }

    fn open_background_view(&self, opts: &BackgroundViewOptions) -> Result<BackgroundViewLaunch> {
        let session = &opts.sidebar.session_name;
        // Idempotent on the tab name. The lead position is owned by session birth
        // ([`Self::open_sidebar`] with a `daemon`): `rimz start` births the session
        // with this tab already leading, so the common case is a no-op here. A
        // failed query propagates rather than risk a duplicate launch.
        if self.session_has_named_tab(session, &opts.view.name)? {
            return Ok(BackgroundViewLaunch::AlreadyRunning);
        }
        // Late add: the session was born without the daemon tab (e.g. a host
        // became available after first start) and now carries one or more working
        // tabs. Zellij can't move a tab to the front, so this appended tab does
        // *not* lead — leading is a birth-time property. `--layout` gives the tab
        // its `sidebar | content | hosts…` shape directly (bypassing the tab
        // template, so the sidebar is spelled out). Zellij can drop transient
        // `new-tab` mutations under load, so keep the temp layout alive until
        // the named tab is confirmed. Each pane carries its own `cwd`, so no
        // tab-level `--cwd` is needed.
        let layout = TempLayoutFile::new(render_background_view_layout(opts)?)?;
        let args = [
            "new-tab".to_owned(),
            "--layout".to_owned(),
            layout.path().to_string_lossy().into_owned(),
            "--name".to_owned(),
            opts.view.name.clone(),
        ];
        self.run_new_tab_confirmed(session, &args, &opts.view.name)?;
        drop(layout);
        // `new-tab` focuses the tab it creates. Return focus to the leading tab so
        // the imminent `attach` lands on a working pane, not this freshly-added
        // daemon tab. Best-effort: a focus hiccup never sinks a launch.
        if let Err(err) = self.go_to_lead_tab(session) {
            tracing::warn!(
                session = %session,
                tags.operation = "zellij.focus_tab",
                error = &err as &dyn std::error::Error,
                "could not return focus off the freshly-added daemon tab",
            );
        }
        Ok(BackgroundViewLaunch::Launched)
    }

    fn open_tab(&self, opts: &TabOptions) -> Result<()> {
        // Zellij always focuses `new-tab`; tmux gets unfocused opens from
        // `new-window -d`. Capture the attached client's pane and tab id first
        // and restore both after the tab exists. A session that offers no single
        // restore target falls back to the lead tab, which lands whenever a
        // client is attached to receive it.
        let restore = (!opts.focus)
            .then(|| {
                self.focus_restore_target(&opts.sidebar.session_name, &opts.sidebar.workspace_id)
            })
            .flatten();
        let view_cols = (|| {
            let panes = self
                .topology_panes(
                    &opts.sidebar.session_name,
                    None,
                    crate::sidebar::timing::RECONCILE_LIST_TIMEOUT,
                )
                .ok()?;
            let tab = panes
                .iter()
                .find(|pane| pane.is_live_terminal())?
                .tab_position;
            tab_view_cols(&panes, tab)
                .and_then(|cols| u16::try_from(cols).ok())
                .filter(|cols| *cols > 0)
        })();
        let runtime = match self.runtime_dir.as_deref() {
            Some(root) => {
                crate::store::RuntimePaths::under(opts.sidebar.workspace_id.clone(), root)
            }
            None => crate::store::RuntimePaths::for_workspace(opts.sidebar.workspace_id.clone()),
        }
        .map_err(|err| MuxErr::Output {
            program: "zellij".to_owned(),
            reason: format!("cannot resolve sidebar runtime paths: {err}"),
        })?;
        let width = crate::mux::SidebarWidth::from_config(
            &crate::config::MachineConfig::load_lenient().theme,
        );
        let sidebar_percent =
            crate::sidebar::width_target::resolve(&runtime, width, view_cols).percent();
        let layout = TempLayoutFile::new(render_tab_layout(opts, sidebar_percent)?)?;
        let args = [
            "new-tab".to_owned(),
            "--layout".to_owned(),
            layout.path().to_string_lossy().into_owned(),
            "--name".to_owned(),
            opts.title.clone(),
        ];
        self.run_new_tab_confirmed(&opts.sidebar.session_name, &args, &opts.title)?;
        drop(layout);
        if !opts.focus {
            let result = match &restore {
                Some(restore) => self.restore_attached_client_focus(
                    &opts.sidebar.session_name,
                    &opts.sidebar.workspace_id,
                    restore,
                ),
                None => self.go_to_lead_tab(&opts.sidebar.session_name),
            };
            if let Err(err) = result {
                tracing::warn!(
                    session = %opts.sidebar.session_name,
                    tags.operation = "zellij.focus_tab",
                    error = &err as &dyn std::error::Error,
                    "could not return focus after opening an unfocused tab",
                );
            }
        }
        Ok(())
    }

    fn rename_tab(&self, session: &str, anchor: &PaneId, name: &str) -> Result<()> {
        let tab_id =
            self.tab_id_for_pane_within(session, anchor, super::super::TAB_RENAME_TIMEOUT)?;
        self.zellij_action(session)
            .args([
                "rename-tab-by-id".to_owned(),
                tab_id.to_string(),
                name.to_owned(),
            ])
            .run_with_timeout(super::super::TAB_RENAME_TIMEOUT)
            .map(|_| ())
    }

    fn close_pane(&self, session: &str, pane: &PaneId) -> Result<()> {
        ZellijBackend::close_pane(self, session, pane)
    }

    fn close_view_floating_panes(&self, session: &str, anchor: &PaneId) -> Result<Vec<PaneId>> {
        ensure_pane_backend(anchor, MuxName::Zellij)?;
        let panes = self.topology_panes(session, None, super::super::COMMAND_TIMEOUT)?;
        let mut closed = Vec::new();
        for pane_id in floating_panes_in_anchor_view(&panes, anchor) {
            match self.close_pane(session, &pane_id) {
                Ok(()) => closed.push(pane_id),
                Err(err) => tracing::warn!(
                    session,
                    pane = %pane_id,
                    tags.operation = "zellij.close_floating_pane",
                    error = &err as &dyn std::error::Error,
                    "could not close floating pane during sidebar self-close",
                ),
            }
        }
        Ok(closed)
    }

    fn ensure_presence_plugin(&self, opts: &super::super::PresencePluginOptions) -> Result<()> {
        if opts.converge {
            self.converge_presence_plugin_for(opts)
        } else {
            self.ensure_presence_plugin_for(opts)
        }
    }

    fn version(&self) -> Result<String> {
        memoized_version(&self.version, &self.cmd().arg("--version"))
    }
}

pub(super) fn reconcile_pane(pane: &PaneTopologyPane) -> Option<ReconcilePane> {
    if !pane.is_terminal() {
        return None;
    }
    let role = ReconcilePaneRole::from_evidence(is_sidebar_pane(pane), is_daemon_host_pane(pane));
    Some(ReconcilePane {
        view: pane.tab_position.to_string(),
        pane_id: PaneId::from(pane.native_id()),
        role,
    })
}

fn off_spec_sidebars(
    panes: &[PaneTopologyPane],
    closing: &[PaneId],
    width_target: Option<crate::mux::SidebarTarget>,
) -> Vec<(u64, u64)> {
    let closing: HashSet<u64> = closing.iter().filter_map(parse_zellij_raw).collect();
    panes
        .iter()
        .filter(|pane| pane.is_live_terminal() && is_sidebar_pane(pane))
        .filter(|pane| !closing.contains(&pane.id))
        .filter(|pane| sidebar_geometry_off_spec(pane, panes, &closing, width_target))
        .map(|pane| (pane.tab_position, pane.id))
        .collect()
}

fn parse_zellij_raw(pane: &PaneId) -> Option<u64> {
    ZellijPaneId::try_from(pane)
        .ok()
        .and_then(ZellijPaneId::terminal_id)
}

fn repair_sidebar_geometry(
    backend: &ZellijBackend,
    opts: &SidebarPaneOptions,
    tab_position: u64,
    raw_id: u64,
    width_floor: Option<u64>,
    report: &mut SidebarRecovery,
) {
    let floor = backend.converge_sidebar_geometry(opts, tab_position, raw_id, width_floor);
    match backend.sidebar_dock_outcome(
        &opts.session_name,
        &opts.workspace_id,
        tab_position,
        raw_id,
        floor,
    ) {
        DockOutcome::Docked => {
            if floor.is_some() {
                report.redocked += 1;
            }
        }
        DockOutcome::Misdocked => {
            report.misdocked += 1;
        }
    }
}

fn client_restoration_target(
    panes: &[PaneTopologyPane],
    view: &ClientView,
) -> Option<(u64, Option<u64>)> {
    let mut viewed = view
        .viewed_panes
        .iter()
        .filter_map(parse_zellij_raw)
        .collect::<Vec<_>>();
    viewed.sort_unstable();
    viewed.dedup();
    let [viewed] = viewed.as_slice() else {
        return None;
    };
    let pane = panes
        .iter()
        .find(|pane| pane.id == *viewed && pane.is_live_terminal())?;
    Some((
        pane.tab_position,
        (!is_sidebar_pane(pane)).then_some(pane.id),
    ))
}

fn restore_client_view(
    backend: &ZellijBackend,
    opts: &SidebarPaneOptions,
    restoration: (u64, Option<u64>),
) {
    let Ok(panes) = backend.topology_panes_for_workspace(
        &opts.session_name,
        &opts.workspace_id,
        None,
        crate::sidebar::timing::RECONCILE_LIST_TIMEOUT,
    ) else {
        return;
    };
    let (tab_position, preferred) = restoration;
    let work = preferred
        .filter(|id| {
            panes.iter().any(|pane| {
                pane.id == *id
                    && pane.tab_position == tab_position
                    && pane.is_live_terminal()
                    && !is_sidebar_pane(pane)
            })
        })
        .or_else(|| super::raw_pane::leftmost_live_work_pane(&panes, tab_position));
    let Some(work) = work else {
        return;
    };
    let Ok(runtime) = backend.runtime_paths_for_workspace(opts.workspace_id.clone()) else {
        return;
    };
    let pane = PaneId::from(ZellijPaneId::Terminal(work));
    let _ = execute_focus_restoration(backend, &runtime, &opts.session_name, &pane, tab_position);
}

fn execute_focus_restoration(
    backend: &ZellijBackend,
    runtime: &crate::store::RuntimePaths,
    session_name: &str,
    pane: &PaneId,
    tab_position: u64,
) -> std::result::Result<(), crate::sidebar::focus_anchor::FocusActionError> {
    let nonce = crate::sidebar::focus_anchor::request_action(
        backend,
        runtime,
        session_name,
        crate::sidebar::focus_anchor::FocusActionRequest {
            pane_id: pane.clone(),
            origin: crate::sidebar::focus_anchor::FocusOrigin::User,
            repair_generation: None,
            expected_pre_action: None,
            offset: 0,
            order: None,
        },
    )?;
    let _ = backend.go_to_tab_position(session_name, tab_position);
    if crate::sidebar::focus_anchor::dispatch_action(
        backend,
        runtime,
        session_name,
        pane,
        nonce,
        crate::sidebar::focus_anchor::FocusDispatchRetries {
            attempts: super::FOCUS_RESTORE_ATTEMPTS,
            delay: super::FOCUS_RESTORE_RETRY_DELAY,
        },
    )? {
        Ok(())
    } else {
        Err(crate::sidebar::focus_anchor::FocusActionError::Superseded)
    }
}

fn focus_action_error(error: crate::sidebar::focus_anchor::FocusActionError) -> MuxErr {
    MuxErr::Output {
        program: "zellij".to_owned(),
        reason: error.to_string(),
    }
}
