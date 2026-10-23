//! tmux window, pane, and tab-layout command helpers.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use crate::ids::{MuxName, PaneId};
use crate::mux::{CommandSpec, HostPane, MuxErr, Result, SidebarPaneOptions, ensure_pane_backend};
use crate::mux::{PaneRect, ViewGeometry};

use super::TmuxBackend;
use super::options::{
    RIMZ_TAB_BASE_OPTION, RIMZ_TAB_FOUNDERS_OPTION, RIMZ_TITLE_OPTION, sidebar_serve_command,
    spawn_command_is_sidebar_serve, window_option_argv,
};
use super::parse::parse_new_window_ids;

/// Window options that carry a compare-and-set rename's two names into one
/// tmux command list; see [`TmuxBackend::window_name_guarded_command`].
const RENAME_OBSERVED_OPTION: &str = "@rimz_rename_observed";
const RENAME_TARGET_OPTION: &str = "@rimz_rename_target";

// A host's ignore-size control client must not turn a detached room into a
// client-sized one. Client loops retain the target session's format context.
const SIZING_CLIENT: &str = "#{!=:#{L:#{?#{&&:#{==:#{client_session},#{session_name}},#{==:#{m:*ignore-size*,#{client_flags}},0}},1,}},}";
const WINDOW_GEOMETRY: &str = "#{window_id} #{window_active} #{window_width} #{window_height} #{window_stack_index} #{window-size}";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TmuxPaneGeometry {
    pub(super) pane_id: String,
    pub(super) window_id: String,
    pub(super) pane_width: u64,
    pub(super) window_width: u64,
    pub(super) is_sidebar: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TmuxPaneCell {
    pane_id: String,
    left: u64,
    width: u64,
    top: u64,
    height: u64,
}

struct NativePaneRect {
    rect: PaneRect,
    stack: String,
    zoomed: bool,
    floating: bool,
    chrome: bool,
}

fn companion_stack_target(panes: &[NativePaneRect]) -> Option<PaneId> {
    let work = panes
        .iter()
        .filter(|pane| !pane.floating && !pane.chrome)
        .collect::<Vec<_>>();
    let first = work.first()?;
    if panes.iter().any(|pane| !pane.floating && pane.zoomed)
        || (work.len() > 1
            && (first.stack.is_empty() || work.iter().any(|pane| pane.stack != first.stack)))
    {
        return None;
    }
    let top = work.iter().map(|pane| pane.rect.y).min()?;
    let bottom = work.iter().map(|pane| pane.rect.y + pane.rect.rows).max()?;
    let open_rows = (bottom - top).saturating_sub(2 * work.len() as u64 + 1);
    if open_rows < crate::mux::COMPANION_STACK_MIN_CONTENT_ROWS {
        return None;
    }
    work.into_iter()
        .max_by_key(|pane| pane.rect.y)
        .map(|pane| pane.rect.pane_id.clone())
}

pub(super) struct OpenedWindow {
    pub(super) window_id: String,
    pub(super) first_pane: String,
}

pub(super) fn equal_row_split_size(pane_count: usize, split_index: usize) -> String {
    debug_assert!(pane_count >= 2 && (1..pane_count).contains(&split_index));
    let remaining = pane_count - split_index;
    format!("{}%", 100 * remaining / (remaining + 1))
}

pub(super) fn even_column_heights(total_height: u64, pane_count: usize) -> Vec<u64> {
    debug_assert!(pane_count > 0 && total_height >= pane_count as u64);
    let separators = pane_count.saturating_sub(1) as u64;
    let available = total_height.saturating_sub(separators);
    let pane_count = pane_count as u64;
    let base = available / pane_count;
    let remainder = available % pane_count;
    (0..pane_count)
        .map(|index| base + u64::from(index >= pane_count - remainder))
        .collect()
}

/// The `-l` sizes for a run of `pane_count - 1` splits that each target the
/// pane the previous split created, so the run leaves `pane_count` even panes
/// (one cell of rounding) across `total`. Split `k` hands the new pane every
/// cell the panes after it still need, separators included.
pub(super) fn even_split_sizes(total: u64, pane_count: usize) -> Vec<u64> {
    if pane_count < 2 {
        return Vec::new();
    }
    // An extent too small for its panes stays tmux's business: it clamps an
    // oversize `-l` and errors only once no space is left. Clamping `total`
    // here only keeps `even_column_heights`'s debug assertion satisfied.
    let cells = even_column_heights(total.max(pane_count as u64), pane_count);
    (1..pane_count)
        .map(|index| cells[index..].iter().sum::<u64>() + (pane_count - 1 - index) as u64)
        .collect()
}

/// The axis a run of layout splits divides.
#[derive(Clone, Copy)]
pub(super) enum SplitAxis {
    /// Side-by-side columns, dividing the anchor's width.
    Columns,
    /// Stacked rows, dividing the anchor's height.
    Rows,
}

impl SplitAxis {
    fn direction(self) -> &'static str {
        match self {
            Self::Columns => "-h",
            Self::Rows => "-v",
        }
    }

    fn extent_format(self) -> &'static str {
        match self {
            Self::Columns => "#{pane_width}",
            Self::Rows => "#{pane_height}",
        }
    }
}

/// A tmux window name with its reserved separators neutralized. tmux parses a
/// colon as the `session:window` boundary and a dot as the `window.pane`
/// boundary in a target spec, so a name carrying either cannot be targeted by
/// name (`lead_window`), and tmux 3.7 rejects it outright (`invalid window
/// name: run: codex`; 3.7a accepts it again). Channel labels and run-pane
/// titles are human text that can carry both, so map each to a dash before the
/// name reaches tmux; Zellij tab names have no such constraint, so the mapping
/// stays inside this backend.
pub(super) fn sanitize_window_name(raw: &str) -> String {
    crate::mux::tab_name::tmux_window_name(raw)
}

/// The sanitized stored name encoded for a tmux name argument. tmux expands
/// formats such as `#h` in `new-window -n` and `rename-window`; `##` writes one
/// literal hash without changing the name read back from tmux.
pub(super) fn window_name_arg(raw: &str) -> String {
    sanitize_window_name(raw).replace('#', "##")
}

impl TmuxBackend {
    pub(super) fn read_lagging_views(
        &self,
        session: &str,
    ) -> Result<Option<crate::mux::LaggingViews>> {
        let format = format!("{WINDOW_GEOMETRY} {SIZING_CLIENT}");
        let output = self
            .cmd()
            .args(["list-windows", "-t", session, "-F", &format])
            .run()?;
        select_lagging_views(&String::from_utf8_lossy(&output.stdout))
    }

