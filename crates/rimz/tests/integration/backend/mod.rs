//! Live backend suites. The mux suites self-skip when their binary is absent,
//! and the browser suite self-skips without ttyd, Chromium, and its selected
//! mux; the shared `CommandSpec` engine suite needs only coreutils.

mod command;
mod single_backend_room;
mod tmux;
mod web;
mod zellij;

/// A resume tab whose panes, one column per entry of `rows`, each write their
/// pane identity (`RIMZ_WORKTREE_PATH`, then `RIMZ_CHANNEL`) to a marker file
/// in `cwd`, under the pane pin a reborn tab carries.
fn identity_marker_tab(
    cwd: &std::path::Path,
    channel: &str,
    rows: &[usize],
) -> rimz::mux::ResumeTab {
    let project_root = cwd.parent().expect("marker cwd has a parent");
    let mut env = rimz::workspace::pin_env(
        &rimz::ids::WorkspaceId::from_project_root(project_root),
        project_root,
    );
    env.insert("RIMZ".to_owned(), "1".to_owned());
    env.insert(
        rimz::workspace::ENV_WORKTREE_PATH.to_owned(),
        cwd.display().to_string(),
    );
    env.insert(rimz::workspace::ENV_CHANNEL.to_owned(), channel.to_owned());
    let mut marker = 0;
    let columns = rows
        .iter()
        .map(|&count| rimz::mux::LayoutColumn {
            panes: (0..count)
                .map(|_| {
                    marker += 1;
                    rimz::mux::PaneCmd {
                        argv: vec![
                            "sh".to_owned(),
                            "-c".to_owned(),
                            r#"printf "%s\n%s" "$RIMZ_WORKTREE_PATH" "$RIMZ_CHANNEL" > "$1"; exec sleep 60"#
                                .to_owned(),
                            "marker".to_owned(),
                            format!("marker-{marker}"),
                        ],
                        name: Some(format!("marker-{marker}")),
                    }
                })
                .collect(),
            stacked: false,
        })
        .collect();
    rimz::mux::ResumeTab {
        label: format!("#{channel}"),
        cwd: cwd.to_owned(),
        env,
        layout: rimz::mux::LayoutPanes {
            columns,
            focused_pane: 0,
        },
    }
}

/// Wait for every marker an [`identity_marker_tab`] pane writes, and assert
/// each carries the tab's cwd and channel.
fn assert_identity_markers(tab: &rimz::mux::ResumeTab, channel: &str) {
    let expected = format!("{}\n{channel}", tab.cwd.display());
    for marker in 1..=tab.pane_count() {
        let path = tab.cwd.join(format!("marker-{marker}"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !std::fs::read_to_string(&path).is_ok_and(|text| !text.is_empty()) {
            assert!(
                std::time::Instant::now() < deadline,
                "missing {}",
                path.display()
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            expected,
            "{}",
            path.display()
        );
    }
}
