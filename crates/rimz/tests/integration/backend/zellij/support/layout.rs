use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use super::actions::poll_until;
use super::panes::{PaneGeometry, PaneSnapshot, list_panes};
use super::session::SPAWN_TIMEOUT;

pub(in crate::backend::zellij) fn serve_processes_for(session: &str) -> Result<usize, String> {
    let entries = std::fs::read_dir("/proc").map_err(|err| format!("read /proc: {err}"))?;
    Ok(entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
            std::fs::read(format!("/proc/{pid}/cmdline")).ok()
        })
        .filter(|cmdline| {
            let cmdline = String::from_utf8_lossy(cmdline).replace('\0', " ");
            cmdline.contains(session) && cmdline.contains("sidebar") && cmdline.contains("serve")
        })
        .count())
}

pub(in crate::backend::zellij) fn assert_session_has_bottom_bar(xdg: &Path, session: &str) {
    let snapshot = PaneSnapshot::expect(xdg, session);
    assert!(
        snapshot.panes.iter().any(|pane| {
            pane.is_plugin
                && pane
                    .title
                    .as_deref()
                    .is_some_and(|title| title.contains("compact-bar"))
        }),
        "session {session} should carry a bottom bar plugin: {snapshot:?}",
    );
}

pub(in crate::backend::zellij) fn named_work_pane_geometry(
    xdg: &Path,
    session: &str,
    tab_name: &str,
) -> Result<Vec<PaneGeometry>, String> {
    let mut work: Vec<_> = list_panes(xdg, session)?
        .panes
        .iter()
        .filter(|pane| {
            !pane.is_plugin && pane.tab_name.as_deref() == Some(tab_name) && !pane.is_sidebar()
        })
        .map(|pane| pane.geometry())
        .collect();
    work.sort_by_key(|pane| pane.x);
    Ok(work)
}

pub(in crate::backend::zellij) fn named_sidebar_pane_geometry(
    xdg: &Path,
    session: &str,
    tab_name: &str,
) -> Result<Option<PaneGeometry>, String> {
    Ok(list_panes(xdg, session)?
        .panes
        .iter()
        .find(|pane| pane.tab_name.as_deref() == Some(tab_name) && pane.is_sidebar())
        .map(|pane| pane.geometry()))
}

pub(in crate::backend::zellij) fn wait_for_named_sidebar_pane(
    xdg: &Path,
    session: &str,
    tab_name: &str,
) -> Option<PaneGeometry> {
    poll_until(
        Duration::from_secs(30),
        || named_sidebar_pane_geometry(xdg, session, tab_name),
        Option::is_some,
        &format!("sidebar pane in {session}/{tab_name}"),
    )
}

pub(in crate::backend::zellij) fn wait_for_named_work_pane_state(
    xdg: &Path,
    session: &str,
    tab_name: &str,
    want: usize,
    ready: impl FnMut(&Vec<PaneGeometry>) -> bool,
) -> Vec<PaneGeometry> {
    let mut ready = ready;
    poll_until(
        Duration::from_secs(30),
        || named_work_pane_geometry(xdg, session, tab_name),
        |work| work.len() == want && ready(work),
        &format!("{want} work panes in {session}/{tab_name}"),
    )
}

pub(in crate::backend::zellij) fn wait_for_named_work_pane_count(
    xdg: &Path,
    session: &str,
    tab_name: &str,
    want: usize,
) -> Vec<PaneGeometry> {
    wait_for_named_work_pane_state(xdg, session, tab_name, want, |_| true)
}

pub(in crate::backend::zellij) fn work_pane_geometry(
    xdg: &Path,
    session: &str,
) -> Vec<PaneGeometry> {
    let mut work: Vec<_> = PaneSnapshot::expect(xdg, session)
        .panes
        .iter()
        .filter(|pane| pane.is_live_terminal() && !pane.is_sidebar())
        .map(|pane| pane.geometry())
        .collect();
    work.sort_by_key(|pane| pane.id);
    work
}