    pub(super) fn read_lagging_view(
        &self,
        session: &str,
        pane: &PaneId,
    ) -> Result<Option<crate::mux::LaggingView>> {
        ensure_pane_backend(pane, MuxName::Tmux)?;
        let format = format!("#{{session_id}} {WINDOW_GEOMETRY} {SIZING_CLIENT}");
        let output = self
            .batch_spec(&[session, pane.raw()].map(|target| {
                ["display-message", "-p", "-t", target, &format]
                    .map(str::to_owned)
                    .into()
            }))
            .run()?;
        let text = String::from_utf8_lossy(&output.stdout);
        let mut lines = text.lines();
        let (source_session, current) = lines
            .next()
            .and_then(|line| line.split_once(' '))
            .ok_or_else(geometry_error)?;
        let (target_session, target) = lines
            .next()
            .and_then(|line| line.split_once(' '))
            .ok_or_else(geometry_error)?;
        if lines.next().is_some() {
            return Err(geometry_error());
        }
        if source_session != target_session {
            return Ok(None);
        }
        Ok(
            select_lagging_views(&format!("{current}\n{target}"))?.and_then(|lag| {
                lag.views
                    .into_iter()
                    .next()
                    .map(|view| crate::mux::LaggingView {
                        view,
                        target: lag.target,
                    })
            }),
        )
    }

    pub(super) fn fit_hidden_view(
        &self,
        view: &crate::ids::ViewId,
        target: crate::mux::ViewSize,
        sidebar_cols: std::num::NonZeroU16,
    ) -> Result<Option<std::collections::HashMap<PaneId, crate::mux::FittedPane>>> {
        let window = view.as_str();
        window
            .strip_prefix('@')
            .and_then(|id| id.parse::<u32>().ok())
            .ok_or_else(geometry_error)?;
        let before = self
            .batch_spec(&[
                ["show-options", "-wqv", "-t", window, "window-size"]
                    .map(str::to_owned)
                    .into(),
                [
                    "list-panes",
                    "-t",
                    window,
                    "-F",
                    "#{pane_id} #{==:#{pane_title},rimz-sidebar}",
                ]
                .map(str::to_owned)
                .into(),
            ])
            .run()?;
        let before = String::from_utf8_lossy(&before.stdout);
        let mut lines = before.lines().peekable();
        let local = match lines.peek().copied() {
            Some("latest" | "largest" | "smallest" | "manual") => lines.next().unwrap_or_default(),
            _ => "",
        };
        let mut sidebars = Vec::new();
        let mut panes = 0;
        for line in lines {
            let mut fields = line.split_whitespace();
            let pane = fields.next().ok_or_else(geometry_error)?;
            pane.strip_prefix('%')
                .and_then(|id| id.parse::<u32>().ok())
                .ok_or_else(geometry_error)?;
            match fields.next() {
                Some("1") => sidebars.push(pane),
                Some("0") => (),
                _ => return Err(geometry_error()),
            }
            if fields.next().is_some() {
                return Err(geometry_error());
            }
            panes += 1;
        }
        let sidebar_guard = "#{&&:#{==:#{window_zoomed_flag},0},#{&&:#{>:#{window_panes},1},#{==:#{P:#{?#{==:#{pane_title},rimz-sidebar},1,}},1}}}";
        let sidebar_resize = match sidebars.as_slice() {
            [pane] if panes > 1 => format!(
                "if-shell -F -t {window} '{sidebar_guard}' 'resize-pane -t {pane} -x {sidebar_cols} ; display-message -p -t {pane} \"sidebar {pane} #{{pane_width}}\"' ; "
            ),
            _ => String::new(),
        };
        let restore = match local {
            "" => format!("set-window-option -u -t {window} window-size"),
            option @ ("latest" | "largest" | "smallest" | "manual") => {
                format!("set-window-option -t {window} window-size {option}")
            }
            _ => return Err(geometry_error()),
        };
        // Selection may change while a paced queue is pending. Recheck in
        // tmux at the mutation, without another read subprocess per step.
        let guard = format!(
            "#{{&&:#{{&&:#{{==:#{{window_active}},0}},#{{!=:#{{window-size}},manual}}}},{SIZING_CLIENT}}}"
        );
        let commands = format!(
            "resize-window -t {window} -x {} -y {} ; {sidebar_resize}{restore} ; list-panes -t {window} -F '#{{pane_id}} #{{pane_height}}'",
            target.cols, target.rows
        );
        let output = self
            .batch_spec(&[[
                "if-shell",
                "-F",
                "-t",
                window,
                &guard,
                &commands,
                "display-message -p skipped",
            ]
            .map(str::to_owned)
            .into()])
            .run()?;
        parse_fitted_panes(&String::from_utf8_lossy(&output.stdout))
    }

