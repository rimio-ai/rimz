//! KDL layout rendering for the Zellij backend: session birth (working tab,
//! daemon view, resumed agents), the `new-tab --layout` background view, and
//! the RAII temp file the async `--default-layout` parse reads from. Pure
//! `&options → String` renderers — no backend state, no subprocess.

use std::num::NonZeroU16;
use std::path::{Path, PathBuf};

use super::SIDEBAR_PANE_NAME;
use crate::mux::{
    BackgroundViewOptions, DaemonView, HostPane, MuxErr, PaneCmd, Result, ResumePane,
    SidebarPaneOptions, TabOptions,
};

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
    let workspace_id = kdl_string(opts.workspace_id.as_str())?;
    let session_name = kdl_string(&opts.session_name)?;
    // The layout grammar spells a fixed size (bare integer, columns) or a
    // percentage (quoted string) — the launch path already resolved the width
    // verdict via `SidebarWidth::birth_size`, and `geometry` picks the
    // spelling that survives where the pane instantiates ([`BirthGeometry`]).
    let size = match geometry {
        BirthGeometry::Attached => fixed_cols.unwrap_or(opts.birth_size.cols).to_string(),
        BirthGeometry::Detached => kdl_string(&format!("{}%", opts.birth_size.percent))?,
    };
    let pane_name = kdl_string(SIDEBAR_PANE_NAME)?;
    let cwd_attr = match cwd {
        Some(cwd) => format!(" cwd={}", kdl_string(&cwd.to_string_lossy())?),
        None => String::new(),
    };
    Ok(format!(
        r#"pane size={size} name={pane_name}{cwd_attr} {{
            command {rimz_bin}
            args "sidebar" "serve" "--mux" "zellij" "--workspace-id" {workspace_id} "--session-name" {session_name}
            start_suspended false
            close_on_exit true
        }}"#,
    ))
}

pub(super) fn render_sidebar_layout(opts: &SidebarPaneOptions) -> Result<String> {
    let sidebar = sidebar_pane_kdl(opts, None, BirthGeometry::Detached, None)?;
    let new_tab_sidebar = sidebar_pane_kdl(opts, None, BirthGeometry::Attached, None)?;
    // Every tab carries the same shape — the sidebar on the left and a focused
    // terminal on the right — in the spelling that fits where it instantiates:
    // the `default_tab_template` wraps the explicit birth tab on the detached
    // session, and the `new_tab_template` sizes each tab the user opens from an
    // attached client ([`BirthGeometry`]). The bare `tab` node is load-bearing:
    // on Zellij 0.44.3 a layout carrying a `new_tab_template` but no tab node
    // kills the background session instead of creating the implicit first tab.
    // The working cwd comes from the session's `--default-cwd`, so panes need
    // no `cwd`.
    //
    // The terminal is an explicit `pane focus=true`, not Zellij's `children`
    // placeholder. A nested `children` template has version-sensitive behavior:
    // on Zellij 0.44.3 it creates the right terminal but leaves focus stranded
    // on the sidebar in newly-created tabs. Spelling out the terminal makes the
    // product contract explicit and pins focus on the user's working pane.
    Ok(format!(
        r#"layout {{
    default_tab_template {{
        pane split_direction="vertical" {{
            {sidebar}
            pane focus=true
        }}
        {COMPACT_BAR_KDL}
    }}
    new_tab_template {{
        pane split_direction="vertical" {{
            {new_tab_sidebar}
            pane focus=true
        }}
        {COMPACT_BAR_KDL}
    }}
    tab focus=true
}}
"#,
    ))
}

