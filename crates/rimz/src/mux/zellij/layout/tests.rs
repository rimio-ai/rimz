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
        cwd: PathBuf::from("/proj/worktree"),
        birth_size: width.birth_size(detected_cols),
        rimz_bin: PathBuf::from("/usr/bin/rimz"),
        replace_existing: false,
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
        },
        sidebar: sidebar_opts("rimz-bg", None, None),
    }
}

fn daemon_view(hosts: Vec<HostPane>) -> DaemonView {
    DaemonView {
        name: "rimzd".to_owned(),
        content: vec![stats_host()],
        hosts,
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

fn swap_layout_ranges(layout: &str) -> Vec<std::ops::Range<usize>> {
    let marker = r#"swap_tiled_layout name="rimz-work-area""#;
    let mut ranges = Vec::new();
    let mut search_from = 0;
    while let Some(relative_start) = layout[search_from..].find(marker) {
        let start = search_from + relative_start;
        let brace_start = start + layout[start..].find('{').expect("swap layout starts");
        let mut depth = 0_u16;
        let mut end = None;
        for (offset, ch) in layout[brace_start..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(brace_start + offset + ch.len_utf8());
                        break;
                    }
                }
                _ => {}
            }
        }
        let end = end.expect("swap layout closes");
        ranges.push(start..end);
        search_from = end;
    }
    ranges
}

fn only_swap_layout(layout: &str) -> &str {
    let ranges = swap_layout_ranges(layout);
    assert_eq!(ranges.len(), 1, "layout carries one swap layout:\n{layout}");
    &layout[ranges[0].clone()]
}

fn layout_without_swap_layouts(layout: &str) -> String {
    let mut without = String::new();
    let mut copied_until = 0;
    for range in swap_layout_ranges(layout) {
        without.push_str(&layout[copied_until..range.start]);
        copied_until = range.end;
    }
    without.push_str(&layout[copied_until..]);
    without
}

fn assert_children_only_in_swap_layout(layout: &str) {
    assert!(
        only_swap_layout(layout).contains("children"),
        "the swap layout carries children tiers:\n{layout}",
    );
    assert!(
        !layout_without_swap_layouts(layout).contains("children"),
        "children stays out of visible tabs and templates:\n{layout}",
    );
}

fn swap_tier<'a>(layout: &'a str, marker: &str, next_marker: &str) -> &'a str {
    let swap = only_swap_layout(layout);
    let start = swap
        .find(marker)
        .unwrap_or_else(|| panic!("swap layout carries {marker}:\n{layout}"));
    let after_marker = start + marker.len();
    let end = after_marker
        + swap[after_marker..].find(next_marker).unwrap_or_else(|| {
            panic!("swap layout carries {next_marker} after {marker}:\n{layout}")
        });
    &swap[start..end]
}

fn unbounded_swap_tier(layout: &str) -> &str {
    let swap = only_swap_layout(layout);
    let unbounded_at = swap
        .rfind("\n        tab {\n")
        .expect("swap layout carries a final bare tab tier");
    &swap[unbounded_at..]
}

const SWAP_TIER_COUNT: usize = 13;

fn horizontal_column_count(tier: &str, rows: usize) -> usize {
    let mut needle = String::from("                pane split_direction=\"horizontal\" {\n");
    for _ in 0..rows {
        needle.push_str("                    pane\n");
    }
    needle.push_str("                }");
    tier.matches(&needle).count()
}

fn assert_no_flat_vertical_children_tier(layout: &str) {
    let swap = only_swap_layout(layout);
    assert!(
        !swap.contains("pane split_direction=\"vertical\" {\n                children")
            && !swap.contains("pane split_direction=\"vertical\" {\n                    children"),
        "children must stay in horizontal overflow stacks, not flat vertical tiers:\n{layout}",
    );
}