    fn pane_rectangles(
        &self,
        anchor: &PaneId,
        timeout: std::time::Duration,
    ) -> Result<Option<Vec<NativePaneRect>>> {
        ensure_pane_backend(anchor, MuxName::Tmux)?;
        let output = match self.cmd().args([
            "list-panes", "-t", anchor.raw(), "-F",
            "#{pane_id} #{pane_left} #{pane_width} #{pane_top} #{pane_height}|#{window_zoomed_flag}|#{pane_floating_flag}|#{==:#{pane_title},rimz-sidebar}|#{pane_current_command}|#{@rimz_stack}|#{pane_start_command}",
        ]).run_with_timeout(timeout) {
            Ok(output) => output,
            Err(MuxErr::Command { stderr, .. }) if stderr.contains("can't find pane") => return Ok(None),
            Err(err) => return Err(err),
        };
        let mut panes = Vec::new();
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let fields = line.splitn(7, '|').collect::<Vec<_>>();
            let [cell, zoomed, floating, sidebar, command, stack, spawn] = fields.as_slice() else {
                return Ok(None);
            };
            let Some(cell) = parse_tmux_pane_cell(cell) else {
                return Ok(None);
            };
            if cell
                .pane_id
                .strip_prefix('%')
                .and_then(|id| id.parse::<u64>().ok())
                .is_none()
                || cell.width == 0
                || cell.height == 0
                || cell.left.checked_add(cell.width).is_none()
                || cell.top.checked_add(cell.height).is_none()
            {
                return Ok(None);
            }
            let pane = PaneRect {
                pane_id: PaneId::from_parts(MuxName::Tmux, cell.pane_id),
                x: cell.left,
                y: cell.top,
                cols: cell.width,
                rows: cell.height,
            };
            panes.push(NativePaneRect {
                rect: pane,
                stack: (*stack).to_owned(),
                zoomed: *zoomed == "1",
                floating: *floating == "1",
                chrome: *sidebar == "1"
                    || crate::pane::command_is_sidebar_chrome(command)
                    || spawn_command_is_sidebar_serve(spawn),
            });
        }
        Ok(Some(panes))
    }

    pub(super) fn read_view_geometry(
        &self,
        anchor: &PaneId,
        timeout: std::time::Duration,
    ) -> Result<Option<ViewGeometry>> {
        let Some(listed) = self.pane_rectangles(anchor, timeout)? else {
            return Ok(None);
        };
        let mut panes = Vec::new();
        for pane in listed.into_iter().filter(|pane| !pane.floating) {
            if pane.zoomed || !pane.stack.is_empty() {
                return Ok(None);
            }
            panes.push(pane.rect);
        }
        if !panes.iter().any(|pane| pane.pane_id == *anchor) {
            return Ok(None);
        }
        Ok(Some(ViewGeometry { panes, gap: 1 }))
    }

    pub(super) fn companion_append_target(&self, anchor: &PaneId) -> Result<Option<PaneId>> {
        let Some(listed) = self.pane_rectangles(anchor, std::time::Duration::from_secs(2))? else {
            return Ok(None);
        };
        Ok(companion_stack_target(&listed))
    }

    /// Attach a RimZ-owned display identity to one pane. Pane user options are
    /// not writable through terminal escape sequences, unlike `pane_title`.
    pub(super) fn set_pane_rimz_title(&self, pane_id: &str, name: &str) {
        if let Err(err) = self.set_pane_rimz_title_command(pane_id, name).run() {
            tracing::warn!(
                pane = pane_id,
                title = name,
                tags.operation = "tmux.set_pane_title",
                error = &err as &dyn std::error::Error,
                "could not set the pane's RimZ title; using its process fallback",
            );
        }
    }

    fn set_pane_rimz_title_command(&self, pane_id: &str, name: &str) -> CommandSpec {
        self.cmd().args([
            "set-option".to_owned(),
            "-p".to_owned(),
            "-t".to_owned(),
            pane_id.to_owned(),
            RIMZ_TITLE_OPTION.to_owned(),
            sanitize_window_name(name),
        ])
    }

    pub(super) fn claim_window_command(
        &self,
        anchor: &PaneId,
        name: &str,
        pane_name: &str,
    ) -> Result<CommandSpec> {
        let rename = Self::rename_window_args(anchor, name)?;
        Ok(self
            .set_pane_rimz_title_command(anchor.raw(), pane_name)
            .arg(";")
            .args(rename)
            .arg(";")
            .args(Self::window_owner_args(
                anchor.raw(),
                name,
                &[anchor.raw().to_owned()],
            )))
    }

    pub(super) fn window_owner_args(target: &str, base: &str, founders: &[String]) -> Vec<String> {
        [
            "set-option",
            "-w",
            "-t",
            target,
            RIMZ_TAB_BASE_OPTION,
            &sanitize_window_name(base),
            ";",
            "set-option",
            "-w",
            "-t",
            target,
            RIMZ_TAB_FOUNDERS_OPTION,
            &founders.join(" "),
        ]
        .into_iter()
        .map(ToOwned::to_owned)
        .collect()
    }

    pub(super) fn window_pane_ids_command(&self, anchor: &PaneId) -> Result<CommandSpec> {
        ensure_pane_backend(anchor, MuxName::Tmux)?;
        Ok(self
            .cmd()
            .args(["list-panes", "-t", anchor.raw(), "-F", "#{pane_id}"]))
    }

    /// Hand the window back to inherited naming and drop every listed pane's
    /// RimZ title pin, in one command list.
    pub(super) fn release_window_command(
        &self,
        anchor: &PaneId,
        name: &str,
        pane_ids: &[String],
    ) -> Result<CommandSpec> {
        let mut command = self.rename_window_command(anchor, name)?;
        for option in [
            "automatic-rename",
            RIMZ_TAB_BASE_OPTION,
            RIMZ_TAB_FOUNDERS_OPTION,
        ] {
            command = command.args([";", "set-option", "-wu", "-t", anchor.raw(), option]);
        }
        for pane_id in pane_ids {
            command = command.args([
                ";",
                "set-option",
                "-pu",
                "-t",
                pane_id.as_str(),
                RIMZ_TITLE_OPTION,
            ]);
        }
        Ok(command)
    }

    fn rename_window_args(anchor: &PaneId, name: &str) -> Result<Vec<String>> {
        ensure_pane_backend(anchor, MuxName::Tmux)?;
        Ok(vec![
            "rename-window".to_owned(),
            "-t".to_owned(),
            anchor.raw().to_owned(),
            window_name_arg(name),
        ])
    }

    pub(super) fn rename_window_command(&self, anchor: &PaneId, name: &str) -> Result<CommandSpec> {
        Ok(self.cmd().args(Self::rename_window_args(anchor, name)?))
    }

    pub(super) fn rebuild_window_command(
        &self,
        anchor: &PaneId,
        name: &str,
        base: &str,
    ) -> Result<CommandSpec> {
        Ok(self.rename_window_command(anchor, name)?.args([
            ";",
            "set-option",
            "-w",
            "-t",
            anchor.raw(),
            RIMZ_TAB_BASE_OPTION,
            base,
        ]))
    }

    /// Wrap `command`, a rename of `anchor`'s window to `name`, so it applies
    /// only while the window is still named `observed`. tmux runs one
    /// invocation's command list without another client's commands in between,
    /// so the check and the rename are one step. Both names travel as window
    /// options and the rename reads its name through a format, so neither is
    /// parsed as tmux command syntax; record values are quoted separately.
    pub(super) fn window_name_guarded_command(
        &self,
        anchor: &PaneId,
        observed: &str,
        name: &str,
        command: &CommandSpec,
    ) -> Result<CommandSpec> {
        let prefix = self.cmd().args.len();
        let mut guarded = Vec::new();
        for words in command.args[prefix..].split(|word| word == ";") {
            let rename = words.first().is_some_and(|verb| verb == "rename-window");
            let mut quoted = Vec::with_capacity(words.len());
            for (index, word) in words.iter().enumerate() {
                let word = if rename && index + 1 == words.len() {
                    format!("#{{{RENAME_TARGET_OPTION}}}")
                } else {
                    word.clone()
                };
                quoted.push(format!("'{}'", word.replace('\'', "'\\''")));
            }
            guarded.push(quoted.join(" "));
        }
        let target = anchor.raw();
        let condition = format!("#{{==:#{{window_name}},#{{{RENAME_OBSERVED_OPTION}}}}}");
        let target_name = sanitize_window_name(name);
        let guarded = guarded.join(" ; ");
        Ok(self.cmd().args([
            "set-option",
            "-w",
            "-t",
            target,
            RENAME_OBSERVED_OPTION,
            observed,
            ";",
            "set-option",
            "-w",
            "-t",
            target,
            RENAME_TARGET_OPTION,
            target_name.as_str(),
            ";",
            "if-shell",
            "-F",
            "-t",
            target,
            condition.as_str(),
            guarded.as_str(),
            ";",
            "set-option",
            "-wu",
            "-t",
            target,
            RENAME_OBSERVED_OPTION,
            ";",
            "set-option",
            "-wu",
            "-t",
            target,
            RENAME_TARGET_OPTION,
        ]))
    }

    /// Open one detached named window and validate the ids printed by the same
    /// tmux client invocation.
    pub(super) fn open_named_window(
        &self,
        session: &str,
        name: &str,
        cwd: &Path,
        argv: &[String],
        after: Option<&PaneId>,
        env: &BTreeMap<String, String>,
    ) -> Result<OpenedWindow> {
        let anchor_window = match after
            .map(|anchor| self.window_id_for_pane(anchor, session))
            .transpose()
        {
            Ok(window) => window,
            Err(err) => {
                tracing::warn!(
                    tags.operation = "tmux.place_tab",
                    error = &err as &dyn std::error::Error,
                    "could not place the new window after its anchor; appending it instead",
                );
                None
            }
        };
        let birth_name = format!("rimz-birth-{}", uuid::Uuid::now_v7().simple());
        let birth_target = format!("={session}:={birth_name}");
        let mut args = vec!["new-window".to_owned(), "-d".to_owned()];
        if anchor_window.is_some() {
            args.push("-a".to_owned());
        }
        args.extend([
            "-P".to_owned(),
            "-F".to_owned(),
            "#{window_id} #{pane_id}".to_owned(),
            "-t".to_owned(),
            anchor_window.unwrap_or_else(|| session.to_owned()),
            "-n".to_owned(),
            birth_name,
            "-c".to_owned(),
            cwd.to_string_lossy().into_owned(),
        ]);
        for (key, value) in env {
            args.extend(["-e".to_owned(), format!("{key}={value}")]);
        }
        // The final name is visible only after ownership exists. A detached
        // new-window does not make the new window the command list's target.
        let output = self
            .cmd()
            .args(args)
            .args(argv.iter().cloned())
            .args([
                ";",
                "set-option",
                "-w",
                "-t",
                &birth_target,
                RIMZ_TAB_BASE_OPTION,
                &sanitize_window_name(name),
                ";",
                "set-option",
                "-w",
                "-F",
                "-t",
                &birth_target,
                RIMZ_TAB_FOUNDERS_OPTION,
                "#{pane_id}",
                ";",
                "rename-window",
                "-t",
                &birth_target,
                &window_name_arg(name),
            ])
            .run()?;
        let (window_id, first_pane) = parse_new_window_ids(&output.stdout)?;
        Ok(OpenedWindow {
            window_id,
            first_pane,
        })
    }

    fn window_id_for_pane(&self, pane: &PaneId, session: &str) -> Result<String> {
        ensure_pane_backend(pane, MuxName::Tmux)?;
        let output = self
            .cmd()
            .args([
                "display-message",
                "-p",
                "-t",
                pane.raw(),
                "#{session_name} #{window_id}",
            ])
            .run()?;
        let raw = String::from_utf8_lossy(&output.stdout);
        if let Some((anchor_session, window_id)) = raw.trim_end().rsplit_once(' ')
            && anchor_session == session
            && window_id.starts_with('@')
        {
            return Ok(window_id.to_owned());
        }
        Err(MuxErr::Output {
            program: "tmux".to_owned(),
            reason: format!("pane `{pane}` did not resolve to a window in session `{session}`"),
        })
    }

    /// Close a single pane by id (`kill-pane -t %N`), terminating its process.
    /// Reconcile uses this to drop a duplicate or unresponsive sidebar pane
    /// without touching the rest of the window.
    pub(super) fn kill_pane(&self, pane: &PaneId) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Tmux)?;
        self.cmd()
            .args([
                "kill-pane".to_owned(),
                "-t".to_owned(),
                pane.raw().to_owned(),
            ])
            .run()
            .map(|_| ())
    }

    /// Split a left sidebar into a specific window in place. When the leftmost
    /// pane already spans the full window height, target that pane so the
    /// sidebar is carved from it alone and the work columns keep their widths.
    /// The `-f` fallback spans the full window edge for split-column left edges,
    /// where tmux can only create a full-height sidebar by redistributing the
    /// remaining width proportionally across the work panes. `-b` places the
    /// pane before/left, `-l <size>` fixes its width, and `-d` keeps the
    /// caller's focus.
    pub(super) fn add_sidebar_to_window(
        &self,
        opts: &SidebarPaneOptions,
        window_id: &str,
    ) -> Result<PaneId> {
        let view_cols = self
            .window_width(window_id)
            .map(|cols| u16::try_from(cols).unwrap_or(u16::MAX));
        let size = opts.target.cols(view_cols).to_string();
        let mut args = vec![
            "split-window".to_owned(),
            "-d".to_owned(),
            "-h".to_owned(),
            "-c".to_owned(),
            "#{session_path}".to_owned(),
            "-P".to_owned(),
            "-F".to_owned(),
            "#{pane_id}".to_owned(),
        ];
        if let Some(pane) = self.leftmost_full_height_pane(window_id) {
            args.extend([
                "-b".to_owned(),
                "-l".to_owned(),
                size,
                "-t".to_owned(),
                pane,
            ]);
        } else {
            args.extend([
                "-f".to_owned(),
                "-b".to_owned(),
                "-l".to_owned(),
                size,
                "-t".to_owned(),
                window_id.to_owned(),
            ]);
        }
        let output = self
            .cmd()
            .args(args)
            .args(sidebar_serve_command(opts))
            .run()?;
        let raw = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !raw.starts_with('%') {
            return Err(MuxErr::Output {
                program: "tmux".to_owned(),
                reason: format!("split-window returned invalid sidebar pane id `{raw}`"),
            });
        }
        Ok(PaneId::from_parts(MuxName::Tmux, raw))
    }

    /// The live column width of `window_id`, when tmux can report it.
    pub(super) fn window_width(&self, window_id: &str) -> Option<u64> {
        let output = self
            .cmd()
            .args(["display-message", "-p", "-t", window_id, "#{window_width}"])
            .run()
            .ok()?;
        String::from_utf8_lossy(&output.stdout).trim().parse().ok()
    }

    /// Align only the detached session's default-size and non-manual current
    /// window with the attaching terminal, minus its status rows. Write only
    /// differences, preserve the local window-size, and leave hidden windows
    /// to the sweep. Best-effort; reconcile's width convergence follows it.
    pub(super) fn normalize_detached_geometry(&self, session: &str, cols: u16, rows: u16) {
        let output = match self
            .batch_spec(&[
                vec![
                    "display-message".into(),
                    "-p".into(),
                    "-t".into(),
                    session.into(),
                    concat!(
                        "#{default-size}|#{status}|#{window_id}|",
                        "#{window_width}|#{window_height}|#{window-size}",
                    )
                    .into(),
                ],
                vec![
                    "show-options".into(),
                    "-wqv".into(),
                    "-t".into(),
                    session.into(),
                    "window-size".into(),
                ],
            ])
            .run()
        {
            Ok(output) => output,
            Err(err) => {
                tracing::warn!(
                    session,
                    tags.operation = "tmux.reconcile.read_geometry",
                    error = &err as &dyn std::error::Error,
                    "sidebar reconcile: reading detached geometry for normalization failed",
                );
                return;
            }
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        let Some(geometry) = parse_detached_geometry(&stdout) else {
            tracing::warn!(
                session,
                tags.operation = "tmux.reconcile.read_geometry",
                error = "invalid detached geometry",
                "sidebar reconcile: reading detached geometry for normalization failed",
            );
            return;
        };
        let default_size = format!("{cols}x{rows}");
        let mut commands = Vec::new();
        if geometry.default_size != default_size {
            commands.push(vec![
                "set-option".into(),
                "-t".into(),
                session.into(),
                "default-size".into(),
                default_size,
            ]);
        }
        let window_rows = rows.saturating_sub(geometry.status_lines).max(1);
        if geometry.policy != "manual" && geometry.size != (cols, window_rows) {
            commands.push(vec![
                "resize-window".into(),
                "-t".into(),
                geometry.window.into(),
                "-x".into(),
                cols.to_string(),
                "-y".into(),
                window_rows.to_string(),
            ]);
            commands.push(window_option_argv(
                "window-size",
                geometry.local_policy,
                Some(geometry.window),
            ));
        }
        if let Err(err) = self.batch(&commands) {
            tracing::warn!(
                session,
                cols,
                rows,
                tags.operation = "tmux.reconcile.normalize_geometry",
                error = &err as &dyn std::error::Error,
                "sidebar reconcile: detached geometry normalization failed; continuing best-effort",
            );
        }
    }

    /// Every pane's current width and containing-window width in `session`,
    /// collected with one tmux client invocation for sidebar reconciliation.
    pub(super) fn session_pane_geometries(&self, session: &str) -> Result<Vec<TmuxPaneGeometry>> {
        let output = self.session_pane_geometries_command(session).run()?;
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(parse_tmux_pane_geometry)
            .collect())
    }

    pub(super) fn session_pane_geometries_command(&self, session: &str) -> CommandSpec {
        self.cmd().args([
            "list-panes",
            "-s",
            "-t",
            session,
            "-F",
            "#{pane_id} #{window_id} #{pane_width} #{window_width} #{==:#{pane_title},rimz-sidebar}",
        ])
    }

    /// Resize a freshly-born tab up to the widest attached client and
    /// re-assert the hook-docked sidebar to its live target, so agent column
    /// splits land even at full width. Returns whether it resized the window;
    /// the caller must then restore autosizing after placing the splits.
    ///
    /// No-ops when the tab is already at least the widest client's width or
    /// when no sized client is attached.
    pub(super) fn normalize_tab_birth_width(
        &self,
        window_id: &str,
        first_pane: &str,
        sidebar: &SidebarPaneOptions,
    ) -> bool {
        let Some((full_w, full_h)) = self.widest_client_size(&sidebar.session_name) else {
            return false;
        };
        match self.window_width(window_id) {
            Some(width) if width >= full_w => return false,
            _ => {}
        }
        if self
            .cmd()
            .args([
                "resize-window".to_owned(),
                "-t".to_owned(),
                window_id.to_owned(),
                "-x".to_owned(),
                full_w.to_string(),
                "-y".to_owned(),
                full_h.to_string(),
            ])
            .run()
            .is_err()
        {
            return false;
        }

        // The after-new-window hook docks the sidebar with `-b`, which makes
        // it the left-edge pane. Before agent column splits, the only other
        // pane is the first agent; if that is leftmost, no hook ran.
        if let Some(sidebar_pane) = self.leftmost_pane(window_id)
            && sidebar_pane != first_pane
        {
            let sidebar_cols = sidebar
                .target
                .cols(Some(u16::try_from(full_w).unwrap_or(u16::MAX)))
                .get();
            let _ = self
                .cmd()
                .args([
                    "resize-pane".to_owned(),
                    "-t".to_owned(),
                    sidebar_pane,
                    "-x".to_owned(),
                    sidebar_cols.to_string(),
                ])
                .run();
        }
        true
    }

    /// Undo the `window-size manual` pin that `resize-window` sets in
    /// [`Self::normalize_tab_birth_width`], so the tab tracks client size
    /// again.
    pub(super) fn restore_window_autosize(&self, window_id: &str) {
        let _ = self
            .cmd()
            .args([
                "set-window-option".to_owned(),
                "-u".to_owned(),
                "-t".to_owned(),
                window_id.to_owned(),
                "window-size".to_owned(),
            ])
            .run();
    }

    pub(super) fn remove_sidebar_from_tab(
        &self,
        window_id: &str,
        first_pane: &str,
        rebalance_even: bool,
    ) -> Result<()> {
        if let Some(sidebar_pane) = self.pane_left_of_first_work(window_id, first_pane) {
            self.kill_pane(&PaneId::from_parts(MuxName::Tmux, sidebar_pane))?;
        }
        if !rebalance_even {
            return Ok(());
        }
        self.cmd()
            .args([
                "select-layout".to_owned(),
                "-t".to_owned(),
                window_id.to_owned(),
                "even-horizontal".to_owned(),
            ])
            .run()
            .map(|_| ())
    }

    /// The hook-docked sidebar is born before the first work pane. Use geometry
    /// instead of the sidebar title, which the renderer sets asynchronously.
    fn pane_left_of_first_work(&self, window_id: &str, first_pane: &str) -> Option<String> {
        let output = self
            .cmd()
            .args([
                "list-panes",
                "-t",
                window_id,
                "-F",
                "#{pane_id} #{pane_left}",
            ])
            .run()
            .ok()?;
        let mut first_left = None;
        let mut leftmost: Option<(u64, String)> = None;
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let (pane_id, left) = line.split_once(' ')?;
            let left: u64 = left.parse().ok()?;
            if pane_id == first_pane {
                first_left = Some(left);
            }
            if leftmost.as_ref().is_none_or(|(min, _)| left < *min) {
                leftmost = Some((left, pane_id.to_owned()));
            }
        }
        let first_left = first_left?;
        let (left, pane_id) = leftmost?;
        (left < first_left).then_some(pane_id)
    }

    /// The `(cols, rows)` of the widest attached client of `session` that
    /// counts toward window sizing. Control-mode `ignore-size` clients are
    /// skipped because they are observers, not display surfaces.
    pub(super) fn widest_client_size(&self, session: &str) -> Option<(u64, u64)> {
        let output = self
            .cmd()
            .args([
                "list-clients",
                "-t",
                session,
                "-F",
                "#{client_width} #{client_height} #{client_flags}",
            ])
            .run()
            .ok()?;
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let width: u64 = fields.next()?.parse().ok()?;
                let height: u64 = fields.next()?.parse().ok()?;
                let flags = fields.next().unwrap_or("");
                (!flags.split(',').any(|flag| flag == "ignore-size")).then_some((width, height))
            })
            .max_by_key(|(width, _)| *width)
    }

    /// The id of the pane at the window's left edge.
    pub(super) fn leftmost_pane(&self, window_id: &str) -> Option<String> {
        let output = self
            .cmd()
            .args([
                "list-panes",
                "-t",
                window_id,
                "-F",
                "#{pane_at_left} #{pane_id}",
            ])
            .run()
            .ok()?;
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| line.strip_prefix("1 ").map(|id| id.trim().to_owned()))
    }

    /// The window's leftmost pane when it spans the full window height.
    fn leftmost_full_height_pane(&self, window_id: &str) -> Option<String> {
        let output = self
            .cmd()
            .args([
                "list-panes",
                "-t",
                window_id,
                "-F",
                "#{pane_at_left} #{pane_at_top} #{pane_at_bottom} #{pane_id}",
            ])
            .run()
            .ok()?;
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| {
                let mut fields = line.split_whitespace();
                let at_left = fields.next()?;
                let at_top = fields.next()?;
                let at_bottom = fields.next()?;
                let id = fields.next()?;
                (at_left == "1" && at_top == "1" && at_bottom == "1").then(|| id.to_owned())
            })
    }

    /// Whether `session` already holds a window named `name`. A RimZ background
    /// view is idempotent on its window name, so a relaunch into a session that
    /// already carries it is skipped.
    pub(super) fn session_has_window(&self, session: &str, name: &str) -> Result<bool> {
        let sanitized = sanitize_window_name(name);
        let config = crate::config::MachineConfig::load_lenient();
        let theme = &config.theme;
        Ok(self
            .window_names(session)?
            .iter()
            .map(|window| crate::theme::strip_status_glyph_suffix(window, theme))
            .any(|window| window == name || window == sanitized))
    }

    /// Every window name in `session` — one `list-windows` probe that callers
    /// checking several names share instead of forking per name.
    pub(super) fn window_names(&self, session: &str) -> Result<Vec<String>> {
        let output = self
            .cmd()
            .args(["list-windows", "-t", session, "-F", "#{window_name}"])
            .run()?;
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|line| line.trim().to_owned())
            .collect())
    }

    /// Force the named window to the session's first slot. tmux tracks the
    /// current window by winlink slot, not stable window id, so swapping into the
    /// base slot can otherwise pull focus to the daemon view. Capture the active
    /// window id before the swap and restore it after; ids survive `swap-window`.
    /// Best-effort: a reorder or focus hiccup never sinks an otherwise-launched
    /// view.
    pub(super) fn lead_window(&self, session: &str, name: &str) {
        let name = sanitize_window_name(name);
        let base = self.base_index();
        let active_window = match self
            .cmd()
            .args(["display-message", "-p", "-t", session, "#{window_id}"])
            .run()
        {
            Ok(output) => {
                let window_id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                (!window_id.is_empty()).then_some(window_id)
            }
            Err(err) => {
                tracing::warn!(
                    session = %session,
                    tags.operation = "tmux.capture_active_window",
                    error = &err as &dyn std::error::Error,
                    "could not capture the active window before moving the daemon window",
                );
                None
            }
        };
        if let Err(err) = self
            .cmd()
            .args([
                "swap-window".to_owned(),
                "-d".to_owned(),
                "-s".to_owned(),
                format!("{session}:{name}"),
                "-t".to_owned(),
                format!("{session}:{base}"),
            ])
            .run()
        {
            tracing::warn!(
                session = %session,
                tags.operation = "tmux.move_daemon_window",
                error = &err as &dyn std::error::Error,
                "could not move the daemon window to the front",
            );
            return;
        }
        let Some(active_window) = active_window else {
            return;
        };
        if let Err(err) = self
            .cmd()
            .args(["select-window".to_owned(), "-t".to_owned(), active_window])
            .run()
        {
            tracing::warn!(
                session = %session,
                tags.operation = "tmux.restore_active_window",
                error = &err as &dyn std::error::Error,
                "could not restore focus after moving the daemon window",
            );
        }
    }

    /// Re-seed the reborn session's prior agents, one `#channel` window per
    /// worktree, born `sidebar | agents…` via the `after-new-window` hook.
    /// Idempotent on the window name so a re-run never doubles a channel
    /// window; the freshest channel (the first in the plan) is selected so
    /// attach lands on it, mirroring the Zellij layout's focus. Best-effort: a
    /// failed window is logged and skipped, and `confirm_resume_tabs` reports it.
    pub(super) fn seed_resume_windows(&self, opts: &SidebarPaneOptions) {
        if opts.resume_tabs.is_empty() {
            return;
        }
        // One `list-windows` probe covers every tab's idempotency check. A
        // failed probe means re-seeding cannot be made idempotent, so every
        // channel is left out.
        let existing = match self.window_names(&opts.session_name) {
            Ok(names) => names,
            Err(err) => {
                tracing::warn!(
                    session = %opts.session_name,
                    tags.operation = "tmux.resume.window_probe",
                    error = &err as &dyn std::error::Error,
                    "resume: window probe failed; leaving the agents out",
                );
                return;
            }
        };
        let config = crate::config::MachineConfig::load_lenient();
        let theme = &config.theme;
        let mut seeded = existing
            .iter()
            .map(|name| crate::theme::strip_status_glyph_suffix(name, theme).to_owned())
            .collect::<HashSet<_>>();
        let mut focus_window: Option<String> = None;
        for tab in &opts.resume_tabs {
            let label = sanitize_window_name(&tab.label);
            if !seeded.insert(label.clone()) {
                continue; // already seeded by an earlier birth
            }
            let Some(first) = tab
                .layout
                .columns
                .first()
                .and_then(|column| column.panes.first())
            else {
                continue;
            };
            match self.open_named_window(
                &opts.session_name,
                &tab.label,
                &tab.cwd,
                &first.argv,
                None,
                &tab.env,
            ) {
                Ok(opened) => {
                    if let Some(name) = first.name.as_deref() {
                        self.set_pane_rimz_title(&opened.first_pane, name);
                    }
                    if focus_window.is_none() {
                        focus_window = Some(opened.window_id.clone());
                    }
                    // Leave tmux's window layout alone after the hook docks the
                    // sidebar: `select-layout` retiles every pane in the window,
                    // including the managed left sidebar. Additional agents split
                    // the active work area and preserve the sidebar's fixed width.
                    if let Err(err) = self
                        .split_layout_columns(&opened.first_pane, &tab.cwd, &tab.layout, &tab.env)
                        .and_then(|founders| {
                            self.cmd()
                                .args(Self::window_owner_args(
                                    &opened.window_id,
                                    &tab.label,
                                    &founders,
                                ))
                                .run()
                        })
                    {
                        tracing::warn!(
                            session = %opts.session_name,
                            tab = %tab.label,
                            tags.operation = "tmux.resume.split_window",
                            error = &err as &dyn std::error::Error,
                            "resume: launching or recording agent panes failed; leaving them out",
                        );
                    }
                }
                Err(err) => tracing::warn!(
                    session = %opts.session_name,
                    tab = %tab.label,
                    tags.operation = "tmux.resume.launch_window",
                    error = &err as &dyn std::error::Error,
                    "resume: launching the channel window failed or returned invalid ids; leaving extra panes out",
                ),
            }
        }
        if let Some(window_id) = focus_window {
            let _ = self
                .cmd()
                .args(["select-window".to_owned(), "-t".to_owned(), window_id])
                .run();
        }
    }

    pub(super) fn split_named_printed(
        &self,
        direction: &str,
        target: &str,
        size: Option<&str>,
        cwd: &Path,
        pane: &crate::mux::PaneCmd,
        env: &BTreeMap<String, String>,
    ) -> Result<String> {
        let pane_id = self.split_printed_with_reason(
            direction,
            target,
            size,
            cwd,
            &pane.argv,
            "split-window did not print a pane id",
            env,
        )?;
        if let Some(name) = &pane.name {
            self.set_pane_rimz_title(&pane_id, name);
        }
        Ok(pane_id)
    }

    /// Split `panes` off `anchor` along `axis`, each split chained onto the
    /// pane the previous one created and sized in cells, so the anchor's
    /// extent ends up evenly divided at creation with no resize pass. Returns
    /// the new pane ids in `panes` order, which is also their on-screen order:
    /// tmux inserts each new pane immediately after its target.
    pub(super) fn split_even_run(
        &self,
        axis: SplitAxis,
        anchor: &str,
        cwd: &Path,
        panes: &[&crate::mux::PaneCmd],
        env: &BTreeMap<String, String>,
    ) -> Result<Vec<String>> {
        if panes.is_empty() {
            return Ok(Vec::new());
        }
        let sizes = even_split_sizes(self.pane_extent(axis, anchor)?, panes.len() + 1);
        let mut created = Vec::with_capacity(panes.len());
        let mut previous = anchor.to_owned();
        for (pane, size) in panes.iter().zip(sizes) {
            previous = self.split_named_printed(
                axis.direction(),
                &previous,
                Some(&size.to_string()),
                cwd,
                pane,
                env,
            )?;
            created.push(previous.clone());
        }
        Ok(created)
    }

    /// The pane's live extent along `axis`, in cells.
    fn pane_extent(&self, axis: SplitAxis, pane: &str) -> Result<u64> {
        let output = self
            .cmd()
            .args(["display-message", "-p", "-t", pane, axis.extent_format()])
            .run()?;
        let raw = String::from_utf8_lossy(&output.stdout);
        raw.trim().parse().map_err(|_| MuxErr::Output {
            program: "tmux".to_owned(),
            reason: format!(
                "display-message returned an invalid pane extent `{}`",
                raw.trim()
            ),
        })
    }

    pub(super) fn append_equal_host_rows<'a>(
        &self,
        first_pane: &str,
        pane_count: usize,
        rows: impl IntoIterator<Item = &'a HostPane>,
        empty_reason: &str,
    ) -> Result<()> {
        let mut previous = first_pane.to_owned();
        for (index, pane) in rows.into_iter().enumerate() {
            let size = equal_row_split_size(pane_count, index + 1);
            previous = self.split_printed_with_reason(
                "-v",
                &previous,
                Some(&size),
                &pane.cwd,
                &pane.argv,
                empty_reason,
                &BTreeMap::new(),
            )?;
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "shared split options plus the caller's diagnostic and pane environment"
    )]
    pub(super) fn split_printed_with_reason(
        &self,
        direction: &str,
        target: &str,
        size: Option<&str>,
        cwd: &Path,
        argv: &[String],
        empty_reason: &str,
        env: &BTreeMap<String, String>,
    ) -> Result<String> {
        let mut args = vec![
            "split-window".to_owned(),
            "-d".to_owned(),
            direction.to_owned(),
            "-P".to_owned(),
            "-F".to_owned(),
            "#{pane_id}".to_owned(),
            "-t".to_owned(),
            target.to_owned(),
        ];
        if let Some(size) = size {
            args.extend(["-l".to_owned(), size.to_owned()]);
        }
        args.extend(["-c".to_owned(), cwd.to_string_lossy().into_owned()]);
        for (key, value) in env {
            args.extend(["-e".to_owned(), format!("{key}={value}")]);
        }
        let output = self.cmd().args(args).args(argv.iter().cloned()).run()?;
        let pane_id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if pane_id.is_empty() {
            return Err(MuxErr::Output {
                program: "tmux".to_owned(),
                reason: empty_reason.to_owned(),
            });
        }
        Ok(pane_id)
    }
}

