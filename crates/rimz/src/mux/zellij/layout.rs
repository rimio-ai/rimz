//! KDL layout rendering for the Zellij backend: session birth (working tab,
//! daemon view, resumed agents), the `new-tab --layout` background view, and
//! the RAII temp file the async `--default-layout` parse reads from. Pure
//! `&options → String` renderers — no backend state, no subprocess.

use std::path::{Path, PathBuf};

use crate::ids::MuxName;
use crate::mux::{
    BackgroundViewOptions, DaemonView, HostPane, LayoutColumn, MuxErr, Result, ResumeTab,
    SidebarPaneOptions, TabOptions, sidebar_serve_args,
};
use crate::pane::SIDEBAR_CHROME_TITLE;

pub(super) struct TempLayoutFile {
    path: PathBuf,
}

impl TempLayoutFile {
    pub(super) fn new(contents: String) -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "rimz-zellij-layout-{}-{}.kdl",
            std::process::id(),
            uuid::Uuid::now_v7().simple(),
        ));
        std::fs::write(&path, contents)?;
        Ok(Self { path })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempLayoutFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The one-row compact-bar plugin pane. Supplying our own layout replaces
/// Zellij's built-in tab/status bar, so every view re-adds the compact-bar or is
/// born bar-less. Must stay multi-line: Zellij's KDL parser rejects the
/// single-line `pane {{ plugin … }}` form.
const COMPACT_BAR_KDL: &str = r#"pane size=1 borderless=true {
        plugin location="zellij:compact-bar"
    }"#;

/// The left `rimz sidebar serve` pane every Zellij view carries, as a KDL `pane`
/// block. `cwd` is spelled only when the pane can't inherit the session's
/// `--default-cwd` — the `new-tab --layout` path ([`render_background_view_layout`]).
/// Birth layouts set `--default-cwd` and pass `None`.
fn sidebar_pane_kdl(
    opts: &SidebarPaneOptions,
    cwd: Option<&Path>,
    width_percent: u16,
) -> Result<String> {
    let rimz_bin = kdl_string(&opts.rimz_bin.to_string_lossy())?;
    // Fixed-size layout panes are resize-pinned by Zellij, so every sidebar
    // spells the shared view proportion as a percentage.
    let size = kdl_string(&format!("{}%", width_percent.clamp(1, 100)))?;
    let pane_name = kdl_string(SIDEBAR_CHROME_TITLE)?;
    let cwd_attr = match cwd {
        Some(cwd) => format!(" cwd={}", kdl_string(&cwd.to_string_lossy())?),
        None => String::new(),
    };
    let serve_args = sidebar_serve_args(MuxName::Zellij, opts)
        .into_iter()
        .map(|arg| kdl_string(&arg))
        .collect::<Result<Vec<_>>>()?
        .join(" ");
    Ok(format!(
        r#"pane size={size} name={pane_name} borderless=true{cwd_attr} {{
            command {rimz_bin}
            args {serve_args}
            start_suspended false
            close_on_exit true
        }}"#,
    ))
}

