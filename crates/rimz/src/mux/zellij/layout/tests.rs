use super::*;
use crate::ids::WorkspaceId;
use crate::mux::SidebarWidth;

fn sidebar_opts(
    session_name: &str,
    refresh_ms: Option<u16>,
    detected_cols: Option<u16>,
) -> SidebarPaneOptions {
    let width = SidebarWidth::default();
    SidebarPaneOptions {
        session_name: session_name.to_owned(),
        workspace_id: WorkspaceId::from_project_root(Path::new("/proj/root")),
        project_root: PathBuf::from("/proj/root"),
        extra_env: Default::default(),
        cwd: PathBuf::from("/proj/worktree"),
        width,
        birth_size: width.birth_size(detected_cols),
        detected_view_size: None,
        width_override: None,
        rimz_bin: PathBuf::from("/usr/bin/rimz"),
        replace_existing: false,
        pristine_birth: false,
        config: crate::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms,
    }
}

fn host(argv: &[&str], cwd: &str) -> HostPane {
    HostPane {
        argv: argv.iter().map(|arg| arg.to_string()).collect(),
        cwd: PathBuf::from(cwd),
    }
}

fn stats_host() -> HostPane {
    host(&["/usr/bin/rimz", "stats", "--refresh"], "/proj/worktree")
}

fn background_view_opts(hosts: Vec<HostPane>) -> BackgroundViewOptions {
    BackgroundViewOptions {
        view: DaemonView {
            name: "rimzd".to_owned(),
            content: vec![stats_host()],
            hosts,
            loop_panel: host(
                &["/usr/bin/rimz", "loop", "watch", "--hold"],
                "/proj/worktree",
            ),
        },
        sidebar: sidebar_opts("rimz-bg", None, None),
    }
}

fn daemon_view(hosts: Vec<HostPane>) -> DaemonView {
    DaemonView {
        name: "rimzd".to_owned(),
        content: vec![stats_host()],
        hosts,
        loop_panel: host(
            &["/usr/bin/rimz", "loop", "watch", "--hold"],
            "/proj/worktree",
        ),
    }
}

fn layout_column(panes: &[&[&str]], stacked: bool) -> crate::mux::LayoutColumn {
    crate::mux::LayoutColumn {
        panes: panes
            .iter()
            .map(|argv| PaneCmd {
                argv: argv.iter().map(|arg| arg.to_string()).collect(),
            })
            .collect(),
        stacked,
    }
}

fn pane_header_before<'a>(layout: &'a str, needle: &str) -> &'a str {
    let args_at = layout.find(needle).expect("pane args present");
    let pane_at = layout[..args_at].rfind("pane").expect("pane header");
    &layout[pane_at..args_at]
}