pub(in crate::backend::zellij) fn assert_sidebars_not_held(
    xdg: &Path,
    session: &str,
    context: &str,
) {
    let snapshot = PaneSnapshot::expect(xdg, session);
    let sidebars: Vec<_> = snapshot
        .panes
        .iter()
        .filter(|pane| pane.is_sidebar())
        .collect();
    assert!(
        !sidebars.is_empty(),
        "rimz-sidebar pane missing while checking {context}: {snapshot:?}",
    );
    assert!(
        sidebars.iter().all(|pane| !pane.is_held),
        "sidebar command pane is waiting for Enter instead of running in {context}: {sidebars:?}",
    );
}

pub(in crate::backend::zellij) fn nonplugin_titles_in_tab(
    xdg: &Path,
    session: &str,
    tab: u64,
) -> Vec<String> {
    PaneSnapshot::expect(xdg, session).terminal_titles_in_tab(tab)
}

pub(in crate::backend::zellij) fn wait_for_tab_count(
    xdg: &Path,
    session: &str,
    want: usize,
) -> Vec<u64> {
    poll_until(
        Duration::from_secs(10),
        || list_panes(xdg, session).map(|snapshot| snapshot.tab_ids()),
        |ids| ids.len() >= want,
        &format!("{want} tabs in {session}"),
    )
}

fn assert_sidebar_is_left_docked_inner(xdg: &Path, session: &str) -> (u64, u64) {
    let snapshot = PaneSnapshot::expect(xdg, session);
    let sidebar = snapshot.sidebar().expect("rimz-sidebar pane");
    let geometry = sidebar.geometry();
    let terminals: Vec<_> = snapshot
        .panes
        .iter()
        .filter(|pane| !pane.is_plugin && pane.tab_id == sidebar.tab_id)
        .collect();
    let total_columns = terminals
        .iter()
        .map(|pane| pane.pane_x + pane.pane_columns)
        .max()
        .expect("tab width");
    assert_eq!(geometry.x, 0, "sidebar should be the left pane");
    for pane in terminals.iter().filter(|pane| pane.id != sidebar.id) {
        assert!(
            pane.pane_x >= geometry.columns,
            "work pane intrudes into sidebar band: sidebar={sidebar:?}, pane={pane:?}",
        );
    }
    (geometry.columns, total_columns)
}

pub(in crate::backend::zellij) fn assert_sidebar_is_left_thirty_percent(xdg: &Path, session: &str) {
    let (columns, total_columns) = assert_sidebar_is_left_docked_inner(xdg, session);
    assert!(
        columns * 100 <= total_columns * 35,
        "sidebar should occupy roughly 30% of tab: {columns}/{total_columns}",
    );
}

pub(in crate::backend::zellij) fn assert_sidebar_is_left_docked(xdg: &Path, session: &str) {
    assert_sidebar_is_left_docked_inner(xdg, session);
}

pub(in crate::backend::zellij) fn sidebar_columns_by_tab(
    xdg: &Path,
    session: &str,
) -> BTreeMap<u64, u64> {
    list_panes(xdg, session)
        .map(|snapshot| {
            snapshot
                .panes
                .iter()
                .filter(|pane| pane.is_sidebar())
                .map(|pane| (pane.tab_id, pane.pane_columns))
                .collect()
        })
        .unwrap_or_default()
}

pub(in crate::backend::zellij) fn wait_for_sidebar_columns(
    xdg: &Path,
    session: &str,
    expected: &[std::ops::RangeInclusive<u64>],
) -> bool {
    let deadline = Instant::now() + SPAWN_TIMEOUT;
    loop {
        let widths = sidebar_columns_by_tab(xdg, session);
        if widths.len() == expected.len()
            && widths
                .values()
                .zip(expected)
                .all(|(width, range)| range.contains(width))
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}
