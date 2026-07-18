use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::common::CommandTimeoutExt;
use rimz::mux::{MuxBackend, WidthSyncOptions, ZellijBackend};

use super::actions::{poll_until, spawn_sleep_pane};
use super::panes::{PaneGeometry, PaneSnapshot, list_panes};
use super::session::{SPAWN_TIMEOUT, scoped_zellij};

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

pub(in crate::backend::zellij) fn named_compact_bar_pane_geometry(
    xdg: &Path,
    session: &str,
    tab_name: &str,
) -> Result<Option<PaneGeometry>, String> {
    Ok(list_panes(xdg, session)?
        .panes
        .iter()
        .find(|pane| {
            pane.is_plugin
                && pane.tab_name.as_deref() == Some(tab_name)
                && pane
                    .title
                    .as_deref()
                    .is_some_and(|title| title.contains("compact-bar"))
        })
        .map(|pane| pane.geometry()))
}

pub(in crate::backend::zellij) fn wait_for_named_sidebar_pane(
    xdg: &Path,
    session: &str,
    tab_name: &str,
) -> Option<PaneGeometry> {
    poll_until(
        Duration::from_secs(10),
        || named_sidebar_pane_geometry(xdg, session, tab_name),
        Option::is_some,
        &format!("sidebar pane in {session}/{tab_name}"),
    )
}

pub(in crate::backend::zellij) fn wait_for_named_compact_bar_pane(
    xdg: &Path,
    session: &str,
    tab_name: &str,
) -> Option<PaneGeometry> {
    poll_until(
        Duration::from_secs(10),
        || named_compact_bar_pane_geometry(xdg, session, tab_name),
        Option::is_some,
        &format!("compact bar in {session}/{tab_name}"),
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
        Duration::from_secs(10),
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

pub(in crate::backend::zellij) fn wait_for_new_tab_name(
    xdg: &Path,
    session: &str,
    before: &[u64],
) -> String {
    let before: BTreeSet<_> = before.iter().copied().collect();
    poll_until(
        Duration::from_secs(10),
        || {
            let snapshot = list_panes(xdg, session)?;
            Ok(snapshot
                .panes
                .iter()
                .find(|pane| !pane.is_plugin && !before.contains(&pane.tab_id))
                .and_then(|pane| pane.tab_name.clone()))
        },
        Option::is_some,
        &format!("new named tab after {before:?}"),
    )
    .expect("poll required a tab name")
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

pub(in crate::backend::zellij) fn assert_work_panes_reopen_in_survivor_after_closing_first(
    backend: &ZellijBackend,
    width_sync: &WidthSyncOptions,
    xdg: &Path,
    session: &str,
    tab_name: &str,
    cwd: &Path,
    client_size: (u16, u16),
) {
    let (client_columns, client_rows) = client_size;
    let work = wait_for_named_work_pane_count(xdg, session, tab_name, 2);
    let closed = scoped_zellij(xdg)
        .args([
            "--session",
            session,
            "action",
            "close-pane",
            "--pane-id",
            &format!("terminal_{}", work[0].id),
        ])
        .bounded_output()
        .expect("close-pane");
    assert!(
        closed.status.success(),
        "close-pane failed: {}",
        String::from_utf8_lossy(&closed.stderr)
    );

    let sidebar = wait_for_named_sidebar_pane(xdg, session, tab_name).expect("work tab sidebar");
    assert_eq!(sidebar.x, 0, "sidebar should stay docked left: {sidebar:?}");
    let expected_columns = u64::from(client_columns).saturating_sub(sidebar.columns);
    let survivor = wait_for_named_work_pane_state(xdg, session, tab_name, 1, |work| {
        work[0].columns.abs_diff(expected_columns) <= 5
    });
    let focused = scoped_zellij(xdg)
        .args([
            "--session",
            session,
            "action",
            "focus-pane-id",
            &format!("terminal_{}", survivor[0].id),
        ])
        .bounded_output()
        .expect("focus-pane-id");
    assert!(
        focused.status.success(),
        "focus-pane-id failed: {}",
        String::from_utf8_lossy(&focused.stderr)
    );

    let bounds = survivor[0];
    spawn_sleep_pane(xdg, session, cwd);
    let inside = |pane: &PaneGeometry| {
        pane.x + 2 >= bounds.x
            && pane.y + 2 >= bounds.y
            && pane.x + pane.columns <= bounds.x + bounds.columns + 2
            && pane.y + pane.rows <= bounds.y + bounds.rows + 2
    };
    let split =
        wait_for_named_work_pane_state(xdg, session, tab_name, 2, |work| work.iter().all(inside));
    assert!(
        split.iter().all(inside),
        "work panes escaped survivor bounds {bounds:?}: {split:?}"
    );
    let target = width_sync
        .width_override
        .unwrap_or(width_sync.width.max_cols)
        .get();
    let deadline = Instant::now() + Duration::from_secs(10);
    let sidebar = loop {
        let sidebar =
            wait_for_named_sidebar_pane(xdg, session, tab_name).expect("work tab sidebar");
        let step = (u64::from(client_columns) / 20).max(1);
        if sidebar.columns.abs_diff(u64::from(target)) <= (step / 2).max(1) {
            break sidebar;
        }
        assert!(
            Instant::now() < deadline,
            "sidebar did not converge after native split/close",
        );
        let pane = rimz::ids::PaneId::from_parts(
            rimz::MuxName::Zellij,
            format!("terminal_{}", sidebar.id),
        );
        backend
            .nudge_sidebar_width(
                session,
                &pane,
                u16::try_from(sidebar.columns).expect("sidebar width fits u16"),
                target,
            )
            .expect("nudge sidebar after native split/close");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(sidebar.x, 0);
    let target = u64::from(target);
    let step = (u64::from(client_columns) / 20).max(1);
    let tolerance = (step / 2).max(1);
    let lower = target.saturating_sub(tolerance);
    assert!(
        (lower..=target + tolerance).contains(&sidebar.columns),
        "sidebar width left the live convergence band: {sidebar:?}"
    );
    let bar = wait_for_named_compact_bar_pane(xdg, session, tab_name).expect("compact bar");
    assert_eq!(
        (bar.x, bar.columns, bar.rows, bar.y + bar.rows),
        (0, u64::from(client_columns), 1, u64::from(client_rows))
    );
}