/// The session-birth layout for a room that leads with a daemon view and/or
/// re-seeds prior agents. Zellij can't reorder tabs or add command panes after
/// birth, so the order and content are fixed here: the daemon tab
/// (`sidebar | hosts…`, first, when present), then one tab per resumed agent
/// (`sidebar | agent`), then the working tab (`sidebar | terminal`). Focus lands
/// on the most-recently-active resumed agent when there is one, else on the
/// working terminal — so attach drops the user straight onto a restored agent.
/// A `new_tab_template` — distinct from `default_tab_template`, applying only to
/// tabs the user opens *later* — carries the `sidebar | terminal` shape so future
/// tabs keep their sidebar and terminal focus without the `children`
/// focus-strand bug ([`render_sidebar_layout`] explains why `children` is
/// avoided). All panes inherit the session's `--default-cwd` except the daemon
/// hosts and resumed agents, which carry their own worktree cwd.
pub(super) fn render_session_layout(
    opts: &SidebarPaneOptions,
    daemon: Option<&DaemonView>,
    resume: &[ResumePane],
) -> Result<String> {
    // The explicit tabs instantiate on the detached background session at
    // birth; only the `new_tab_template` waits for an attached client.
    let sidebar = sidebar_pane_kdl(opts, None, BirthGeometry::Detached, None)?;
    let new_tab_sidebar = sidebar_pane_kdl(opts, None, BirthGeometry::Attached, None)?;

    // The daemon tab leads, when present.
    let daemon_tab = match daemon {
        Some(daemon) => {
            if daemon.hosts.is_empty() {
                return Err(MuxErr::Output {
                    program: "zellij".to_owned(),
                    reason: "daemon view has no host panes".to_owned(),
                });
            }
            let daemon_name = kdl_string(&daemon.name)?;
            let host_panes = daemon
                .hosts
                .iter()
                .enumerate()
                .map(|(index, host)| render_host_pane(host, index == 0))
                .collect::<Result<String>>()?;
            format!(
                r#"    tab name={daemon_name} {{
        pane split_direction="vertical" {{
            {sidebar}
{host_panes}        }}
        {COMPACT_BAR_KDL}
    }}
"#,
            )
        }
        None => String::new(),
    };

    // One tab per resumed agent, focusing the first (most-recently-active).
    let mut agent_tabs = String::new();
    for (index, pane) in resume.iter().enumerate() {
        let tab_name = kdl_string(&pane.label)?;
        let agent_pane = render_command_pane(&pane.command, &pane.cwd, true)?;
        let focus_attr = if index == 0 { " focus=true" } else { "" };
        agent_tabs.push_str(&format!(
            r#"    tab name={tab_name}{focus_attr} {{
        pane split_direction="vertical" {{
            {sidebar}
{agent_pane}        }}
        {COMPACT_BAR_KDL}
    }}
"#,
        ));
    }

    // The free working terminal: focused only when no resumed agent took focus.
    let work_focus = if resume.is_empty() { " focus=true" } else { "" };
    Ok(format!(
        r#"layout {{
    new_tab_template {{
        pane split_direction="vertical" {{
            {new_tab_sidebar}
            pane focus=true
        }}
        {COMPACT_BAR_KDL}
    }}
{daemon_tab}{agent_tabs}    tab{work_focus} {{
        pane split_direction="vertical" {{
            {sidebar}
            pane focus=true
        }}
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

/// A tab layout born `sidebar | hosts…`: the global sidebar docked on the left,
/// the view's hosts side by side to its right (the first focused), and the
/// compact-bar below — mirroring the session's working-tab template
/// ([`render_sidebar_layout`]). Supplying this as `new-tab --layout` overrides
/// that template, so the sidebar is spelled out here rather than inherited. The
/// sidebar runs from its own worktree cwd and each host from its own `cwd`. Every
/// host closes with its process (`close_on_exit true`): an exit means it is gone.
pub(super) fn render_background_view_layout(opts: &BackgroundViewOptions) -> Result<String> {
    if opts.hosts.is_empty() {
        return Err(MuxErr::Output {
            program: "zellij".to_owned(),
            reason: "background view has no host panes".to_owned(),
        });
    }
    // `new-tab --layout` does not set a tab `--default-cwd`, so the sidebar pane
    // spells its own worktree cwd; each host carries its own. The view can be
    // opened before the launch attaches a client, so it sizes detached-safe.
    let sidebar = sidebar_pane_kdl(
        &opts.sidebar,
        Some(&opts.sidebar.cwd),
        BirthGeometry::Detached,
        None,
    )?;
    let host_panes = opts
        .hosts
        .iter()
        .enumerate()
        .map(|(index, host)| render_host_pane(host, index == 0))
        .collect::<Result<String>>()?;
    // The body (sidebar + hosts) is a nested vertical split above the one-row
    // compact-bar.
    Ok(format!(
        r#"layout {{
    pane split_direction="vertical" {{
        {sidebar}
{host_panes}    }}
    {COMPACT_BAR_KDL}
}}
"#,
    ))
}

/// A user-opened tab born with the global sidebar docked on the left and the
/// caller's columns to the right. Columns split vertically; rows inside a
/// column split horizontally. Every command pane shares `opts.cwd`.
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
    let sidebar = sidebar_pane_kdl(
        &opts.sidebar,
        Some(&opts.sidebar.cwd),
        BirthGeometry::Attached,
        template_sidebar_cols,
    )?;
    let mut focused = false;
    let mut columns = String::new();
    for column in &opts.panes.columns {
        columns.push_str(&render_tab_column(column, &opts.cwd, &mut focused)?);
    }
    Ok(format!(
        r#"layout {{
    pane split_direction="vertical" {{
        {sidebar}
{columns}    }}
    {COMPACT_BAR_KDL}
}}
"#,
    ))
}

