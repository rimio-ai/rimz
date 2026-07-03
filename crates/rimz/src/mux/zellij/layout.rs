//! KDL layout rendering for the Zellij backend: session birth (working tab,
//! daemon view, resumed agents), the `new-tab --layout` background view, and
//! the RAII temp file the async `--default-layout` parse reads from. Pure
//! `&options → String` renderers — no backend state, no subprocess.

use std::num::NonZeroU16;
use std::path::{Path, PathBuf};

use crate::ids::MuxName;
use crate::mux::{
    BackgroundViewOptions, DaemonView, HostPane, MuxErr, PaneCmd, Result, ResumeTab,
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

/// Which geometry a layout's panes instantiate at, picking the spelling of
/// the [`BirthSize`](crate::mux::BirthSize) verdict. `Detached` covers panes that can materialize on
/// the background session's small default geometry — session-birth tabs and
/// `new-tab --layout` views — where a fixed size wider than that geometry
/// kills the session; they spell the verdict's percentage share of the probed
/// terminal and land on the verdict when the launching client attaches.
/// `Attached` covers panes only an attached client instantiates — the
/// `new_tab_template` behind every tab the user opens — which pin the
/// verdict's fixed columns exactly, whatever the live geometry. (A client
/// narrower than the fixed width refuses the new tab until widened.)
#[derive(Clone, Copy)]
enum BirthGeometry {
    Detached,
    Attached,
}

/// The left `rimz sidebar serve` pane every Zellij view carries, as a KDL `pane`
/// block. `cwd` is spelled only when the pane can't inherit the session's
/// `--default-cwd` — the `new-tab --layout` path ([`render_background_view_layout`]).
/// Birth layouts set `--default-cwd` and pass `None`.
fn sidebar_pane_kdl(
    opts: &SidebarPaneOptions,
    cwd: Option<&Path>,
    geometry: BirthGeometry,
    fixed_cols: Option<NonZeroU16>,
) -> Result<String> {
    let rimz_bin = kdl_string(&opts.rimz_bin.to_string_lossy())?;
    // The layout grammar spells a fixed size (bare integer, columns) or a
    // percentage (quoted string) — the launch path already resolved the width
    // verdict via `SidebarWidth::birth_size`, and `geometry` picks the
    // spelling that survives where the pane instantiates ([`BirthGeometry`]).
    let size = match geometry {
        BirthGeometry::Attached => fixed_cols.unwrap_or(opts.birth_size.cols).to_string(),
        BirthGeometry::Detached => kdl_string(&format!("{}%", opts.birth_size.percent))?,
    };
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
/// sidebar in newly-created tabs. The root `rimz-work-area` swap layout applies
/// to every birth and user-opened tab, pinning the sidebar and compact bar while
/// no-direction pane opens rebalance the work area. All panes inherit the
/// session's `--default-cwd` except the daemon hosts and resumed agents, which
/// carry their own worktree cwd.
pub(super) fn render_session_layout(
    opts: &SidebarPaneOptions,
    daemon: Option<&DaemonView>,
    resume: &[ResumeTab],
) -> Result<String> {
    // The explicit tabs instantiate on the detached background session at
    // birth; only the `new_tab_template` waits for an attached client.
    let sidebar = sidebar_pane_kdl(opts, None, BirthGeometry::Detached, None)?;
    let new_tab_sidebar = sidebar_pane_kdl(opts, None, BirthGeometry::Attached, None)?;

    // The daemon tab leads, when present.
    let daemon_tab = match daemon {
        Some(daemon) => {
            let daemon_name = kdl_string(&daemon.name)?;
            let body = render_daemon_columns(
                &sidebar,
                &daemon.content,
                &daemon.hosts,
                opts.birth_size.percent,
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
        let agent_panes = if tab.panes.is_empty() {
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
            tab.panes
                .iter()
                .enumerate()
                .map(|(pane_index, argv)| {
                    render_command_pane(argv, &tab.cwd, pane_index == 0, 16, None)
                })
                .collect::<Result<String>>()?
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
    let new_tab_body = render_sidebar_work_area(&new_tab_sidebar, &new_tab_pane, 8);
    let swap_layout = rimz_swap_layout_kdl(opts.birth_size.cols.get());
    Ok(format!(
        r#"layout {{
    new_tab_template {{
{new_tab_body}
        {COMPACT_BAR_KDL}
    }}
{swap_layout}
{daemon_tab}{agent_tabs}    tab{work_focus} {{
{work_body}
        {COMPACT_BAR_KDL}
    }}
}}
"#,
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
        BirthGeometry::Detached,
        None,
    )?;
    let body = render_daemon_columns(
        &sidebar,
        &opts.view.content,
        &opts.view.hosts,
        opts.sidebar.birth_size.percent,
        4,
    )?;
    let swap_layout = rimz_swap_layout_kdl(opts.sidebar.birth_size.cols.get());
    // The body (sidebar + work area) is a nested vertical split above the
    // one-row compact-bar.
    Ok(format!(
        r#"layout {{
{body}
    {COMPACT_BAR_KDL}
{swap_layout}
}}
"#,
    ))
}

/// A user-opened tab born with the global sidebar docked on the left and the
/// caller's columns to the right. Columns split vertically; rows inside a
/// column either tile horizontally or render as a Zellij stack. Every command
/// pane shares `opts.cwd`.
pub(super) fn render_tab_layout(
    opts: &TabOptions,
    template_sidebar_cols: Option<NonZeroU16>,
) -> Result<String> {
    if opts.panes.columns.is_empty() {
        return Err(MuxErr::Output {
            program: "zellij".to_owned(),
            reason: "tab layout has no columns".to_owned(),
        });
    }
    if !opts.dock_sidebar {
        return render_undocked_tab_layout(opts);
    }
    let sidebar = sidebar_pane_kdl(
        &opts.sidebar,
        Some(&opts.sidebar.cwd),
        BirthGeometry::Attached,
        template_sidebar_cols,
    )?;
    let sidebar_cols = template_sidebar_cols.unwrap_or(opts.sidebar.birth_size.cols);
    let mut focused = false;
    let mut columns = String::new();
    for column in &opts.panes.columns {
        columns.push_str(&render_tab_column(
            &column.panes,
            column.stacked,
            &opts.cwd,
            &mut focused,
            12,
        )?);
    }
    let body = render_sidebar_work_area(&sidebar, &columns, 4);
    Ok(format!(
        r#"layout {{
{body}
    {COMPACT_BAR_KDL}
{swap_layout}
}}
"#,
        swap_layout = rimz_swap_layout_kdl(sidebar_cols.get()),
    ))
}

fn render_undocked_tab_layout(opts: &TabOptions) -> Result<String> {
    let mut focused = false;
    let mut columns = String::new();
    for column in &opts.panes.columns {
        columns.push_str(&render_tab_column(
            &column.panes,
            column.stacked,
            &opts.cwd,
            &mut focused,
            8,
        )?);
    }
    let swap_layout = rimz_undocked_swap_layout();
    Ok(format!(
        r#"layout {{
    pane split_direction="vertical" {{
{columns}    }}
    {COMPACT_BAR_KDL}
{swap_layout}
}}
"#,
    ))
}