/// The session-birth layout for a room that leads with a daemon view and/or
/// re-seeds prior agents. Zellij can't reorder tabs or add command panes after
/// birth, so the order and content are fixed here: the daemon tab
/// (`sidebar | content | hosts…`, first, when present), then one `#channel` tab
/// per resumed worktree (`sidebar | agents…`), then the working tab
/// (`sidebar | terminal`). Focus lands on the most-recently-active resumed
/// channel when there is one, else on the working terminal — so attach drops
/// the user straight onto a restored agent.
///
/// The same renderer also births the plain room (`None`, empty resume set). The
/// bare `tab` nodes are load-bearing: on Zellij 0.44.3 a layout carrying a
/// `new_tab_template` but no tab node kills the background session instead of
/// creating the implicit first tab. The terminal in the template is an explicit
/// `pane focus=true`, not Zellij's `children` placeholder; nested `children`
/// creates the right terminal on 0.44.3 but leaves focus stranded on the
/// sidebar in newly-created tabs. The visible layout pins the sidebar and
/// compact-bar as fixed tree siblings. Zellij's `auto_layout=false` plus
/// `stacked_resize=true` leaves no-direction pane opens and closes on the
/// focused-pane native split path instead of a root swap layout. All panes
/// inherit the session's `--default-cwd` except the daemon hosts and resumed
/// agents, which carry their own worktree cwd.
///
/// Zellij 0.45 defaults `stacked_pane_list` on, which removes collapsed stack
/// members from both pane manifests and `list-panes`. RimZ keeps it off so
/// every agent pane remains observable; older hosts ignore the unknown option.
///
/// Zellij 0.44 drops `attach --create-background … options` flags while
/// starting the detached server, and fixes serialization and metadata behavior
/// at first-client initialization. The layout therefore carries those options
/// through the only configuration channel the detached server preserves.
pub(super) fn render_session_layout(
    opts: &SidebarPaneOptions,
    daemon: Option<&DaemonView>,
    resume: &[ResumeTab],
) -> Result<String> {
    // The explicit tabs instantiate on the detached background session at
    // birth; only the `new_tab_template` waits for an attached client. Both
    // carry the same seed derived from the launch probe.
    let sidebar = sidebar_pane_kdl(opts, None, opts.target.percent())?;

    // The daemon tab leads, when present.
    let daemon_tab = match daemon {
        Some(daemon) => {
            let daemon_name = kdl_string(&daemon.name)?;
            let body = render_daemon_columns(
                &sidebar,
                &daemon.content,
                &daemon.hosts,
                &daemon.loop_panel,
                opts.target.percent(),
                8,
            )?;
            format!(
                r#"    tab name={daemon_name} {{
{body}
        {COMPACT_BAR_KDL}
    }}
"#,
            )
        }
        None => String::new(),
    };

    // One tab per resumed channel, focusing the first (most-recently-active).
    let mut agent_tabs = String::new();
    for (index, tab) in resume.iter().enumerate() {
        let tab_name = kdl_string(&tab.label)?;
        let agent_panes = if tab.layout.columns.is_empty() {
            render_command_pane(
                &crate::harness::launch::channel_label_shell_argv(
                    &opts.workspace_id,
                    &opts.project_root,
                    &tab.cwd,
                    &tab.label,
                ),
                &tab.cwd,
                true,
                16,
                None,
            )?
        } else {
            let mut focused = false;
            let mut columns = String::new();
            for column in &tab.layout.columns {
                columns.push_str(&render_tab_column(column, &tab.cwd, &mut focused, 16)?);
            }
            columns
        };
        let body = render_sidebar_work_area(&sidebar, &agent_panes, 8);
        let focus_attr = if index == 0 { " focus=true" } else { "" };
        agent_tabs.push_str(&format!(
            r#"    tab name={tab_name}{focus_attr} {{
{body}
        {COMPACT_BAR_KDL}
    }}
"#,
        ));
    }

    // The free working terminal: focused only when no resumed agent took focus.
    let work_focus = if resume.is_empty() { " focus=true" } else { "" };
    let work_pane = render_plain_terminal_pane(16);
    let work_body = render_sidebar_work_area(&sidebar, &work_pane, 8);
    let new_tab_pane = render_plain_terminal_pane(16);
    let new_tab_body = render_sidebar_work_area(&sidebar, &new_tab_pane, 8);
    Ok(format!(
        r#"layout {{
    new_tab_template {{
{new_tab_body}
        {COMPACT_BAR_KDL}
    }}
{daemon_tab}{agent_tabs}    tab{work_focus} {{
{work_body}
        {COMPACT_BAR_KDL}
    }}
}}
session_serialization {session_serialization}
disable_session_metadata {disable_session_metadata}
stacked_pane_list false
"#,
        session_serialization = opts.config.zellij.session_serialization,
        disable_session_metadata = opts.config.zellij.disable_session_metadata,
    ))
}

fn kdl_string(value: &str) -> Result<String> {
    serde_json::to_string(value).map_err(|err| MuxErr::Output {
        program: "zellij".to_owned(),
        reason: format!("escaping layout string: {err}"),
    })
}