fn assert_unbounded_swap_tier_after_budget(layout: &str, highest_max_panes: u16) {
    let swap = only_swap_layout(layout);
    let highest_tier = format!("tab max_panes={highest_max_panes}");
    let highest_at = swap.find(&highest_tier).unwrap_or_else(|| {
        panic!("swap layout carries {highest_tier} before the unbounded tier:\n{layout}")
    });
    let unbounded_at = swap
        .rfind("\n        tab {\n")
        .expect("swap layout carries a final bare tab tier");
    let unbounded = &swap[unbounded_at..];
    assert!(
        unbounded_at > highest_at,
        "unbounded swap tier follows the max_panes tiers:\n{layout}",
    );
    assert!(
        unbounded.contains("children"),
        "unbounded swap tier absorbs extra panes:\n{layout}",
    );
    assert!(
        !unbounded.contains("max_panes"),
        "unbounded swap tier has no max_panes cap:\n{layout}",
    );
    assert!(
        !swap.contains(&format!("tab max_panes={}", highest_max_panes + 1)),
        "swap layout should catch larger pane counts with the unbounded tier:\n{layout}",
    );
}

fn assert_docked_balanced_swap_shape(layout: &str) {
    for max_panes in 3..=14 {
        assert!(
            layout.contains(&format!("tab max_panes={max_panes}")),
            "{layout}"
        );
    }
    assert_unbounded_swap_tier_after_budget(layout, 14);

    let four_work = swap_tier(layout, "tab max_panes=6", "tab max_panes=7");
    assert!(
        !four_work.contains("children"),
        "4-work-pane tier is exact and carries no overflow children:\n{layout}",
    );
    assert_eq!(
        horizontal_column_count(four_work, 2),
        2,
        "4-work-pane tier carries two 2-pane columns:\n{layout}",
    );

    let five_work = swap_tier(layout, "tab max_panes=7", "tab max_panes=8");
    assert!(
        !five_work.contains("children"),
        "5-work-pane tier is exact and carries no overflow children:\n{layout}",
    );
    assert_eq!(
        horizontal_column_count(five_work, 2),
        1,
        "5-work-pane tier carries one 2-pane column:\n{layout}",
    );
    assert_eq!(
        horizontal_column_count(five_work, 3),
        1,
        "5-work-pane tier carries one 3-pane column:\n{layout}",
    );

    let twelve_work = swap_tier(layout, "tab max_panes=14", "\n        tab {\n");
    assert_eq!(
        horizontal_column_count(twelve_work, 4),
        3,
        "12-work-pane tier carries three 4-pane columns:\n{layout}",
    );

    let unbounded = unbounded_swap_tier(layout);
    assert!(
        unbounded.contains("children"),
        "unbounded tier keeps the overflow children stack:\n{layout}",
    );
    assert_eq!(
        horizontal_column_count(unbounded, 4),
        2,
        "unbounded tier carries two explicit 4-pane columns:\n{layout}",
    );
    assert_no_flat_vertical_children_tier(layout);
}

fn assert_undocked_balanced_swap_shape(layout: &str) {
    for max_panes in 2..=13 {
        assert!(
            layout.contains(&format!("tab max_panes={max_panes}")),
            "{layout}"
        );
    }
    assert_unbounded_swap_tier_after_budget(layout, 13);

    let four_work = swap_tier(layout, "tab max_panes=5", "tab max_panes=6");
    assert!(
        !four_work.contains("children"),
        "4-work-pane undocked tier is exact and carries no overflow children:\n{layout}",
    );
    assert_eq!(
        horizontal_column_count(four_work, 2),
        2,
        "4-work-pane undocked tier carries two 2-pane columns:\n{layout}",
    );

    let five_work = swap_tier(layout, "tab max_panes=6", "tab max_panes=7");
    assert!(
        !five_work.contains("children"),
        "5-work-pane undocked tier is exact and carries no overflow children:\n{layout}",
    );
    assert_eq!(
        horizontal_column_count(five_work, 2),
        1,
        "5-work-pane undocked tier carries one 2-pane column:\n{layout}",
    );
    assert_eq!(
        horizontal_column_count(five_work, 3),
        1,
        "5-work-pane undocked tier carries one 3-pane column:\n{layout}",
    );

    let unbounded = unbounded_swap_tier(layout);
    assert_eq!(
        horizontal_column_count(unbounded, 4),
        2,
        "unbounded undocked tier carries two explicit 4-pane columns:\n{layout}",
    );
    assert_no_flat_vertical_children_tier(layout);
}