pub(super) fn parse_tmux_pane_geometry(line: &str) -> Option<TmuxPaneGeometry> {
    let mut fields = line.split_whitespace();
    let geometry = TmuxPaneGeometry {
        pane_id: fields.next()?.to_owned(),
        window_id: fields.next()?.to_owned(),
        pane_width: fields.next()?.parse().ok()?,
        window_width: fields.next()?.parse().ok()?,
        is_sidebar: fields.next()? == "1",
    };
    fields.next().is_none().then_some(geometry)
}

fn parse_tmux_pane_cell(line: &str) -> Option<TmuxPaneCell> {
    let mut fields = line.split_whitespace();
    let cell = TmuxPaneCell {
        pane_id: fields.next()?.to_owned(),
        left: fields.next()?.parse().ok()?,
        width: fields.next()?.parse().ok()?,
        top: fields.next()?.parse().ok()?,
        height: fields.next()?.parse().ok()?,
    };
    fields.next().is_none().then_some(cell)
}

struct WindowGeometry {
    view: crate::ids::ViewId,
    current: bool,
    size: crate::mux::ViewSize,
    stack: usize,
    manual: bool,
    attached: bool,
}

fn geometry_error() -> MuxErr {
    MuxErr::Output {
        program: "tmux".to_owned(),
        reason: "invalid hidden-window geometry".to_owned(),
    }
}

