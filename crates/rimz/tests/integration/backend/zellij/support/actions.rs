use std::collections::BTreeSet;
use std::fmt::Debug;
use std::path::Path;
use std::time::{Duration, Instant};

use rimz::mux::{MuxBackend, SidebarLiveness, SidebarPaneOptions, SidebarRecovery, ZellijBackend};

use crate::common::{CommandTimeoutExt, ZellijNamespace};

use super::panes::{PaneSnapshot, list_panes};
use super::session::{DUMP_LAYOUT_ATTEMPTS, DUMP_LAYOUT_RETRY_DELAY, SPAWN_TIMEOUT};

pub(in crate::backend::zellij) fn poll_until<T: Debug>(
    timeout: Duration,
    mut observe: impl FnMut() -> Result<T, String>,
    mut ready: impl FnMut(&T) -> bool,
    label: &str,
) -> T {
    let deadline = Instant::now() + timeout;
    let mut last_observation = None;
    let mut last_error = String::new();
    loop {
        match observe() {
            Ok(value) if ready(&value) => return value,
            Ok(value) => last_observation = Some(value),
            Err(err) => last_error = err,
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for {label}; last observation: {last_observation:?}; last error: {last_error}"
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub(in crate::backend::zellij) fn open_new_tab(xdg: &Path, session: &str) {
    let before = PaneSnapshot::expect(xdg, session).tab_ids();
    let output = ZellijNamespace::command_at(xdg)
        .args(["--session", session, "action", "new-tab"])
        .bounded_output()
        .unwrap_or_else(|err| panic!("new-tab failed to run for {session}: {err}"));
    assert!(
        output.status.success(),
        "new-tab failed for {session}: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    poll_until(
        SPAWN_TIMEOUT,
        || list_panes(xdg, session).map(|snapshot| snapshot.tab_ids()),
        |after| after.iter().filter(|id| !before.contains(id)).count() == 1,
        &format!("one fresh tab after {before:?} in {session}"),
    );
}

pub(in crate::backend::zellij) fn spawn_sleep_pane(xdg: &Path, session: &str, cwd: &Path) {
    let before: BTreeSet<_> = PaneSnapshot::expect(xdg, session)
        .panes
        .iter()
        .filter(|pane| pane.is_live_terminal() && !pane.is_sidebar())
        .map(|pane| pane.id)
        .collect();
    let output = ZellijNamespace::command_at(xdg)
        .args(["--session", session, "action", "new-pane", "--cwd"])
        .arg(cwd)
        .args(["--", "sleep", "600"])
        .bounded_output()
        .unwrap_or_else(|err| panic!("new-pane failed to run for {session}: {err}"));
    assert!(
        output.status.success(),
        "new-pane failed for {session}: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    poll_until(
        SPAWN_TIMEOUT,
        || {
            list_panes(xdg, session).map(|snapshot| {
                snapshot
                    .panes
                    .iter()
                    .filter(|pane| pane.is_live_terminal() && !pane.is_sidebar())
                    .map(|pane| pane.id)
                    .filter(|id| !before.contains(id))
                    .collect::<Vec<_>>()
            })
        },
        |fresh| fresh.len() == 1,
        &format!("one fresh work pane after {before:?} in {session}"),
    );
}

pub(in crate::backend::zellij) fn new_tab_template_dump(xdg: &Path, session: &str) -> String {
    let mut last_observation = "dump-layout was not checked".to_owned();
    for attempt in 0..DUMP_LAYOUT_ATTEMPTS {
        if attempt > 0 {
            std::thread::sleep(DUMP_LAYOUT_RETRY_DELAY);
        }
        let output = ZellijNamespace::command_at(xdg)
            .args(["--session", session, "action", "dump-layout"])
            .bounded_output()
            .unwrap_or_else(|err| panic!("dump-layout failed to run for {session}: {err}"));
        let dump = String::from_utf8_lossy(&output.stdout);
        if output.status.success() {
            if let Some(start) = dump.find("new_tab_template") {
                return dump[start..].to_owned();
            }
            last_observation = format!("stdout:\n{dump}");
        } else {
            last_observation = format!(
                "status {}; stderr:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr),
            );
        }
    }
    panic!(
        "dump-layout has no new_tab_template after {DUMP_LAYOUT_ATTEMPTS} attempts in {session}; last observation: {last_observation}"
    );
}

pub(in crate::backend::zellij) fn reconcile_until_converged(
    xdg: &Path,
    opts: &SidebarPaneOptions,
    live: &SidebarLiveness,
) -> SidebarRecovery {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let report = ZellijBackend::with_runtime_dir(xdg)
            .reconcile_sidebars(opts, live)
            .expect("reconcile_sidebars");
        let deferral_only = report.deferred > 0
            && SidebarRecovery {
                deferred: 0,
                ..report
            } == SidebarRecovery::default();
        let transient = report == SidebarRecovery::default() || deferral_only;
        if !transient || Instant::now() >= deadline {
            return report;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