fn assert_work_area_template(layout: &str, visible_compact_bars: usize, focused: usize) {
    assert!(layout.contains("compact-bar"), "{layout}");
    assert!(
        layout.contains(r#"swap_tiled_layout name="rimz-work-area""#),
        "{layout}",
    );
    assert_docked_balanced_swap_shape(layout);
    assert_children_only_in_swap_layout(layout);
    assert_eq!(layout.matches("focus=true").count(), focused, "{layout}");
    assert_eq!(
        layout
            .matches(r#"plugin location="zellij:compact-bar""#)
            .count(),
        visible_compact_bars + SWAP_TIER_COUNT,
        "every visible tab/template and swap template carries compact-bar:\n{layout}",
    );
}

fn assert_undocked_work_area_template(layout: &str, visible_compact_bars: usize, focused: usize) {
    assert!(layout.contains("compact-bar"), "{layout}");
    assert!(
        layout.contains(r#"swap_tiled_layout name="rimz-work-area""#),
        "{layout}",
    );
    assert_undocked_balanced_swap_shape(layout);
    assert_children_only_in_swap_layout(layout);
    assert_eq!(layout.matches("focus=true").count(), focused, "{layout}");
    assert_eq!(
        layout
            .matches(r#"plugin location="zellij:compact-bar""#)
            .count(),
        visible_compact_bars + SWAP_TIER_COUNT,
        "every visible tab/template and swap template carries compact-bar:\n{layout}",
    );
}

fn resume_tab(label: &str, panes: &[&[&str]], cwd: &str) -> ResumeTab {
    ResumeTab {
        label: label.to_owned(),
        cwd: PathBuf::from(cwd),
        panes: panes
            .iter()
            .map(|argv| argv.iter().map(|arg| arg.to_string()).collect())
            .collect(),
    }
}

#[test]
fn balanced_work_columns_splits_rows_evenly_with_taller_columns_right() {
    let cases = [
        (1, vec![1]),
        (2, vec![1, 1]),
        (3, vec![1, 2]),
        (4, vec![2, 2]),
        (5, vec![2, 3]),
        (6, vec![3, 3]),
        (7, vec![3, 4]),
        (8, vec![4, 4]),
        (9, vec![3, 3, 3]),
        (10, vec![3, 3, 4]),
        (11, vec![3, 4, 4]),
        (12, vec![4, 4, 4]),
        (13, vec![3, 3, 3, 4]),
    ];
    for (panes, columns) in cases {
        assert_eq!(
            balanced_work_columns(panes),
            columns,
            "{panes} work panes should split into balanced columns",
        );
    }
}

#[test]
fn session_layout_renders_terminal_template_bar_swap_and_runtime_args() {
    let layout =
        render_session_layout(&sidebar_opts("rimz-contract", Some(50), None), None, &[]).unwrap();
    assert_work_area_template(&layout, 2, 3);
    assert!(layout.contains("pane focus=true"), "{layout}");
    assert!(layout.contains("tab focus=true"), "{layout}");
    assert!(!layout.contains("default_tab_template"), "{layout}");
    assert!(layout.contains("start_suspended false"), "{layout}");
    assert!(!layout.contains("start_suspended true"), "{layout}");
    assert!(layout.contains(r#""--refresh-ms" "50""#), "{layout}");
}

#[test]
fn session_layout_pins_fixed_cols_attached_and_percent_detached() {
    let opts = sidebar_opts("rimz-width", None, Some(120));
    let layout = render_session_layout(&opts, None, &[]).expect("render layout");
    assert!(
        layout.contains(r#"pane size="30%" name="rimz-sidebar" borderless=true"#),
        "the explicit birth tab instantiates detached, so the verdict is \
             its percentage share:\n{layout}",
    );
    assert!(
        layout.contains(r#"pane size=36 name="rimz-sidebar" borderless=true"#),
        "the new_tab_template instantiates attached, so it pins the fixed \
             verdict:\n{layout}",
    );
    let capped = sidebar_opts("rimz-width", None, Some(340));
    let layout = render_session_layout(&capped, None, &[]).expect("render layout");
    assert!(
        layout.contains(r#"pane size="21%" name="rimz-sidebar" borderless=true"#),
        "the explicit birth tab instantiates detached, so a capped width \
             is its derived percentage:\n{layout}",
    );
    assert!(
        layout.contains(r#"pane size=72 name="rimz-sidebar" borderless=true"#),
        "the new_tab_template instantiates attached, so a capped width is \
             the fixed `max_cols` cap:\n{layout}",
    );
    let new_tab_template = layout
        .split("new_tab_template")
        .nth(1)
        .and_then(|section| section.split("    swap_tiled_layout").next())
        .expect("layout carries a new_tab_template");
    assert!(
        !new_tab_template.contains('%'),
        "the new_tab_template carries no percentage:\n{layout}",
    );
    let birth_tab = layout
        .split("    tab focus=true")
        .nth(1)
        .expect("layout carries an explicit birth tab");
    assert!(
        !birth_tab.contains("pane size=72 name=\"rimz-sidebar\""),
        "the explicit birth tab carries no fixed sidebar size:\n{layout}",
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
    assert!(layout.contains(r#"command "/usr/bin/rimz""#), "{layout}");
    assert!(
        layout.contains(r#"args "codex" "app-server" "serve""#),
        "{layout}",
    );
    assert!(
        layout.contains(r#"pane size="30%" split_direction="horizontal""#),
        "daemon hosts stack in a right column at the sidebar birth width:\n{layout}",
    );
    assert!(layout.contains("pane focus=true"), "{layout}");
    assert!(layout.contains("start_suspended false"), "{layout}");
    assert!(layout.contains("close_on_exit true"), "{layout}");
    assert!(
        layout.contains(r#"name="rimz-sidebar" borderless=true"#),
        "{layout}"
    );
    assert!(layout.contains(r#""sidebar" "serve""#), "{layout}");
    assert!(layout.contains(r#"cwd="/proj/worktree""#), "{layout}");
    assert!(layout.contains(r#"cwd="/proj/root""#), "{layout}");

    let layout = render_background_view_layout(&background_view_opts(vec![]))
        .expect("render content-only background view layout");
    assert_work_area_template(&layout, 1, 1);
    assert!(layout.contains(r#"args "stats" "--refresh""#), "{layout}");
    assert!(
        !layout_without_swap_layouts(&layout).contains(r#"split_direction="horizontal""#),
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
fn tab_layout_renders_columns_and_can_mirror_template_width() {
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
    let layout = render_tab_layout(&opts, None).expect("render tab layout");
    assert_work_area_template(&layout, 1, 1);
    assert!(
        layout.contains(r#"pane size=72 name="rimz-sidebar" borderless=true"#),
        "custom tab layouts instantiate from a live client, so the \
             sidebar must pin the fixed max-cols verdict instead of \
            re-evaluating a percentage against wide terminals:\n{layout}",
    );
    assert!(
        !layout.contains(r#"size="30%""#),
        "custom tab layouts must not use detached percentage sizing:\n{layout}",
    );
    assert!(layout.contains("pane size=72"), "{layout}");
    assert!(
        layout.contains(r#"pane split_direction="horizontal""#),
        "{layout}"
    );
    assert!(layout.contains(r#"command "codex""#), "{layout}");
    assert_eq!(layout.matches("focus=true").count(), 1, "{layout}");

    let layout = render_tab_layout(&opts, NonZeroU16::new(60)).expect("render tab layout");
    assert!(
        layout.contains(r#"pane size=60 name="rimz-sidebar" borderless=true"#),
        "custom tab layouts must be able to mirror the live \
             new_tab_template instead of this command's pane-width probe:\n{layout}",
    );
    assert!(
        layout.matches("pane size=60").count() == 14,
        "the visible sidebar and thirteen swap tiers must mirror the live width:\n{layout}",
    );
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

    let layout = render_tab_layout(&opts, None).expect("render tab layout");

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

    let layout = render_tab_layout(&opts, None).expect("render tab layout");

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
    let layout = render_tab_layout(&opts, None).expect("render gallery layout");
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
        layout.contains(r#"pane size="30%" split_direction="horizontal""#),
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
        !layout_without_swap_layouts(&layout).contains(r#"split_direction="horizontal""#),
        "no stacked content or daemon column with one content pane and no hosts:\n{layout}",
    );
}