struct DetachedGeometry<'a> {
    default_size: &'a str,
    status_lines: u16,
    window: &'a str,
    size: (u16, u16),
    policy: &'a str,
    local_policy: Option<&'a str>,
}

fn parse_detached_geometry(output: &str) -> Option<DetachedGeometry<'_>> {
    let mut lines = output.lines();
    let fields: Vec<_> = lines.next()?.split('|').collect();
    let [default_size, status, window, cols, rows, policy] = fields.as_slice() else {
        return None;
    };
    let status_lines = match *status {
        "off" => 0,
        "on" => 1,
        value => value.parse().ok().filter(|lines| (2..=5).contains(lines))?,
    };
    window.strip_prefix('@')?.parse::<u32>().ok()?;
    Some(DetachedGeometry {
        default_size,
        status_lines,
        window,
        size: (cols.parse().ok()?, rows.parse().ok()?),
        policy,
        local_policy: lines.next().filter(|policy| !policy.is_empty()),
    })
}

fn parse_window_geometry(line: &str) -> Option<WindowGeometry> {
    let mut fields = line.split_whitespace();
    let view = fields.next()?;
    view.strip_prefix('@')?.parse::<u32>().ok()?;
    let current = match fields.next()? {
        "0" => false,
        "1" => true,
        _ => return None,
    };
    let cols = fields.next()?.parse().ok().filter(|cols| *cols > 0)?;
    let rows = fields.next()?.parse().ok().filter(|rows| *rows > 0)?;
    let stack = fields.next()?.parse().ok()?;
    let manual = match fields.next()? {
        "manual" => true,
        "latest" | "largest" | "smallest" => false,
        _ => return None,
    };
    let attached = match fields.next()? {
        "0" => false,
        "1" => true,
        _ => return None,
    };
    fields.next().is_none().then(|| WindowGeometry {
        view: crate::ids::ViewId::new_unchecked(view),
        current,
        size: crate::mux::ViewSize { cols, rows },
        stack,
        manual,
        attached,
    })
}