/// A tab layout born `sidebar | content | hosts…`: the global sidebar docked on
/// the left, content panes in the middle, daemon hosts stacked on the right when
/// present, and the compact-bar below. The render and daemon columns share the
/// sidebar width verdict; content absorbs the center remainder.
/// Supplying this as `new-tab --layout` overrides the session template, so the
/// sidebar is spelled out here rather than inherited. The sidebar runs from its
/// own worktree cwd and each command pane from its own `cwd`. Every command pane
/// closes with its process (`close_on_exit true`): an exit means it is gone.
pub(super) fn render_background_view_layout(opts: &BackgroundViewOptions) -> Result<String> {
    // `new-tab --layout` does not set a tab `--default-cwd`, so the sidebar pane
    // spells its own worktree cwd; each command pane carries its own. The view
    // can be opened before the launch attaches a client, so it sizes
    // detached-safe.
    let sidebar = sidebar_pane_kdl(
        &opts.sidebar,
        Some(&opts.sidebar.cwd),
        opts.sidebar.target.percent(),
    )?;
    let body = render_daemon_columns(
        &sidebar,
        &opts.view.content,
        &opts.view.hosts,
        &opts.view.loop_panel,
        opts.sidebar.target.percent(),
        4,
    )?;
    // The body (sidebar + work area) is a nested vertical split above the
    // one-row compact-bar.
    Ok(format!(
        r#"layout {{
{body}
    {COMPACT_BAR_KDL}
}}
"#,
    ))
}

/// A user-opened tab born with the global sidebar docked on the left and the
/// caller's columns to the right. Columns split vertically; rows inside a
/// column either tile horizontally or render as a Zellij stack. Every command
/// pane shares `opts.sidebar.cwd`. The sidebar uses the percentage derived from the
/// current live target because Zellij resize-pins fixed-size layout panes.
pub(super) fn render_tab_layout(opts: &TabOptions, sidebar_percent: u16) -> Result<String> {
    opts.panes.leading_pane("zellij")?;
    if !opts.dock_sidebar {
        return render_undocked_tab_layout(opts);
    }
    let sidebar = sidebar_pane_kdl(&opts.sidebar, Some(&opts.sidebar.cwd), sidebar_percent)?;
    let mut focused = false;
    let mut columns = String::new();
    for column in &opts.panes.columns {
        columns.push_str(&render_tab_column(
            column,
            &opts.sidebar.cwd,
            &mut focused,
            12,
        )?);
    }
    let body = render_sidebar_work_area(&sidebar, &columns, 4);
    Ok(format!(
        r#"layout {{
{body}
    {COMPACT_BAR_KDL}
}}
"#,
    ))
}

fn render_undocked_tab_layout(opts: &TabOptions) -> Result<String> {
    let mut focused = false;
    let mut columns = String::new();
    for column in &opts.panes.columns {
        columns.push_str(&render_tab_column(
            column,
            &opts.sidebar.cwd,
            &mut focused,
            8,
        )?);
    }
    Ok(format!(
        r#"layout {{
    pane split_direction="vertical" {{
{columns}    }}
    {COMPACT_BAR_KDL}
}}
"#,
    ))
}

fn render_daemon_columns(
    sidebar: &str,
    content: &[HostPane],
    daemons: &[HostPane],
    loop_panel: &HostPane,
    width_percent: u16,
    indent: usize,
) -> Result<String> {
    let base = " ".repeat(indent);
    let child = " ".repeat(indent + 4);
    let content_col = render_content_column(content, false, indent + 4)?;
    let daemon_column = render_daemon_column(daemons, loop_panel, width_percent, indent + 4)?;
    Ok(format!(
        r#"{base}pane split_direction="vertical" {{
{child}{sidebar}
{content_col}{daemon_column}{base}}}
"#,
    ))
}

fn render_content_column(content: &[HostPane], focus_first: bool, indent: usize) -> Result<String> {
    match content {
        [] => Err(MuxErr::Output {
            program: "zellij".to_owned(),
            reason: "daemon view has no content panes".to_owned(),
        }),
        [pane] => render_managed_command_pane(&pane.argv, &pane.cwd, focus_first, indent, None),
        panes => {
            let mut rendered = String::new();
            for (index, pane) in panes.iter().enumerate() {
                rendered.push_str(&render_managed_command_pane(
                    &pane.argv,
                    &pane.cwd,
                    focus_first && index == 0,
                    indent + 4,
                    None,
                )?);
            }
            let base = " ".repeat(indent);
            Ok(format!(
                r#"{base}pane split_direction="horizontal" {{
{rendered}{base}}}
"#,
            ))
        }
    }
}

