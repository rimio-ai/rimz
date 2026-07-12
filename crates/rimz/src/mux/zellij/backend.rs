//! Zellij [`MuxBackend`](crate::mux::MuxBackend) trait implementation.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::ZellijBackend;
use super::layout::{TempLayoutFile, render_background_view_layout, render_tab_layout};
use super::pane_topology::{PaneTopologyCache, PaneTopologyPane};
use super::parse::{
    SessionState, classify_session_not_found, is_no_active_sessions, is_session_not_found,
    is_transient_empty, live_session_name_from_line, parse_focused_client_panes,
    parse_focused_terminal_client_ids, trim_capture,
};
use super::raw_pane::{
    RawPane, RawPaneListing, SessionCleanliness, SidebarDock, floating_panes_in_anchor_view,
    is_sidebar_pane, own_zellij_pane_id, repairable_nested_work_pane_ids, sidebar_dock_verdict,
    sidebar_geometry_off_spec, tab_view_cols, tabs_with_sidebars, views_with_sidebars,
};
use super::sidebar::DockOutcome;
use crate::ids::{MuxName, PaneId, WorkspaceId};
use crate::mux::width::{live_target_cols, sidebar_width_off_spec};
use crate::mux::{
    AddOutcome, BRACKET_PASTE_CLOSE, BRACKET_PASTE_OPEN, BackgroundViewLaunch,
    BackgroundViewOptions, ClientFocusOptions, ClientPresence, ClientView, CommandSpec, DaemonView,
    MuxBackend, MuxErr, NamedKey, PaneCapture, PaneListOptions, PaneListing, Result, SessionHealth,
    SessionOptions, SidebarLiveness, SidebarPaneOptions, SidebarRecovery, SplitDirection,
    SplitPaneOptions, TabOptions, WidthAdjust, WidthSyncOptions, ensure_pane_backend, execute_adds,
    execute_closes, memoized_version,
};
use crate::pane::PaneRef;
use crate::sidebar::timing::unix_now_ms;
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
struct RawTab {
    name: String,
    #[serde(default)]
    selectable_tiled_panes_count: u64,
}

#[derive(Debug, Deserialize)]
struct RawListedPane {
    id: u64,
    #[serde(default)]
    is_plugin: bool,
    #[serde(default)]
    is_held: bool,
    #[serde(default)]
    exited: bool,
    #[serde(default)]
    is_suppressed: bool,
    #[serde(default)]
    is_floating: bool,
    #[serde(default)]
    is_focused: bool,
    #[serde(alias = "tab_id")]
    tab_position: u64,
    #[serde(default)]
    tab_name: Option<String>,
    #[serde(default)]
    pane_columns: Option<u64>,
    #[serde(default)]
    pane_x: Option<u64>,
    #[serde(default)]
    title: Option<String>,
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
            is_focused: pane.is_focused,
            tab_position: pane.tab_position,
            tab_name: pane.tab_name,
            pane_columns: pane.pane_columns,
            pane_x: pane.pane_x,
            title: pane.title,
            pane_command: None,
            pane_cwd: None,
            terminal_command: pane.terminal_command,
        }
    }
}

fn merge_topology_enrichment(cache: &mut PaneTopologyCache, prior: PaneTopologyCache) {
    let enrichment = prior
        .panes
        .into_iter()
        .map(|pane| (pane.id, (pane.pane_command, pane.pane_cwd)))
        .collect::<HashMap<_, _>>();
    for pane in &mut cache.panes {
        let Some((command, cwd)) = enrichment.get(&pane.id) else {
            continue;
        };
        if pane.pane_command.is_none() {
            pane.pane_command.clone_from(command);
        }
        if pane.pane_cwd.is_none() {
            pane.pane_cwd.clone_from(cwd);
        }
    }
}