/// The swap-layout shape Zellij's `auto_layout` flow applies after native
/// no-direction pane opens and closes. The first tiled pane in a Rimz tab is the
/// sidebar and the final tiled pane is the compact-bar plugin; keeping both
/// explicit makes Zellij rebalance only the work area when users close one peer
/// pane and open another. The plugin slot is load-bearing: `max_panes` counts
/// plugin panes and Zellij assigns them to swap slots like terminals, so a
/// template without one re-tiles the bar into the work area as a full-size pane
/// (swap-layout semantics in `docs/externals/mux-adapter/zellij-reference.md`).
///
/// The first two templates keep the nicer one- and two-work-pane shapes; later
/// templates mirror Zellij's vanilla swap progression beside the pinned
/// sidebar: one main work pane plus an overflow stack, then multi-column grids.
/// The last tier stays unbounded so the sidebar and compact-bar stay pinned at
/// any pane count. Without that catch-all, Zellij's native no-direction
/// fallback splits the largest weighted-area pane, which is normally the
/// full-height sidebar.
fn rimz_swap_layout_kdl(sidebar_cols: u16) -> String {
    format!(
        r#"    swap_tiled_layout name="rimz-work-area" {{
        tab max_panes=3 {{
            pane split_direction="vertical" {{
                pane size={sidebar_cols}
                pane
            }}
            pane size=1 borderless=true {{
                plugin location="zellij:compact-bar"
            }}
        }}
        tab max_panes=4 {{
            pane split_direction="vertical" {{
                pane size={sidebar_cols}
                pane split_direction="vertical" {{
                    pane
                    pane
                }}
            }}
            pane size=1 borderless=true {{
                plugin location="zellij:compact-bar"
            }}
        }}
        tab max_panes=5 {{
            pane split_direction="vertical" {{
                pane size={sidebar_cols}
                pane
                pane split_direction="horizontal" {{
                    children
                }}
            }}
            pane size=1 borderless=true {{
                plugin location="zellij:compact-bar"
            }}
        }}
        tab max_panes=8 {{
            pane split_direction="vertical" {{
                pane size={sidebar_cols}
                pane split_direction="horizontal" {{
                    children
                }}
                pane split_direction="horizontal" {{
                    pane
                    pane
                    pane
                    pane
                }}
            }}
            pane size=1 borderless=true {{
                plugin location="zellij:compact-bar"
            }}
        }}
        tab {{
            pane split_direction="vertical" {{
                pane size={sidebar_cols}
                pane split_direction="horizontal" {{
                    children
                }}
                pane split_direction="horizontal" {{
                    pane
                    pane
                    pane
                    pane
                }}
                pane split_direction="horizontal" {{
                    pane
                    pane
                    pane
                    pane
                }}
            }}
            pane size=1 borderless=true {{
                plugin location="zellij:compact-bar"
            }}
        }}
    }}