fn render_daemon_column(
    daemons: &[HostPane],
    loop_panel: &HostPane,
    width_percent: u16,
    indent: usize,
) -> Result<String> {
    let size = format!("{}%", width_percent);
    match daemons {
        [] => render_managed_command_pane(
            &loop_panel.argv,
            &loop_panel.cwd,
            true,
            indent,
            Some(&size),
        ),
        daemons => {
            let mut rendered = String::new();
            for (index, daemon) in daemons.iter().enumerate() {
                rendered.push_str(&render_managed_command_pane(
                    &daemon.argv,
                    &daemon.cwd,
                    index == 0,
                    indent + 4,
                    None,
                )?);
            }
            rendered.push_str(&render_managed_command_pane(
                &loop_panel.argv,
                &loop_panel.cwd,
                false,
                indent + 4,
                None,
            )?);
            let base = " ".repeat(indent);
            let size = kdl_string(&size)?;
            Ok(format!(
                r#"{base}pane size={size} split_direction="horizontal" {{
{rendered}{base}}}
"#,
            ))
        }
    }
}

fn render_sidebar_work_area(sidebar: &str, work_panes: &str, indent: usize) -> String {
    let base = " ".repeat(indent);
    let child = " ".repeat(indent + 4);
    format!(
        r#"{base}pane split_direction="vertical" {{
{child}{sidebar}
{child}pane split_direction="vertical" {{
{work_panes}{child}}}
{base}}}
"#,
    )
}

fn render_plain_terminal_pane(indent: usize) -> String {
    format!("{}pane focus=true\n", " ".repeat(indent))
}

fn render_tab_column(
    column: &LayoutColumn,
    cwd: &Path,
    focused: &mut bool,
    indent: usize,
) -> Result<String> {
    let (first, rows) = column.split_leading("zellij")?;
    if rows.is_empty() {
        let focus = !*focused;
        *focused = true;
        return render_command_pane(&first.argv, cwd, focus, indent, None);
    }
    let mut rendered = String::new();
    for pane in std::iter::once(first).chain(rows) {
        let focus = !*focused;
        *focused = true;
        rendered.push_str(&render_command_pane(
            &pane.argv,
            cwd,
            focus,
            indent + 4,
            None,
        )?);
    }
    let base = " ".repeat(indent);
    let container = if column.stacked {
        r#"pane stacked=true"#
    } else {
        r#"pane split_direction="horizontal""#
    };
    Ok(format!(
        r#"{base}{container} {{
{rendered}{base}}}
"#,
    ))
}

/// One command pane in a tab's right side (`argv` run in `cwd`), indented to
/// nest under the split that contains it. Born unsuspended and closing with its
/// process — an exit means the pane is gone. `focus` pins the tab's focus on it;
/// `size` pins a daemon edge column to the same percentage verdict as the
/// sidebar while content remains sizeless and absorbs the center remainder.
fn render_command_pane(
    argv: &[String],
    cwd: &Path,
    focus: bool,
    indent: usize,
    size: Option<&str>,
) -> Result<String> {
    render_named_command_pane(argv, cwd, focus, indent, size, None)
}

fn render_managed_command_pane(
    argv: &[String],
    cwd: &Path,
    focus: bool,
    indent: usize,
    size: Option<&str>,
) -> Result<String> {
    render_named_command_pane(argv, cwd, focus, indent, size, Some(&argv.join(" ")))
}

fn render_named_command_pane(
    argv: &[String],
    cwd: &Path,
    focus: bool,
    indent: usize,
    size: Option<&str>,
    name: Option<&str>,
) -> Result<String> {
    let (program, args) = argv.split_first().ok_or_else(|| MuxErr::Output {
        program: "zellij".to_owned(),
        reason: "command pane has no program".to_owned(),
    })?;
    let program = kdl_string(program)?;
    let cwd = kdl_string(&cwd.to_string_lossy())?;
    let size_attr = match size {
        Some(size) => format!(" size={}", kdl_string(size)?),
        None => String::new(),
    };
    let name_attr = match name {
        Some(name) => format!(" name={}", kdl_string(name)?),
        None => String::new(),
    };
    let focus_attr = if focus { " focus=true" } else { "" };
    let args_line = if args.is_empty() {
        String::new()
    } else {
        let rendered = args
            .iter()
            .map(|arg| kdl_string(arg))
            .collect::<Result<Vec<_>>>()?
            .join(" ");
        format!("\n{}args {rendered}", " ".repeat(indent + 4))
    };
    let base = " ".repeat(indent);
    let child = " ".repeat(indent + 4);
    Ok(format!(
        r#"{base}pane{size_attr}{focus_attr}{name_attr} cwd={cwd} {{
{child}command {program}{args_line}
{child}start_suspended false
{child}close_on_exit true
{base}}}
"#,
    ))
}

#[cfg(test)]
mod tests;