fn render_tab_column(column: &[PaneCmd], cwd: &Path, focused: &mut bool) -> Result<String> {
    match column {
        [] => Err(MuxErr::Output {
            program: "zellij".to_owned(),
            reason: "tab layout has an empty column".to_owned(),
        }),
        [pane] => {
            let focus = !*focused;
            *focused = true;
            render_command_pane(&pane.argv, cwd, focus)
        }
        rows => {
            let mut rendered = String::new();
            for pane in rows {
                let focus = !*focused;
                *focused = true;
                rendered.push_str(&render_command_pane(&pane.argv, cwd, focus)?);
            }
            Ok(format!(
                r#"        pane split_direction="horizontal" {{
{rendered}        }}
"#,
            ))
        }
    }
}

/// One command pane in a tab's right side (`argv` run in `cwd`), indented to
/// nest under the vertical split beside the sidebar. Born unsuspended and
/// closing with its process — an exit means the pane is gone. `focus` pins the
/// tab's focus on it. Shared by the daemon hosts and the resumed agents, so both
/// render identically.
fn render_command_pane(argv: &[String], cwd: &Path, focus: bool) -> Result<String> {
    let (program, args) = argv.split_first().ok_or_else(|| MuxErr::Output {
        program: "zellij".to_owned(),
        reason: "command pane has no program".to_owned(),
    })?;
    let program = kdl_string(program)?;
    let cwd = kdl_string(&cwd.to_string_lossy())?;
    let focus_attr = if focus { " focus=true" } else { "" };
    let args_line = if args.is_empty() {
        String::new()
    } else {
        let rendered = args
            .iter()
            .map(|arg| kdl_string(arg))
            .collect::<Result<Vec<_>>>()?
            .join(" ");
        format!("\n            args {rendered}")
    };
    Ok(format!(
        r#"        pane{focus_attr} cwd={cwd} {{
            command {program}{args_line}
            start_suspended false
            close_on_exit true
        }}
"#,
    ))
}