fn parse_fitted_panes(
    output: &str,
) -> Result<Option<std::collections::HashMap<PaneId, crate::mux::FittedPane>>> {
    if output.trim() == "skipped" {
        return Ok(None);
    }
    let mut panes = std::collections::HashMap::new();
    let mut sidebar = None;
    for line in output.lines() {
        let reasserted = line.strip_prefix("sidebar ");
        let mut fields = reasserted.unwrap_or(line).split_whitespace();
        let pane = fields.next().ok_or_else(geometry_error)?;
        pane.strip_prefix('%')
            .and_then(|id| id.parse::<u32>().ok())
            .ok_or_else(geometry_error)?;
        let extent = fields
            .next()
            .and_then(|extent| extent.parse().ok())
            .filter(|extent| *extent > 0)
            .ok_or_else(geometry_error)?;
        if fields.next().is_some() {
            return Err(geometry_error());
        }
        let pane = PaneId::from_parts(MuxName::Tmux, pane);
        if reasserted.is_some() {
            if sidebar.replace((pane, extent)).is_some() || !panes.is_empty() {
                return Err(geometry_error());
            }
            continue;
        }
        panes.insert(
            pane,
            crate::mux::FittedPane {
                rows: extent,
                cols: None,
            },
        );
    }
    if let Some((pane, cols)) = sidebar {
        panes.get_mut(&pane).ok_or_else(geometry_error)?.cols = Some(cols);
    }
    Ok(Some(panes))
}