impl ZellijBackend {
    pub(super) fn authoritative_pane_listing(
        &self,
        session_name: &str,
        runtime_paths: Option<&RuntimePaths>,
        workspace_id: Option<&WorkspaceId>,
        timeout: Duration,
    ) -> Result<RawPaneListing> {
        let observed_at_ms = crate::sidebar::timing::unix_now_ms();
        let output = self
            .zellij_action(session_name)
            .args(["list-panes", "--all", "--json"])
            .run_with_timeout(timeout)?;
        let listed: Vec<RawListedPane> =
            serde_json::from_slice(&output.stdout).map_err(|err| MuxErr::Output {
                program: "zellij".to_owned(),
                reason: format!("parsing `list-panes --all --json`: {err}"),
            })?;
        let focused_pane = listed
            .iter()
            .find(|pane| pane.is_focused && !pane.is_plugin)
            .map(|pane| pane.id);
        let mut cache = PaneTopologyCache {
            session_name: session_name.to_owned(),
            produced_at_ms: observed_at_ms,
            writer: None,
            focused_pane,
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
        Ok(RawPaneListing::from_topology(cache))
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
        let pane = self
            .client_view(ClientFocusOptions {
                session_name: Some(session_name.to_owned()),
                command_timeout: None,
            })
            .map(|view| view.viewed_panes)
            .ok()?
            .pop()?;
        let floor_ms = crate::sidebar::timing::unix_now_ms();
        let raw_id = parse_zellij_raw(&pane)?;
        let tab_position = self
            .topology_panes_for_workspace(
                session_name,
                workspace_id,
                Some(floor_ms),
                super::super::COMMAND_TIMEOUT,
            )
            .ok()?
            .into_iter()
            .find(|candidate| candidate.is_terminal() && candidate.id == raw_id)
            .map(|candidate| candidate.tab_position)?;
        Some(FocusRestoreTarget { pane, tab_position })
    }

    fn restore_attached_client_focus(
        &self,
        session_name: &str,
        restore: &FocusRestoreTarget,
    ) -> Result<()> {
        let deadline = Instant::now() + super::FOCUS_RESTORE_CONFIRM_WINDOW;
        loop {
            let last_error = match self
                .go_to_tab_position(session_name, restore.tab_position)
                .and_then(|_| self.focus_pane(&restore.pane, Some(session_name)))
            {
                Ok(()) => match self
                    .client_view(ClientFocusOptions {
                        session_name: Some(session_name.to_owned()),
                        command_timeout: None,
                    })
                    .map(|view| view.viewed_panes)
                {
                    Ok(focused) if focused.iter().any(|pane| pane == &restore.pane) => {
                        return Ok(());
                    }
                    Ok(focused) => {
                        format!("focused panes were {focused:?}")
                    }
                    Err(err) => err.to_string(),
                },
                Err(err) => err.to_string(),
            };
            if Instant::now() >= deadline {
                return Err(MuxErr::Output {
                    program: "zellij".to_owned(),
                    reason: format!(
                        "restoring focus to {} in tab {} did not settle: {last_error}",
                        restore.pane, restore.tab_position,
                    ),
                });
            }
            std::thread::sleep(super::FOCUS_RESTORE_CONFIRM_STEP);
        }
    }

    fn run_new_tab_confirmed(&self, session: &str, args: &[String], tab_name: &str) -> Result<()> {
        let before = self.named_tab_count(session, tab_name)?;
        let before_materialized = self.named_materialized_tab_count(session, tab_name)?;
        for attempt in 0..super::NEW_TAB_ATTEMPTS {
            if attempt > 0 && self.named_tab_count(session, tab_name)? > before {
                self.wait_for_named_tab_materialized(session, tab_name, before_materialized)?;
                return Ok(());
            }
            self.zellij_action(session)
                .args(args.iter().cloned())
                .run()?;
            let deadline = Instant::now() + super::NEW_TAB_CONFIRM_WINDOW;
            loop {
                if self.named_tab_count(session, tab_name)? > before {
                    self.wait_for_named_tab_materialized(session, tab_name, before_materialized)?;
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

    fn named_tab_count(&self, session: &str, tab_name: &str) -> Result<usize> {
        Ok(self
            .tab_names(session)?
            .iter()
            .filter(|name| name.as_str() == tab_name)
            .count())
    }

    fn wait_for_named_tab_materialized(
        &self,
        session: &str,
        tab_name: &str,
        before_materialized: usize,
    ) -> Result<()> {
        let deadline = Instant::now() + super::NEW_TAB_MATERIALIZE_WINDOW;
        loop {
            let last_count = self.named_materialized_tab_count(session, tab_name)?;
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
        }
    }

    fn named_materialized_tab_count(&self, session: &str, tab_name: &str) -> Result<usize> {
        Ok(self
            .list_tabs(session)?
            .into_iter()
            .filter(|tab| tab.name == tab_name && tab.selectable_tiled_panes_count > 0)
            .count())
    }

    fn list_tabs(&self, session: &str) -> Result<Vec<RawTab>> {
        for attempt in 0..super::TAB_NAMES_ATTEMPTS {
            if attempt > 0 {
                std::thread::sleep(super::TAB_NAMES_RETRY_DELAY);
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
            return serde_json::from_slice::<Vec<RawTab>>(&output.stdout).map_err(|e| {
                MuxErr::Output {
                    program: "zellij".to_owned(),
                    reason: format!("parsing list-tabs JSON: {e}"),
                }
            });
        }
        Err(MuxErr::Output {
            program: "zellij".to_owned(),
            reason: format!(
                "list-tabs returned no output after {} attempts",
                super::TAB_NAMES_ATTEMPTS
            ),
        })
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

    fn list_panes(&self, opts: PaneListOptions) -> Result<PaneListing> {
        let timeout = opts
            .command_timeout
            .unwrap_or(super::super::COMMAND_TIMEOUT);
        let session_name = opts.session_name.unwrap_or_default();
        let raws = if opts.authoritative && !session_name.is_empty() {
            match self.authoritative_pane_listing(
                &session_name,
                opts.runtime_paths.as_ref(),
                opts.workspace_id.as_ref(),
                timeout,
            ) {
                Ok(listing) => listing,
                Err(err) => {
                    tracing::debug!(session = %session_name, error = %err, "authoritative Zellij pane listing failed; falling back to topology cache");
                    self.topology_listing(
                        Some(&session_name),
                        opts.runtime_paths.as_ref(),
                        opts.workspace_id.as_ref(),
                        opts.min_topology_produced_at_ms,
                        timeout,
                    )?
                }
            }
        } else {
            self.topology_listing(
                (!session_name.is_empty()).then_some(session_name.as_str()),
                opts.runtime_paths.as_ref(),
                opts.workspace_id.as_ref(),
                opts.min_topology_produced_at_ms,
                timeout,
            )?
        };
        Ok(raws.into_pane_listing(session_name, |mut p, session_name| {
            if !p.is_listed_pane() {
                return None;
            }
            let command = p.display_command();
            Some(PaneRef {
                pane_id: PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", p.id)),
                session_name: session_name.to_owned(),
                view_id: Some(format!("tab_{}", p.view_position())),
                view_kind: Some(crate::mux::view_kind(MuxName::Zellij)),
                view_name: p.tab_name.take(),
                is_focused: p.is_focused,
                is_floating: p.is_floating,
                pane_pid: None,
                pane_process_start: None,
                hosted_agent_kind: None,
                hosted_agent_process_start: None,
                command,
                spawn_command: p.spawn_command().map(str::to_owned),
                cwd: p.pane_cwd.take(),
                resumed_session_id: None,
                elevated_agent: None,
                first_seen_at_ms: None,
                // Zellij topology exposes no per-pane "tab is active" or
                // "session attached" signal, so pane visibility is unknown here.
                // `None` makes the renderer's visibility gate fall back to
                // always painting — the deliberate cross-backend floor.
            })
        }))
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
        let viewed_panes = parse_focused_client_panes(&output.stdout);
        let human_clients = parse_focused_terminal_client_ids(&output.stdout).len();
        Ok(ClientView {
            viewed_panes,
            presence: ClientPresence {
                human_clients,
                last_input_ms: None,
            },
        })
    }

    fn split_pane(&self, opts: SplitPaneOptions) -> Result<()> {
        let target = opts.target_pane_id;
        if let Some(target) = &target {
            ensure_pane_backend(target, MuxName::Zellij)?;
        }
        let mut spec = match opts.session_name.as_deref() {
            Some(session) => self.zellij_action(session).arg("new-pane"),
            None => self.cmd().args(["action", "new-pane"]),
        };
        if opts.stacked {
            spec = spec.arg("--stacked");
        } else {
            let direction = match opts.direction {
                SplitDirection::Right => "right",
                SplitDirection::Down => "down",
            };
            spec = spec.args(["--direction", direction]);
        }
        if let Some(target) = &target {
            let pane_id = target
                .creation_ordinal()
                .map(|id| id.to_string())
                .unwrap_or_else(|| target.raw().to_owned());
            spec = spec.env("ZELLIJ_PANE_ID", pane_id);
        }
        if let Some(tab_id) = opts.target_view_id.as_deref().and_then(zellij_numeric_id) {
            spec = spec.args(["--tab-id".to_owned(), tab_id.to_string()]);
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
        // `new-pane` focuses the pane it creates; for the `--bg` path
        // return focus to the splitting pane (best-effort — the pane is open).
        if !opts.focus
            && let Some(target) = &target
        {
            let _ = self.focus_pane(target, opts.session_name.as_deref());
        }
        Ok(())
    }

    fn focus_pane(&self, pane: &PaneId, session: Option<&str>) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Zellij)?;
        // Zellij 0.41+: `focus-pane-id <raw>`. The earlier `focus-pane-with-id`
        // name was removed; the stub that referenced it never reached a
        // running binary.
        let spec = match session {
            Some(session) => self.zellij_action(session).arg("focus-pane-id"),
            None => self.cmd().args(["action", "focus-pane-id"]),
        };
        spec.arg(pane.raw()).run().map(|_| ())
    }

    fn resize_sidebar_width(&self, session: &str, pane: &PaneId, dir: WidthAdjust) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Zellij)?;
        let direction = match dir {
            WidthAdjust::Narrower => "decrease",
            WidthAdjust::Wider => "increase",
        };
        self.resize_sidebar_step(session, pane.raw(), direction)
    }

    fn converge_sidebar_widths(&self, opts: &WidthSyncOptions) -> Result<usize> {
        if !self.session_has_attached_client(&opts.session_name) {
            return Ok(0);
        }
        // Width repair follows local Zellij mutations closely. A topology-cache
        // writer can stamp an older observation after the mutation has begun,
        // so take the first verdict from Zellij itself and keep the cache only
        // as a fallback for environments where the authoritative action fails.
        let listing = self
            .authoritative_pane_listing(
                &opts.session_name,
                None,
                Some(&opts.workspace_id),
                crate::sidebar::timing::RECONCILE_LIST_TIMEOUT,
            )
            .or_else(|err| {
                tracing::debug!(
                    session = %opts.session_name,
                    error = &err as &dyn std::error::Error,
                    "authoritative Zellij width listing failed; falling back to topology cache",
                );
                self.topology_listing(
                    Some(&opts.session_name),
                    None,
                    Some(&opts.workspace_id),
                    Some(unix_now_ms()),
                    crate::sidebar::timing::RECONCILE_LIST_TIMEOUT,
                )
            })?;
        // Keep the off-spec verdict and per-pane direction latch on this same
        // post-trigger snapshot (or newer), never an ambient cache entry.
        let floor = Some(listing.observed_at_ms);
        let panes = listing.panes;
        let mut sidebars: HashMap<u64, Vec<u64>> = HashMap::new();
        for pane in panes
            .iter()
            .filter(|pane| pane.is_live_terminal() && is_sidebar_pane(pane))
        {
            sidebars.entry(pane.tab_position).or_default().push(pane.id);
        }
        let mut resized = 0;
        for (tab_position, pane_ids) in sidebars {
            let [raw_id] = pane_ids.as_slice() else {
                continue;
            };
            if !panes.iter().any(|pane| {
                pane.tab_position == tab_position
                    && pane.is_live_terminal()
                    && !is_sidebar_pane(pane)
            }) {
                continue;
            }
            let Some(pane) = panes.iter().find(|pane| pane.id == *raw_id) else {
                continue;
            };
            let Some((cols, view_cols)) =
                pane.pane_columns.zip(tab_view_cols(&panes, tab_position))
            else {
                continue;
            };
            let target = live_target_cols(opts.width, opts.width_override, view_cols);
            if !sidebar_width_off_spec(cols, target, view_cols) {
                continue;
            }
            let (_, changed) = self.converge_sidebar_width(opts, tab_position, *raw_id, floor);
            resized += usize::from(changed);
        }
        Ok(resized)
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

    fn send_key(&self, pane: &PaneId, key: NamedKey) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Zellij)?;
        let bytes = key.write_bytes().iter().map(u8::to_string);
        self.cmd()
            .args(["action", "write", "--pane-id", pane.raw()])
            .args(bytes)
            .run()
            .map(|_| ())
    }

    fn paste_text(&self, pane: &PaneId, text: &str) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Zellij)?;
        // Open marker + text + close marker as one decimal byte list, mirroring
        // the existing `send_key` byte-write mechanism. Raw bytes pass through
        // untouched, so a marker-looking byte inside `text` is never re-parsed.
        let bytes = BRACKET_PASTE_OPEN
            .bytes()
            .chain(text.bytes())
            .chain(BRACKET_PASTE_CLOSE.bytes())
            .map(|byte| byte.to_string());
        self.cmd()
            .args(["action", "write", "--pane-id", pane.raw()])
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
            SessionState::Absent => self.create_session_with_sidebar(opts, daemon),
            SessionState::Exited => {
                self.delete_session(&opts.session_name)?;
                self.create_session_with_sidebar(opts, daemon)
            }
            SessionState::Live => {
                match self.session_cleanliness(&opts.session_name, &opts.workspace_id) {
                    Ok(SessionCleanliness::Clean) if !opts.replace_existing => Ok(()),
                    Ok(_) => {
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
            SessionState::Absent => SessionHealth::Healthy,
            // `attach --create` would resurrect a serialized, suspended layout.
            SessionState::Exited => SessionHealth::Stuck,
            // `list-sessions` liveness is the attach gate truth; attach live rooms as-is.
            SessionState::Live => SessionHealth::Healthy,
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
        if matches!(state, SessionState::Live) {
            return Ok(SessionHealth::Healthy);
        }
        // Absent → first birth; Exited → delete and rebirth from the layout so
        // the room comes up clean and RUNNING (with serialization off, a rebirth
        // can never resurrect). A rebirth that still fails to talk to Zellij
        // reads as Stuck so the caller runs or reports the reset path.
        let rebirth = || -> Result<()> {
            if !matches!(state, SessionState::Absent) {
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
        super::super::recovery::purge_zellij_session_cache_in(
            &crate::store::paths::cache_home(),
            name,
        )
    }

    fn resurrection_cache_paths(&self, name: &str) -> Vec<PathBuf> {
        super::super::recovery::zellij_session_cache_paths_in(
            &crate::store::paths::cache_home(),
            name,
        )
    }

    fn reconcile_sidebars(
        &self,
        opts: &SidebarPaneOptions,
        live: &SidebarLiveness,
    ) -> Result<SidebarRecovery> {
        // Zellij docks the sidebar left only at session birth, but a left pane
        // can still be reached in a live session: close a stray sidebar by id,
        // and add one by splitting right, moving it left, and resizing it to the
        // tab's live target. This never rebirths the session, so working panes
        // survive.
        let listing = self.topology_listing(
            Some(&opts.session_name),
            None,
            Some(&opts.workspace_id),
            None,
            crate::sidebar::timing::RECONCILE_LIST_TIMEOUT,
        )?;
        let panes = listing.panes;
        let views = views_with_sidebars(&panes);
        let plan = super::super::plan_reconcile(&views, live);
        // Kept sidebars (not planned for closing) whose geometry sits off the
        // layout's dock — the residue of a mis-mounted add — converge in place
        // this pass, renderer untouched.
        let off_spec = off_spec_sidebars(&panes, &plan.close, opts.width, opts.width_override);
        if plan.close.is_empty() && plan.add.is_empty() && off_spec.is_empty() {
            return Ok(SidebarRecovery::default());
        }

        // Adding (and closing) a pane shifts focus, so remember each tab's
        // focused (working) pane to restore afterwards, and the user's own
        // invoking pane to return the visible tab to where they ran `rimz reload`.
        let focused_in_tab = focused_work_panes(&panes);

        let mut report = SidebarRecovery::default();
        // Close duplicate / unresponsive sidebar panes first, so a view that lost
        // its only live sidebar reads as missing and gains exactly one fresh one.
        let failed_stale_close_views = execute_closes(&plan, live, &mut report, |pane| match self
            .close_pane(&opts.session_name, pane)
        {
            Ok(()) => true,
            Err(err) => {
                tracing::warn!(
                    session = %opts.session_name,
                    pane = %pane.as_str(),
                    tags.operation = "zellij.reconcile.close_stray",
                    error = &err as &dyn std::error::Error,
                    "sidebar reconcile: closing a stray sidebar pane failed; leaving it",
                );
                false
            }
        });
        // In-place adds and geometry moves both need an attached client: a
        // detached session's screen thread drops the mount while the spawned
        // serve pair keeps running, so adding there only leaks (the closes
        // above are safe detached). An unanswerable probe reads detached —
        // deferring one run is recoverable, a leaked pair is not. tmux splits
        // fine detached, so the gate is Zellij-internal.
        let needs_attached = !plan.add.is_empty() || !off_spec.is_empty();
        let attached = !needs_attached || self.session_has_attached_client(&opts.session_name);
        if attached {
            for (tab_position, raw_id) in &off_spec {
                repair_sidebar_geometry(
                    self,
                    opts,
                    *tab_position,
                    *raw_id,
                    &focused_in_tab,
                    &mut report,
                );
            }
        }
        if needs_attached && !attached {
            report.deferred = plan.add.len() + off_spec.len();
            tracing::info!(
                session = %opts.session_name,
                deferred = report.deferred,
                "sidebar reconcile: no attached client; deferring in-place adds and geometry repairs",
            );
        } else {
            let mut tabs_with_sidebar =
                existing_sidebar_tabs(self, &opts.session_name, &opts.workspace_id, &plan.add);
            execute_adds(
                &plan,
                &failed_stale_close_views,
                &mut report,
                |tab, _restart| {
                    let Ok(tab_position) = tab.parse::<u64>() else {
                        return AddOutcome::Failed;
                    };
                    let Some(occupied_tabs) = tabs_with_sidebar.as_mut() else {
                        return AddOutcome::Failed;
                    };
                    if occupied_tabs.contains(tab) {
                        tracing::warn!(
                            session = %opts.session_name,
                            tab = tab_position,
                            tags.operation = "zellij.reconcile.add_skipped",
                            "sidebar reconcile: add skipped because the tab still has a sidebar",
                        );
                        return AddOutcome::Failed;
                    }
                    match self.add_sidebar_to_tab(opts, tab_position) {
                        Ok(outcome) => {
                            occupied_tabs.insert(tab_position.to_string());
                            restore_tab_focus(
                                self,
                                &opts.session_name,
                                &opts.workspace_id,
                                tab_position,
                                &focused_in_tab,
                            );
                            match outcome {
                                DockOutcome::Docked => AddOutcome::Added,
                                DockOutcome::Misdocked => AddOutcome::AddedMisdocked,
                            }
                        }
                        Err(err) => {
                            tracing::warn!(
                                session = %opts.session_name,
                                tab = tab_position,
                                tags.operation = "zellij.reconcile.add",
                                error = &err as &dyn std::error::Error,
                                "sidebar reconcile: in-place add failed; leaving the tab without a sidebar",
                            );
                            AddOutcome::Failed
                        }
                    }
                },
            );
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
        if let Err(err) = self.go_to_tab(session, 1) {
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
        // and restore both after the tab exists, falling back to the lead tab
        // only for detached sessions where there is no client focus to restore.
        let restore = (!opts.focus)
            .then(|| self.focus_restore_target(&opts.session_name, &opts.sidebar.workspace_id))
            .flatten();
        let sidebar_percent = (|| {
            let panes = self
                .topology_panes(
                    &opts.session_name,
                    None,
                    crate::sidebar::timing::RECONCILE_LIST_TIMEOUT,
                )
                .ok()?;
            let tab = panes
                .iter()
                .find(|pane| pane.is_live_terminal())?
                .tab_position;
            let view_cols = tab_view_cols(&panes, tab)?;
            let view_cols = u16::try_from(view_cols).ok()?;
            Some(
                opts.sidebar
                    .width
                    .birth_size_with_override(Some(view_cols), opts.sidebar.width_override)
                    .percent,
            )
        })()
        .unwrap_or_else(|| opts.sidebar.width.percent.clamp(10, 90));
        let layout = TempLayoutFile::new(render_tab_layout(opts, sidebar_percent)?)?;
        let args = [
            "new-tab".to_owned(),
            "--layout".to_owned(),
            layout.path().to_string_lossy().into_owned(),
            "--name".to_owned(),
            opts.title.clone(),
        ];
        self.run_new_tab_confirmed(&opts.session_name, &args, &opts.title)?;
        drop(layout);
        if !opts.focus {
            let result = match &restore {
                Some(restore) => self.restore_attached_client_focus(&opts.session_name, restore),
                None => self.go_to_tab(&opts.session_name, 1),
            };
            if let Err(err) = result {
                tracing::warn!(
                    session = %opts.session_name,
                    tags.operation = "zellij.focus_tab",
                    error = &err as &dyn std::error::Error,
                    "could not return focus after opening an unfocused tab",
                );
            }
        }
        Ok(())
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

    fn share_web_session(&self, opts: &super::super::PresencePluginOptions) -> Result<()> {
        self.share_web_session_for(opts)
    }

    fn version(&self) -> Result<String> {
        memoized_version(&self.version, &self.cmd().arg("--version"))
    }
}

fn off_spec_sidebars(
    panes: &[RawPane],
    closing: &[PaneId],
    width: crate::mux::SidebarWidth,
    width_override: Option<std::num::NonZeroU16>,
) -> Vec<(u64, u64)> {
    let closing: HashSet<u64> = closing.iter().filter_map(parse_zellij_raw).collect();
    panes
        .iter()
        .filter(|pane| pane.is_live_terminal() && is_sidebar_pane(pane))
        .filter(|pane| !closing.contains(&pane.id))
        .filter(|pane| sidebar_geometry_off_spec(pane, panes, &closing, width, width_override))
        .map(|pane| (pane.tab_position, pane.id))
        .collect()
}

fn parse_zellij_raw(pane: &PaneId) -> Option<u64> {
    (pane.mux() == MuxName::Zellij)
        .then(|| pane.raw().strip_prefix("terminal_")?.parse().ok())
        .flatten()
}

fn focused_work_panes(panes: &[RawPane]) -> HashMap<u64, u64> {
    let mut focused = HashMap::new();
    for pane in panes
        .iter()
        .filter(|pane| pane.is_focused && !pane.is_plugin)
    {
        focused.entry(pane.tab_position).or_insert(pane.id);
    }
    focused
}

fn repair_sidebar_geometry(
    backend: &ZellijBackend,
    opts: &SidebarPaneOptions,
    tab_position: u64,
    raw_id: u64,
    focused_in_tab: &HashMap<u64, u64>,
    report: &mut SidebarRecovery,
) {
    let floor = backend.converge_sidebar_geometry(opts, tab_position, raw_id);
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
                restore_tab_focus(
                    backend,
                    &opts.session_name,
                    &opts.workspace_id,
                    tab_position,
                    focused_in_tab,
                );
            }
        }
        DockOutcome::Misdocked => {
            if repairable_nested_sidebar_remains(
                backend,
                &opts.session_name,
                &opts.workspace_id,
                tab_position,
                raw_id,
                floor,
            ) {
                rebuild_misdocked_sidebar(
                    backend,
                    opts,
                    tab_position,
                    raw_id,
                    focused_in_tab,
                    report,
                );
            } else {
                report.misdocked += 1;
            }
        }
    }
}

fn repairable_nested_sidebar_remains(
    backend: &ZellijBackend,
    session_name: &str,
    workspace_id: &WorkspaceId,
    tab_position: u64,
    raw_id: u64,
    min_topology_produced_at_ms: Option<u64>,
) -> bool {
    let Ok(panes) = backend.topology_panes_for_workspace(
        session_name,
        workspace_id,
        min_topology_produced_at_ms,
        crate::sidebar::timing::RECONCILE_LIST_TIMEOUT,
    ) else {
        return false;
    };
    let Some(sidebar) = panes
        .iter()
        .find(|pane| pane.is_terminal() && pane.tab_position == tab_position && pane.id == raw_id)
    else {
        return false;
    };
    let excluded = HashSet::new();
    sidebar_dock_verdict(sidebar, &panes, &excluded) == Some(SidebarDock::NestedRow)
        && repairable_nested_work_pane_ids(sidebar, &panes, &excluded).is_some()
}

fn rebuild_misdocked_sidebar(
    backend: &ZellijBackend,
    opts: &SidebarPaneOptions,
    tab_position: u64,
    raw_id: u64,
    focused_in_tab: &HashMap<u64, u64>,
    report: &mut SidebarRecovery,
) {
    let pane = PaneId::from_parts(MuxName::Zellij, format!("terminal_{raw_id}"));
    if let Err(err) = backend.close_pane(&opts.session_name, &pane) {
        tracing::warn!(
            session = %opts.session_name,
            tab = tab_position,
            pane = %pane.as_str(),
            tags.operation = "zellij.reconcile.close",
            error = &err as &dyn std::error::Error,
            "sidebar reconcile: closing a nested sidebar for rebuild failed; leaving it",
        );
        report.failed += 1;
        return;
    }
    match backend.add_sidebar_to_tab(opts, tab_position) {
        Ok(DockOutcome::Docked) => {
            report.redocked += 1;
            restore_tab_focus(
                backend,
                &opts.session_name,
                &opts.workspace_id,
                tab_position,
                focused_in_tab,
            );
        }
        Ok(DockOutcome::Misdocked) => {
            report.misdocked += 1;
            restore_tab_focus(
                backend,
                &opts.session_name,
                &opts.workspace_id,
                tab_position,
                focused_in_tab,
            );
        }
        Err(err) => {
            tracing::warn!(
                session = %opts.session_name,
                tab = tab_position,
                tags.operation = "zellij.reconcile.rebuild",
                error = &err as &dyn std::error::Error,
                "sidebar reconcile: rebuilding a nested sidebar failed",
            );
            report.failed += 1;
        }
    }
}

fn existing_sidebar_tabs(
    backend: &ZellijBackend,
    session_name: &str,
    workspace_id: &WorkspaceId,
    add: &[String],
) -> Option<HashSet<String>> {
    if add.is_empty() {
        return Some(HashSet::new());
    }
    match backend.topology_panes_for_workspace(
        session_name,
        workspace_id,
        None,
        crate::sidebar::timing::RECONCILE_LIST_TIMEOUT,
    ) {
        Ok(panes) => Some(tabs_with_sidebars(&panes)),
        Err(err) => {
            tracing::warn!(
                session = %session_name,
                tags.operation = "zellij.reconcile.verify",
                error = &err as &dyn std::error::Error,
                "sidebar reconcile: cannot verify sidebar absence before add; skipping adds",
            );
            None
        }
    }
}

fn restore_tab_focus(
    backend: &ZellijBackend,
    session_name: &str,
    workspace_id: &WorkspaceId,
    tab_position: u64,
    focused_in_tab: &HashMap<u64, u64>,
) {
    const ATTEMPTS: u32 = 5;
    const CYCLE_STEPS: u32 = 8;
    const RETRY_DELAY: Duration = Duration::from_millis(50);

    let Some(work) = focused_in_tab.get(&tab_position).copied() else {
        return;
    };
    for attempt in 0..ATTEMPTS {
        let _ = backend.focus_terminal(session_name, work);
        if tab_focus_is(backend, session_name, workspace_id, tab_position, work) {
            return;
        }
        if attempt + 1 < ATTEMPTS {
            std::thread::sleep(RETRY_DELAY);
        }
    }
    for action in ["focus-previous-pane", "focus-next-pane"] {
        for _ in 0..CYCLE_STEPS {
            let _ = backend.zellij_action(session_name).arg(action).run();
            std::thread::sleep(RETRY_DELAY);
            if tab_focus_is(backend, session_name, workspace_id, tab_position, work) {
                return;
            }
        }
    }
}

fn zellij_numeric_id(raw: &str) -> Option<u64> {
    raw.rsplit_once('_')
        .and_then(|(_, tail)| tail.parse::<u64>().ok())
}

fn tab_focus_is(
    backend: &ZellijBackend,
    session_name: &str,
    workspace_id: &WorkspaceId,
    tab_position: u64,
    raw_id: u64,
) -> bool {
    backend
        .topology_panes_for_workspace(
            session_name,
            workspace_id,
            None,
            crate::sidebar::timing::RECONCILE_LIST_TIMEOUT,
        )
        .is_ok_and(|panes| {
            panes.iter().any(|pane| {
                pane.is_terminal()
                    && pane.tab_position == tab_position
                    && pane.id == raw_id
                    && pane.is_focused
            })
        })
}