fn assert_sidebar_sizes_are_percent(layout: &str) {
    let sidebar_headers = layout
        .lines()
        .filter(|line| line.contains(r#"name="rimz-sidebar""#))
        .collect::<Vec<_>>();
    assert!(
        !sidebar_headers.is_empty(),
        "layout carries a sidebar:\n{layout}"
    );
    for header in sidebar_headers {
        assert!(
            header.contains(r#"pane size=""#) && header.contains(r#"%" name="rimz-sidebar""#),
            "fixed-size Zellij sidebars are resize-pinned; use percentage spelling:\n{layout}",
        );
    }
}

fn assert_work_area_template(layout: &str, visible_compact_bars: usize, focused: usize) {
    assert!(layout.contains("compact-bar"), "{layout}");
    assert!(
        !layout.contains("swap_tiled_layout"),
        "RimZ must leave Zellij's native focused-pane split path unobstructed:\n{layout}",
    );
    assert_eq!(layout.matches("focus=true").count(), focused, "{layout}");
    assert_eq!(
        layout
            .matches(r#"plugin location="zellij:compact-bar""#)
            .count(),
        visible_compact_bars,
        "every visible tab/template carries compact-bar:\n{layout}",
    );
}

fn assert_undocked_work_area_template(layout: &str, visible_compact_bars: usize, focused: usize) {
    assert!(layout.contains("compact-bar"), "{layout}");
    assert!(
        !layout.contains("swap_tiled_layout"),
        "undocked layouts must not reintroduce a swap layout:\n{layout}",
    );
    assert_eq!(layout.matches("focus=true").count(), focused, "{layout}");
    assert_eq!(
        layout
            .matches(r#"plugin location="zellij:compact-bar""#)
            .count(),
        visible_compact_bars,
        "every visible tab/template carries compact-bar:\n{layout}",
    );
}

fn resume_tab(label: &str, panes: &[&[&str]], cwd: &str) -> ResumeTab {
    ResumeTab::flat(
        label.to_owned(),
        PathBuf::from(cwd),
        panes
            .iter()
            .map(|argv| argv.iter().map(|arg| arg.to_string()).collect())
            .collect(),
    )
}

fn resume_tab_with_columns(label: &str, columns: &[&[&[&str]]], cwd: &str) -> ResumeTab {
    ResumeTab {
        label: label.to_owned(),
        cwd: PathBuf::from(cwd),
        layout: crate::mux::LayoutPanes {
            columns: columns
                .iter()
                .map(|column| crate::mux::LayoutColumn {
                    panes: column
                        .iter()
                        .map(|argv| crate::mux::PaneCmd {
                            argv: argv.iter().map(|arg| arg.to_string()).collect(),
                        })
                        .collect(),
                    stacked: false,
                })
                .collect(),
        },
    }
}

#[test]
fn session_layout_renders_terminal_template_bar_and_runtime_args() {
    let layout =
        render_session_layout(&sidebar_opts("rimz-contract", Some(50), None), None, &[]).unwrap();
    assert_work_area_template(&layout, 2, 3);
    assert!(layout.contains("pane focus=true"), "{layout}");
    assert!(layout.contains("tab focus=true"), "{layout}");
    assert!(!layout.contains("default_tab_template"), "{layout}");
    assert!(layout.contains("start_suspended false"), "{layout}");
    assert!(!layout.contains("start_suspended true"), "{layout}");
    assert!(layout.contains(r#""--refresh-ms" "50""#), "{layout}");
    assert!(layout.contains("session_serialization false"), "{layout}");
    assert!(layout.contains("disable_session_metadata true"), "{layout}");
}

#[test]
fn session_layout_uses_configured_birth_fixed_options() {
    let mut opts = sidebar_opts("rimz-options", None, None);
    opts.config.zellij.session_serialization = true;
    opts.config.zellij.disable_session_metadata = false;

    let layout = render_session_layout(&opts, None, &[]).expect("render layout");

    assert!(layout.contains("session_serialization true"), "{layout}");
    assert!(
        layout.contains("disable_session_metadata false"),
        "{layout}"
    );
}

#[test]
fn session_layout_seeds_template_and_birth_from_probed_width() {
    let opts = sidebar_opts("rimz-width", None, Some(120));
    let layout = render_session_layout(&opts, None, &[]).expect("render layout");
    assert_eq!(
        layout
            .matches(r#"pane size="25%" name="rimz-sidebar" borderless=true"#)
            .count(),
        2,
        "the template and explicit birth tab share the probed seed:\n{layout}",
    );
    assert_eq!(
        layout
            .matches(r#"pane size="30%" name="rimz-sidebar" borderless=true"#)
            .count(),
        0,
        "a probed narrow launch does not use the wide fallback:\n{layout}",
    );
    let capped = sidebar_opts("rimz-width", None, Some(340));
    let layout = render_session_layout(&capped, None, &[]).expect("render layout");
    assert_eq!(
        layout
            .matches(r#"pane size="21%" name="rimz-sidebar" borderless=true"#)
            .count(),
        2,
        "the template and explicit birth tab share the cap-aware seed:\n{layout}",
    );
    let new_tab_template = layout
        .split("new_tab_template")
        .nth(1)
        .and_then(|section| section.split("\n    tab").next())
        .expect("layout carries a new_tab_template");
    assert!(
        new_tab_template.contains(r#"size="21%""#),
        "the new_tab_template carries the cap-aware launch seed:\n{layout}",
    );
    let birth_tab = layout
        .split("    tab focus=true")
        .nth(1)
        .expect("layout carries an explicit birth tab");
    assert!(
        !birth_tab.contains("pane size=72 name=\"rimz-sidebar\""),
        "the explicit birth tab carries no fixed sidebar size:\n{layout}",
    );
    assert_sidebar_sizes_are_percent(&layout);

    let unprobed = sidebar_opts("rimz-width", None, None);
    let layout = render_session_layout(&unprobed, None, &[]).expect("render layout");
    let new_tab_template = layout
        .split("new_tab_template")
        .nth(1)
        .and_then(|section| section.split("\n    tab").next())
        .expect("layout carries a new_tab_template");
    assert!(
        new_tab_template.contains(r#"size="25%""#),
        "an unprobed launch uses the unknown-geometry narrow fallback:\n{layout}",
    );
}

#[test]
fn background_view_layout_renders_content_and_stacked_daemons() {
    let layout = render_background_view_layout(&background_view_opts(vec![
        host(
            &["claude", "remote-control", "--spawn", "worktree"],
            "/proj/root",
        ),
        host(
            &["/usr/bin/rimz", "codex", "app-server", "serve"],
            "/proj/worktree",
        ),
    ]))
    .expect("render background view layout");
    assert_work_area_template(&layout, 1, 1);
    assert!(layout.contains(r#"args "stats" "--refresh""#), "{layout}");
    assert!(
        !pane_header_before(&layout, r#"args "stats" "--refresh""#).contains("size="),
        "content stays sizeless so it absorbs the middle column:\n{layout}",
    );
    assert!(layout.contains(r#"command "claude""#), "{layout}");
    assert!(
        layout.contains(r#"args "remote-control" "--spawn" "worktree""#),
        "{layout}",
    );
    assert!(
        layout.contains(r#"name="claude remote-control --spawn worktree""#),
        "managed pane carries stable title identity:\n{layout}",
    );
    assert!(layout.contains(r#"command "/usr/bin/rimz""#), "{layout}");
    assert!(
        layout.contains(r#"args "codex" "app-server" "serve""#),
        "{layout}",
    );
    assert!(
        layout.contains(r#"name="/usr/bin/rimz stats --refresh""#)
            && layout.contains(r#"name="/usr/bin/rimz loop watch --hold""#),
        "content and loop panes carry stable title identity:\n{layout}",
    );
    assert!(
        layout.contains(r#"pane size="25%" split_direction="horizontal""#),
        "daemon hosts stack in a right column at the sidebar birth width:\n{layout}",
    );
    assert!(layout.contains("pane focus=true"), "{layout}");
    assert!(layout.contains("start_suspended false"), "{layout}");
    assert!(layout.contains("close_on_exit true"), "{layout}");
    assert!(
        layout.contains(r#"name="rimz-sidebar" borderless=true"#),
        "{layout}"
    );
    assert_sidebar_sizes_are_percent(&layout);
    assert!(layout.contains(r#""sidebar" "serve""#), "{layout}");
    assert!(layout.contains(r#"cwd="/proj/worktree""#), "{layout}");
    assert!(layout.contains(r#"cwd="/proj/root""#), "{layout}");

    let layout = render_background_view_layout(&background_view_opts(vec![]))
        .expect("render content-only background view layout");
    assert_work_area_template(&layout, 1, 1);
    assert!(layout.contains(r#"args "stats" "--refresh""#), "{layout}");
    assert!(
        !layout.contains(r#"split_direction="horizontal""#),
        "no stacked content or daemon column with one content pane and no hosts:\n{layout}",
    );
    assert_eq!(
        layout.matches("focus=true").count(),
        1,
        "first content pane takes focus in a daemon-less view:\n{layout}",
    );

    let mut opts = background_view_opts(vec![]);
    opts.view.content.push(host(&["btop"], "/proj/worktree"));
    let layout = render_background_view_layout(&opts).expect("render multi-content layout");
    assert_work_area_template(&layout, 1, 1);
    assert!(
        layout.contains(r#"pane split_direction="horizontal""#),
        "multiple content panes stack in the middle column:\n{layout}",
    );
    assert!(layout.contains(r#"command "btop""#), "{layout}");
}

#[test]
fn tab_layout_derives_percent_from_an_explicit_live_width() {
    let sidebar = background_view_opts(vec![]).sidebar;
    let opts = TabOptions {
        session_name: sidebar.session_name.clone(),
        title: "review".to_owned(),
        cwd: PathBuf::from("/proj/worktree"),
        panes: crate::mux::LayoutPanes {
            columns: vec![
                layout_column(&[&["/bin/sh"]], false),
                layout_column(&[&["codex"], &["/bin/sh", "-l"]], false),
            ],
        },
        focus: true,
        dock_sidebar: true,
        sidebar,
    };
    let layout = render_tab_layout(&opts, 21).expect("render tab layout");
    assert_work_area_template(&layout, 1, 1);
    assert!(
        layout.contains(r#"pane size="21%" name="rimz-sidebar" borderless=true"#),
        "custom tab layouts spell the live column target as a percentage:\n{layout}",
    );
    assert_sidebar_sizes_are_percent(&layout);
    assert!(
        layout.contains(r#"pane split_direction="horizontal""#),
        "{layout}"
    );
    assert!(layout.contains(r#"command "codex""#), "{layout}");
    assert_eq!(layout.matches("focus=true").count(), 1, "{layout}");

    let layout = render_tab_layout(&opts, 25).expect("render tab layout");
    assert!(
        layout.contains(r#"pane size="25%" name="rimz-sidebar" borderless=true"#),
        "custom tab layouts accept the backend's derived live percentage:\n{layout}",
    );
    assert_sidebar_sizes_are_percent(&layout);
}

#[test]
fn tab_layout_renders_tiled_and_stacked_columns() {
    let sidebar = background_view_opts(vec![]).sidebar;
    let opts = TabOptions {
        session_name: sidebar.session_name.clone(),
        title: "review".to_owned(),
        cwd: PathBuf::from("/proj/worktree"),
        panes: crate::mux::LayoutPanes {
            columns: vec![
                layout_column(&[&["planner"], &["logs"]], false),
                layout_column(&[&["coder"], &["reviewer"]], true),
            ],
        },
        focus: true,
        dock_sidebar: true,
        sidebar,
    };

    let layout = render_tab_layout(&opts, 30).expect("render tab layout");

    assert_work_area_template(&layout, 1, 1);
    assert!(
        layout.contains(r#"pane split_direction="horizontal""#),
        "tiled column should use horizontal split:\n{layout}",
    );
    assert!(
        layout.contains("pane stacked=true"),
        "stacked column should use native Zellij stack:\n{layout}",
    );
    assert!(layout.contains(r#"command "coder""#), "{layout}");
    assert!(layout.contains(r#"command "reviewer""#), "{layout}");
    assert_eq!(layout.matches("focus=true").count(), 1, "{layout}");
}

#[test]
fn undocked_tab_layout_renders_stacked_columns() {
    let sidebar = background_view_opts(vec![]).sidebar;
    let opts = TabOptions {
        session_name: sidebar.session_name.clone(),
        title: "review".to_owned(),
        cwd: PathBuf::from("/proj/worktree"),
        panes: crate::mux::LayoutPanes {
            columns: vec![layout_column(&[&["coder"], &["reviewer"]], true)],
        },
        focus: true,
        dock_sidebar: false,
        sidebar,
    };

    let layout = render_tab_layout(&opts, 30).expect("render tab layout");

    assert!(!layout.contains("rimz-sidebar"), "{layout}");
    assert!(
        layout.contains("pane stacked=true"),
        "undocked layout should preserve native stack:\n{layout}",
    );
    assert_eq!(layout.matches("focus=true").count(), 1, "{layout}");
    assert_undocked_work_area_template(&layout, 1, 1);
}

#[test]
fn tab_layout_can_omit_sidebar_for_gallery_columns() {
    let sidebar = background_view_opts(vec![]).sidebar;
    let opts = TabOptions {
        session_name: sidebar.session_name.clone(),
        title: "sidebar gallery".to_owned(),
        cwd: PathBuf::from("/proj/worktree"),
        panes: crate::mux::LayoutPanes {
            columns: vec![layout_column(&[&["rimz", "sidebar"]], false)],
        },
        focus: true,
        dock_sidebar: false,
        sidebar,
    };
    let layout = render_tab_layout(&opts, 30).expect("render gallery layout");
    assert!(!layout.contains("rimz-sidebar"), "{layout}");
    assert_undocked_work_area_template(&layout, 1, 1);
}

#[test]
fn session_layout_seeds_resumed_agents_and_focuses_working_when_empty() {
    let opts = background_view_opts(vec![]).sidebar;
    let resume = vec![
        resume_tab(
            "#feature",
            &[
                &["claude", "--resume", "sess-1"],
                &["codex", "resume", "sess-2"],
            ],
            "/proj/feature",
        ),
        resume_tab("#main", &[&["pi", "resume", "sess-3"]], "/proj/main"),
    ];
    let layout = render_session_layout(&opts, None, &resume).expect("render resume layout");
    assert_work_area_template(&layout, 4, 5);
    assert!(layout.contains(r#"command "claude""#), "{layout}");
    assert!(layout.contains(r#"args "--resume" "sess-1""#), "{layout}");
    assert!(layout.contains(r#"command "codex""#), "{layout}");
    assert!(layout.contains(r#"args "resume" "sess-2""#), "{layout}");
    assert!(layout.contains(r#"command "pi""#), "{layout}");
    assert!(layout.contains(r#"args "resume" "sess-3""#), "{layout}");
    assert!(layout.contains(r#"cwd="/proj/feature""#), "{layout}");
    assert!(layout.contains(r#"cwd="/proj/main""#), "{layout}");
    assert!(layout.contains("start_suspended false"), "{layout}");
    assert!(
        layout.contains(r##"tab name="#feature" focus=true"##),
        "the freshest resumed channel leads:\n{layout}",
    );
    assert!(
        !layout.contains(r##"tab name="#main" focus=true"##),
        "only the first resumed tab is focused:\n{layout}",
    );
    assert!(
        layout.contains("    tab {"),
        "a bare working terminal tab remains:\n{layout}",
    );
    assert!(layout.contains("new_tab_template"), "{layout}");

    let layout = render_session_layout(&opts, None, &[]).expect("render layout");
    assert_work_area_template(&layout, 2, 3);
    assert!(layout.contains("tab focus=true"), "{layout}");
    assert!(
        !layout.contains("tab name="),
        "no daemon or agent tabs without a daemon or resume set:\n{layout}",
    );
}

#[test]
fn session_layout_renders_resume_columns() {
    let opts = background_view_opts(vec![]).sidebar;
    let resume = vec![resume_tab_with_columns(
        "#team",
        &[
            &[&["planner", "resume"]],
            &[&["coder", "resume"], &["reviewer", "resume"]],
        ],
        "/proj/team",
    )];

    let layout = render_session_layout(&opts, None, &resume).expect("render resume layout");

    assert!(
        layout.contains(r##"tab name="#team" focus=true"##),
        "{layout}"
    );
    assert!(
        layout.contains(r#"pane split_direction="horizontal" {"#),
        "right column rows stay nested instead of flattening into peer columns:\n{layout}",
    );
    assert!(layout.contains(r#"command "planner""#), "{layout}");
    assert!(layout.contains(r#"command "coder""#), "{layout}");
    assert!(layout.contains(r#"command "reviewer""#), "{layout}");
}

#[test]
fn session_layout_leads_daemon_tab_with_three_column_daemon_view() {
    let bg = background_view_opts(vec![
        host(&["claude", "remote-control"], "/proj/root"),
        host(
            &["/usr/bin/rimz", "codex", "app-server", "serve"],
            "/proj/worktree",
        ),
    ]);
    let layout = render_session_layout(&bg.sidebar, Some(&daemon_view(bg.view.hosts.clone())), &[])
        .expect("render session layout with daemon");
    assert_work_area_template(&layout, 3, 4);
    let daemon_at = layout.find(r#"tab name="rimzd""#).expect("daemon tab");
    let work_at = layout.find("tab focus=true").expect("working tab");
    assert!(
        daemon_at < work_at,
        "daemon tab must precede the working tab\n{layout}",
    );
    assert!(layout.contains("new_tab_template"), "{layout}");
    assert!(layout.contains(r#"command "claude""#), "{layout}");
    assert!(
        layout.contains(r#"args "codex" "app-server" "serve""#),
        "{layout}",
    );
    assert!(layout.contains(r#"args "stats" "--refresh""#), "{layout}");
    assert!(
        !pane_header_before(&layout, r#"args "stats" "--refresh""#).contains("size="),
        "content stays sizeless so it absorbs the middle column:\n{layout}",
    );
    assert!(
        layout.contains(r#"pane size="25%" split_direction="horizontal""#),
        "daemon hosts stack in a right column at the sidebar birth width:\n{layout}",
    );
    assert!(
        layout.contains(r#"name="rimz-sidebar" borderless=true"#),
        "{layout}"
    );
    assert!(layout.contains(r#"cwd="/proj/root""#), "{layout}");

    let bg = background_view_opts(vec![]);
    let layout = render_session_layout(&bg.sidebar, Some(&daemon_view(vec![])), &[])
        .expect("render content-only daemon tab");
    assert_work_area_template(&layout, 3, 4);
    assert!(layout.contains(r#"tab name="rimzd""#), "{layout}");
    assert!(layout.contains(r#"args "stats" "--refresh""#), "{layout}");
    assert!(
        !layout.contains(r#"split_direction="horizontal""#),
        "no stacked content or daemon column with one content pane and no hosts:\n{layout}",
    );
}