fn select_lagging_views(output: &str) -> Result<Option<crate::mux::LaggingViews>> {
    let windows = output
        .lines()
        .map(|line| parse_window_geometry(line).ok_or_else(geometry_error))
        .collect::<Result<Vec<_>>>()?;
    let current = windows
        .iter()
        .find(|window| window.current)
        .ok_or_else(geometry_error)?;
    if !current.attached {
        return Ok(None);
    }
    let target = current.size;
    let mut hidden: Vec<_> = windows
        .into_iter()
        .filter(|window| !window.current && !window.manual && window.size != target)
        .collect();
    hidden.sort_by_key(|window| {
        if window.stack == 0 {
            usize::MAX
        } else {
            window.stack
        }
    });
    Ok(Some(crate::mux::LaggingViews {
        target,
        views: hidden.into_iter().map(|window| window.view).collect(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ViewId;
    use crate::mux::{LaggingViews, ViewSize};

    fn companion_member(id: &str, y: u64, rows: u64, stack: &str) -> NativePaneRect {
        NativePaneRect {
            rect: PaneRect {
                pane_id: PaneId::from_parts(MuxName::Tmux, id),
                x: 31,
                y,
                cols: 100,
                rows,
            },
            stack: stack.to_owned(),
            zoomed: false,
            floating: false,
            chrome: false,
        }
    }

    #[test]
    fn companion_stack_admits_lone_pane_and_targets_bottom_member() {
        let lone = companion_member("%1", 0, 13, "");
        assert_eq!(
            companion_stack_target(&[lone]),
            Some(PaneId::from_parts(MuxName::Tmux, "%1"))
        );
        let stack = [
            companion_member("%3", 14, 1, "%1"),
            companion_member("%1", 0, 13, "%1"),
        ];
        assert_eq!(
            companion_stack_target(&stack),
            Some(PaneId::from_parts(MuxName::Tmux, "%3"))
        );
    }

    #[test]
    fn companion_stack_room_admits_floor_but_not_one_row_less() {
        for (span, admits) in [(15, true), (14, false)] {
            let panes = [
                companion_member("%1", 0, span - 2, "%1"),
                companion_member("%2", span - 1, 1, "%1"),
            ];
            assert_eq!(
                companion_stack_target(&panes).is_some(),
                admits,
                "span {span}"
            );
        }
    }

    #[test]
    fn companion_stack_rejects_grids_mixed_marks_zoom_and_chrome_only() {
        assert!(companion_stack_target(&[companion_member("%1", 0, 40, "")]).is_some());
        let mut zoomed = companion_member("%1", 0, 40, "");
        zoomed.zoomed = true;
        let mut chrome = companion_member("%0", 0, 40, "");
        chrome.chrome = true;
        for panes in [
            vec![],
            vec![
                companion_member("%1", 0, 19, ""),
                companion_member("%2", 20, 20, ""),
            ],
            vec![
                companion_member("%1", 0, 19, "%1"),
                companion_member("%2", 20, 20, "%2"),
            ],
            vec![
                companion_member("%1", 0, 19, "%1"),
                companion_member("%2", 20, 20, ""),
            ],
            vec![zoomed],
            vec![chrome],
        ] {
            assert!(companion_stack_target(&panes).is_none());
        }
    }

    #[test]
    fn companion_stack_ignores_floating_panes_and_sidebar_anchor() {
        let mut chrome = companion_member("%0", 0, 40, "");
        chrome.chrome = true;
        let mut floating = companion_member("%9", 50, 20, "");
        floating.floating = true;
        let panes = [chrome, floating, companion_member("%1", 0, 40, "")];
        assert_eq!(
            companion_stack_target(&panes),
            Some(PaneId::from_parts(MuxName::Tmux, "%1"))
        );
    }

    #[test]
    fn declined_fit_is_skipped_not_fitted_rows() {
        assert!(matches!(parse_fitted_panes("skipped\n"), Ok(None)));
        assert_eq!(
            parse_fitted_panes("%1 65\n").unwrap(),
            Some(std::collections::HashMap::from([(
                PaneId::from_parts(MuxName::Tmux, "%1"),
                crate::mux::FittedPane {
                    rows: 65,
                    cols: None
                }
            ),]))
        );
        assert!(parse_fitted_panes("skipped\n%1 65\n").is_err());
    }

    #[test]
    fn lagging_selection_orders_mru_and_excludes_current_manual_and_equal() {
        let output = "@0 1 200 60 0 latest 1\n@1 0 180 50 0 latest 1\n@2 0 180 50 2 largest 1\n@3 0 180 50 1 smallest 1\n@4 0 180 50 3 manual 1\n@5 0 200 60 4 latest 1\n";
        assert_eq!(
            select_lagging_views(output).unwrap(),
            Some(LaggingViews {
                target: ViewSize {
                    cols: 200,
                    rows: 60
                },
                views: ["@3", "@2", "@1"].map(ViewId::new_unchecked).into(),
            })
        );
    }

    #[test]
    fn detached_views_are_not_fitted_and_malformed_rows_are_rejected() {
        assert_eq!(
            select_lagging_views("@0 1 200 60 0 latest 0\n@1 0 180 50 1 latest 0").unwrap(),
            None
        );
        for output in [
            "@0 1 wide 60 0 latest 1",
            "@0 1 200 60 0 latest 1 extra",
            "@0 1 200 60 0 unknown 1",
            "not-a-view 1 200 60 0 latest 1",
        ] {
            assert!(select_lagging_views(output).is_err(), "{output}");
        }
    }
}