"#,
    )
}

fn rimz_undocked_swap_layout() -> String {
    String::from(
        r#"    swap_tiled_layout name="rimz-work-area" {
        tab max_panes=2 {
            pane
            pane size=1 borderless=true {
                plugin location="zellij:compact-bar"
            }
        }
        tab max_panes=3 {
            pane split_direction="vertical" {
                pane
                pane
            }
            pane size=1 borderless=true {
                plugin location="zellij:compact-bar"
            }
        }
        tab max_panes=4 {
            pane split_direction="vertical" {
                pane
                pane split_direction="horizontal" {
                    children
                }
            }
            pane size=1 borderless=true {
                plugin location="zellij:compact-bar"
            }
        }
        tab max_panes=7 {
            pane split_direction="vertical" {
                pane split_direction="horizontal" {
                    children
                }
                pane split_direction="horizontal" {
                    pane
                    pane
                    pane
                    pane
                }
            }
            pane size=1 borderless=true {
                plugin location="zellij:compact-bar"
            }
        }
        tab {
            pane split_direction="vertical" {
                pane split_direction="horizontal" {
                    children
                }
                pane split_direction="horizontal" {
                    pane
                    pane
                    pane
                    pane
                }
                pane split_direction="horizontal" {
                    pane
                    pane
                    pane
                    pane
                }
            }
            pane size=1 borderless=true {
                plugin location="zellij:compact-bar"
            }
        }
    }
"#,
    )
}

fn render_daemon_columns(
    sidebar: &str,
    content: &[HostPane],
    daemons: &[HostPane],
    width_percent: u16,
    indent: usize,
) -> Result<String> {
    let base = " ".repeat(indent);
    let child = " ".repeat(indent + 4);
    let content_col = render_content_column(content, daemons.is_empty(), indent + 4)?;
    let daemon_column = render_daemon_column(daemons, width_percent, indent + 4)?;
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
        [pane] => render_command_pane(&pane.argv, &pane.cwd, focus_first, indent, None),
        panes => {
            let mut rendered = String::new();
            for (index, pane) in panes.iter().enumerate() {
                rendered.push_str(&render_command_pane(
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

fn render_daemon_column(daemons: &[HostPane], width_percent: u16, indent: usize) -> Result<String> {
    let size = format!("{}%", width_percent);
    match daemons {
        [] => Ok(String::new()),
        [daemon] => render_command_pane(&daemon.argv, &daemon.cwd, true, indent, Some(&size)),
        daemons => {
            let mut rendered = String::new();
            for (index, daemon) in daemons.iter().enumerate() {
                rendered.push_str(&render_command_pane(
                    &daemon.argv,
                    &daemon.cwd,
                    index == 0,
                    indent + 4,
                    None,
                )?);
            }
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
    column: &[PaneCmd],
    stacked: bool,
    cwd: &Path,
    focused: &mut bool,
    indent: usize,
) -> Result<String> {
    match column {
        [] => Err(MuxErr::Output {
            program: "zellij".to_owned(),
            reason: "tab layout has an empty column".to_owned(),
        }),
        [pane] => {
            let focus = !*focused;
            *focused = true;
            render_command_pane(&pane.argv, cwd, focus, indent, None)
        }
        rows => {
            let mut rendered = String::new();
            for pane in rows {
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
            let container = if stacked {
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
    }
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
        r#"{base}pane{size_attr}{focus_attr} cwd={cwd} {{
{child}command {program}{args_line}
{child}start_suspended false
{child}close_on_exit true
{base}}}
"#,
    ))
}

#[cfg(test)]
mod tests;