/// One host pane in the daemon view's right side. Thin wrapper over
/// [`render_command_pane`] for the daemon hosts.
fn render_host_pane(host: &HostPane, focus: bool) -> Result<String> {
    render_command_pane(&host.argv, &host.cwd, focus)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mux::SidebarWidth;

    #[test]
    fn sidebar_layout_carries_a_bottom_bar() {
        use crate::ids::WorkspaceId;
        let opts = SidebarPaneOptions {
            session_name: "rimz-bar".to_owned(),
            workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-bar")),
            project_root: PathBuf::from("/tmp/rimz-bar"),
            cwd: PathBuf::from("/tmp/rimz-bar"),
            width: SidebarWidth::default(),
            birth_size: SidebarWidth::default().birth_size(None),
            rimz_bin: PathBuf::from("/usr/bin/rimz"),
            replace_existing: false,
            config: crate::config::MultiplexerConfig::default(),
            resume_panes: Vec::new(),
        };
        let layout = render_sidebar_layout(&opts).expect("render layout");
        assert!(
            layout.contains("compact-bar"),
            "the sidebar layout overrides Zellij's default tab template, so it must \
             re-add a bottom bar plugin or the tab/status bar vanishes:\n{layout}",
        );
    }

    #[test]
    fn sidebar_layout_focuses_an_explicit_terminal_in_every_tab() {
        use crate::ids::WorkspaceId;
        let opts = SidebarPaneOptions {
            session_name: "rimz-focus".to_owned(),
            workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-focus")),
            project_root: PathBuf::from("/tmp/rimz-focus"),
            cwd: PathBuf::from("/tmp/rimz-focus"),
            width: SidebarWidth::default(),
            birth_size: SidebarWidth::default().birth_size(None),
            rimz_bin: PathBuf::from("/usr/bin/rimz"),
            replace_existing: false,
            config: crate::config::MultiplexerConfig::default(),
            resume_panes: Vec::new(),
        };
        let layout = render_sidebar_layout(&opts).expect("render layout");
        // The template must spell out the focused terminal instead of relying
        // on a nested `children` placeholder: every template-born tab needs a
        // right pane with focus, never a bare or focused sidebar.
        assert!(
            layout.contains("pane focus=true"),
            "the layout must focus an explicit terminal pane:\n{layout}",
        );
        assert!(
            !layout.contains("children"),
            "the layout must not depend on `children`: placeholder semantics \
             can misplace focus or omit the right terminal in template-born tabs:\n{layout}",
        );
        // The bare `tab` node is load-bearing: with a `new_tab_template`
        // present and no tab node, Zellij 0.44.3 kills the background session
        // instead of creating the implicit first tab.
        assert!(
            layout.contains("tab focus=true"),
            "the layout must carry an explicit birth tab alongside the \
             templates or the detached session dies:\n{layout}",
        );
    }

    #[test]
    fn sidebar_layout_pins_fixed_cols_attached_and_percent_detached() {
        use crate::ids::WorkspaceId;
        let opts = SidebarPaneOptions {
            session_name: "rimz-width".to_owned(),
            workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-width")),
            project_root: PathBuf::from("/tmp/rimz-width"),
            cwd: PathBuf::from("/tmp/rimz-width"),
            width: SidebarWidth::default(),
            // 30% of 120 is 36 ≤ the 72 cap — the under-cap verdict.
            birth_size: SidebarWidth::default().birth_size(Some(120)),
            rimz_bin: PathBuf::from("/usr/bin/rimz"),
            replace_existing: false,
            config: crate::config::MultiplexerConfig::default(),
            resume_panes: Vec::new(),
        };
        let layout = render_sidebar_layout(&opts).expect("render layout");
        // The birth tab spells the verdict's percentage share — a fixed size
        // wider than the detached session's default geometry kills the
        // session — and lands on the verdict when the launching client
        // attaches.
        assert!(
            layout.contains(r#"pane size="30%" name="rimz-sidebar""#),
            "the default_tab_template births detached, so the verdict is its \
             percentage share:\n{layout}",
        );
        // Tabs the user opens later instantiate at live geometry, so the
        // new_tab_template pins the verdict exactly, as a bare KDL integer —
        // even under the cap. A raw percentage here re-evaluates against
        // whatever geometry instantiates the tab, which is exactly how the
        // cap used to vanish from a session.
        assert!(
            layout.contains(r#"pane size=36 name="rimz-sidebar""#),
            "the new_tab_template instantiates attached, so it pins the fixed \
             verdict:\n{layout}",
        );
        // Past the cap the same split holds: ⌊72·100/340⌋ = 21% detached,
        // the fixed cap attached.
        let capped = SidebarPaneOptions {
            birth_size: SidebarWidth::default().birth_size(Some(340)),
            ..opts
        };
        let layout = render_sidebar_layout(&capped).expect("render layout");
        assert!(
            layout.contains(r#"pane size="21%" name="rimz-sidebar""#),
            "the default_tab_template births detached, so a capped width is \
             its derived percentage:\n{layout}",
        );
        assert!(
            layout.contains(r#"pane size=72 name="rimz-sidebar""#),
            "the new_tab_template instantiates attached, so a capped width is \
             the fixed `max_cols` cap:\n{layout}",
        );
        let new_tab_template = layout
            .split("new_tab_template")
            .nth(1)
            .expect("layout carries a new_tab_template");
        assert!(
            !new_tab_template.contains('%'),
            "the new_tab_template carries no percentage:\n{layout}",
        );
    }

    fn host(argv: &[&str], cwd: &str) -> HostPane {
        HostPane {
            argv: argv.iter().map(|arg| arg.to_string()).collect(),
            cwd: PathBuf::from(cwd),
        }
    }

    fn background_view_opts(hosts: Vec<HostPane>) -> BackgroundViewOptions {
        use crate::ids::WorkspaceId;
        BackgroundViewOptions {
            name: "rimzd".to_owned(),
            hosts,
            sidebar: SidebarPaneOptions {
                session_name: "rimz-bg".to_owned(),
                workspace_id: WorkspaceId::from_project_root(Path::new("/proj/root")),
                project_root: PathBuf::from("/proj/root"),
                cwd: PathBuf::from("/proj/worktree"),
                width: SidebarWidth::default(),
                birth_size: SidebarWidth::default().birth_size(None),
                rimz_bin: PathBuf::from("/usr/bin/rimz"),
                replace_existing: false,
                config: crate::config::MultiplexerConfig::default(),
                resume_panes: Vec::new(),
            },
        }
    }

    #[test]
    fn background_view_layout_runs_the_host_beside_the_sidebar() {
        let layout = render_background_view_layout(&background_view_opts(vec![host(
            &["claude", "remote-control", "--spawn", "worktree"],
            "/proj/root",
        )]))
        .expect("render background view layout");
        // The host is the focused right pane, born unsuspended, and closes with
        // its process — an exit means the host is gone.
        assert!(layout.contains(r#"command "claude""#), "{layout}");
        assert!(
            layout.contains(r#"args "remote-control" "--spawn" "worktree""#),
            "{layout}",
        );
        assert!(layout.contains("pane focus=true"), "{layout}");
        assert!(layout.contains("start_suspended false"), "{layout}");
        assert!(layout.contains("close_on_exit true"), "{layout}");
        // The global sidebar is docked on the left, running the renderer.
        assert!(layout.contains(r#"name="rimz-sidebar""#), "{layout}");
        assert!(layout.contains(r#""sidebar" "serve""#), "{layout}");
        // A bottom bar, mirroring the working-tab template.
        assert!(layout.contains("compact-bar"), "{layout}");
        // Each pane carries its own cwd: the sidebar from the worktree, the host
        // from the project root.
        assert!(layout.contains(r#"cwd="/proj/worktree""#), "{layout}");
        assert!(layout.contains(r#"cwd="/proj/root""#), "{layout}");
    }

    #[test]
    fn background_view_layout_stacks_two_hosts_focusing_the_first() {
        let layout = render_background_view_layout(&background_view_opts(vec![
            host(&["claude", "remote-control"], "/proj/root"),
            host(
                &["/usr/bin/rimz", "codex", "app-server", "serve"],
                "/proj/worktree",
            ),
        ]))
        .expect("render background view layout");
        // Both hosts are present beside the sidebar.
        assert!(layout.contains(r#"command "claude""#), "{layout}");
        assert!(layout.contains(r#"command "/usr/bin/rimz""#), "{layout}");
        assert!(
            layout.contains(r#"args "codex" "app-server" "serve""#),
            "{layout}",
        );
        // Exactly one pane takes focus — the first host (the interactive Claude
        // host), never the broker.
        assert_eq!(layout.matches("focus=true").count(), 1, "{layout}");
    }

    #[test]
    fn background_view_layout_rejects_no_hosts() {
        assert!(render_background_view_layout(&background_view_opts(vec![])).is_err());
    }

    #[test]
    fn tab_layout_renders_sidebar_columns_rows_and_focus() {
        let sidebar = background_view_opts(vec![]).sidebar;
        let opts = TabOptions {
            session_name: sidebar.session_name.clone(),
            title: "review".to_owned(),
            cwd: PathBuf::from("/proj/worktree"),
            panes: crate::mux::LayoutPanes {
                columns: vec![
                    vec![PaneCmd {
                        argv: vec!["/bin/sh".to_owned()],
                    }],
                    vec![
                        PaneCmd {
                            argv: vec!["codex".to_owned()],
                        },
                        PaneCmd {
                            argv: vec!["/bin/sh".to_owned(), "-l".to_owned()],
                        },
                    ],
                ],
            },
            focus: true,
            sidebar,
        };
        let layout = render_tab_layout(&opts, None).expect("render tab layout");
        assert!(
            layout.contains(r#"pane size=72 name="rimz-sidebar""#),
            "custom tab layouts instantiate from a live client, so the \
             sidebar must pin the fixed max-cols verdict instead of \
             re-evaluating a percentage against wide terminals:\n{layout}",
        );
        assert!(
            !layout.contains(r#"size="30%""#),
            "custom tab layouts must not use detached percentage sizing:\n{layout}",
        );
        assert!(layout.contains("compact-bar"), "{layout}");
        assert!(
            layout.contains(r#"pane split_direction="horizontal""#),
            "{layout}"
        );
        assert!(layout.contains(r#"command "codex""#), "{layout}");
        assert_eq!(layout.matches("focus=true").count(), 1, "{layout}");
    }

    #[test]
    fn tab_layout_can_mirror_the_new_tab_template_sidebar_width() {
        let sidebar = background_view_opts(vec![]).sidebar;
        let opts = TabOptions {
            session_name: sidebar.session_name.clone(),
            title: "review".to_owned(),
            cwd: PathBuf::from("/proj/worktree"),
            panes: crate::mux::LayoutPanes {
                columns: vec![vec![PaneCmd {
                    argv: vec!["/bin/sh".to_owned()],
                }]],
            },
            focus: true,
            sidebar,
        };
        let layout = render_tab_layout(&opts, NonZeroU16::new(60)).expect("render tab layout");
        assert!(
            layout.contains(r#"pane size=60 name="rimz-sidebar""#),
            "custom tab layouts must be able to mirror the live \
             new_tab_template instead of this command's pane-width probe:\n{layout}",
        );
    }

    fn daemon_view(hosts: Vec<HostPane>) -> DaemonView {
        DaemonView {
            name: "rimzd".to_owned(),
            hosts,
        }
    }

    fn resume_pane(label: &str, argv: &[&str], cwd: &str) -> ResumePane {
        ResumePane {
            command: argv.iter().map(|arg| arg.to_string()).collect(),
            cwd: PathBuf::from(cwd),
            label: label.to_owned(),
        }
    }

    #[test]
    fn session_layout_seeds_resumed_agents_focusing_the_first() {
        let opts = background_view_opts(vec![]).sidebar;
        let resume = vec![
            resume_pane(
                "claude:feature",
                &["claude", "--resume", "sess-1"],
                "/proj/feature",
            ),
            resume_pane("codex:main", &["codex", "resume", "sess-2"], "/proj/main"),
        ];
        let layout = render_session_layout(&opts, None, &resume).expect("render resume layout");
        // Each agent runs its resume CLI in its own worktree, born unsuspended.
        assert!(layout.contains(r#"command "claude""#), "{layout}");
        assert!(layout.contains(r#"args "--resume" "sess-1""#), "{layout}");
        assert!(layout.contains(r#"command "codex""#), "{layout}");
        assert!(layout.contains(r#"args "resume" "sess-2""#), "{layout}");
        assert!(layout.contains(r#"cwd="/proj/feature""#), "{layout}");
        assert!(layout.contains(r#"cwd="/proj/main""#), "{layout}");
        assert!(layout.contains("start_suspended false"), "{layout}");
        // One tab per agent, named by label; the first (most-recent) takes focus.
        assert!(
            layout.contains(r#"tab name="claude:feature" focus=true"#),
            "the freshest resumed agent leads:\n{layout}",
        );
        assert!(
            !layout.contains(r#"tab name="codex:main" focus=true"#),
            "only the first resumed tab is focused:\n{layout}",
        );
        // A free working terminal tab still exists, unfocused (an agent has focus).
        assert!(
            layout.contains("    tab {"),
            "a bare working terminal tab remains:\n{layout}",
        );
        // Future user tabs inherit the sidebar+terminal template, no `children`.
        assert!(layout.contains("new_tab_template"), "{layout}");
        assert!(!layout.contains("children"), "{layout}");
    }

    #[test]
    fn session_layout_without_daemon_or_resume_focuses_the_working_tab() {
        let opts = background_view_opts(vec![]).sidebar;
        let layout = render_session_layout(&opts, None, &[]).expect("render layout");
        // No agents, no daemon: the working terminal tab takes focus and there
        // are no named daemon/agent tabs to seed.
        assert!(layout.contains("tab focus=true"), "{layout}");
        assert!(
            !layout.contains("tab name="),
            "no daemon or agent tabs without a daemon or resume set:\n{layout}",
        );
    }

    #[test]
    fn session_layout_with_daemon_leads_with_the_daemon_tab() {
        let bg = background_view_opts(vec![
            host(&["claude", "remote-control"], "/proj/root"),
            host(
                &["/usr/bin/rimz", "codex", "app-server", "serve"],
                "/proj/worktree",
            ),
        ]);
        let layout = render_session_layout(&bg.sidebar, Some(&daemon_view(bg.hosts.clone())), &[])
            .expect("render session layout with daemon");
        // The daemon tab is declared first — before the focused working tab — so
        // it leads. Zellij fixes tab order at birth (it can't reorder later).
        let daemon_at = layout.find(r#"tab name="rimzd""#).expect("daemon tab");
        let work_at = layout.find("tab focus=true").expect("working tab");
        assert!(
            daemon_at < work_at,
            "daemon tab must precede the working tab\n{layout}",
        );
        // Future user tabs inherit a sidebar + focused terminal via the
        // `new_tab_template`, which (unlike `default_tab_template` with explicit
        // tabs) needs no `children` and so dodges the focus-strand bug.
        assert!(layout.contains("new_tab_template"), "{layout}");
        assert!(!layout.contains("children"), "{layout}");
        // Both hosts and the sidebar are present beside each other.
        assert!(layout.contains(r#"command "claude""#), "{layout}");
        assert!(
            layout.contains(r#"args "codex" "app-server" "serve""#),
            "{layout}",
        );
        assert!(layout.contains(r#"name="rimz-sidebar""#), "{layout}");
        assert!(layout.contains("compact-bar"), "{layout}");
        // The host that leads the daemon view runs from the project root; the
        // sidebars inherit the session `--default-cwd`, so they carry no cwd.
        assert!(layout.contains(r#"cwd="/proj/root""#), "{layout}");
    }

    #[test]
    fn session_layout_with_daemon_rejects_no_hosts() {
        assert!(
            render_session_layout(
                &background_view_opts(vec![]).sidebar,
                Some(&daemon_view(vec![])),
                &[],
            )
            .is_err()
        );
    }

    #[test]
    fn sidebar_layout_starts_the_sidebar_without_a_run_prompt() {
        use crate::ids::WorkspaceId;
        let opts = SidebarPaneOptions {
            session_name: "rimz-run".to_owned(),
            workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-run")),
            project_root: PathBuf::from("/tmp/rimz-run"),
            cwd: PathBuf::from("/tmp/rimz-run"),
            width: SidebarWidth::default(),
            birth_size: SidebarWidth::default().birth_size(None),
            rimz_bin: PathBuf::from("/usr/bin/rimz"),
            replace_existing: false,
            config: crate::config::MultiplexerConfig::default(),
            resume_panes: Vec::new(),
        };
        let layout = render_sidebar_layout(&opts).expect("render layout");
        assert!(
            layout.contains("start_suspended false"),
            "Zellij command panes default to a run prompt unless the layout \
             starts them explicitly:\n{layout}",
        );
        assert!(
            !layout.contains("start_suspended true"),
            "the sidebar pane must never be born suspended:\n{layout}",
        );
    }
}
