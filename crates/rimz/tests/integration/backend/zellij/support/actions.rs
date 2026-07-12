use std::fmt::Debug;
use std::path::Path;
use std::time::{Duration, Instant};

use rimz::mux::{MuxBackend, SidebarLiveness, SidebarPaneOptions, SidebarRecovery, ZellijBackend};

use crate::common::CommandTimeoutExt;

use super::panes::{PaneSnapshot, list_panes};
use super::session::{
    ACTION_ATTEMPTS, ACTION_CONFIRM_STEP, ACTION_CONFIRM_WINDOW, DUMP_LAYOUT_ATTEMPTS,
    DUMP_LAYOUT_RETRY_DELAY, scoped_zellij,
};

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

pub(in crate::backend::zellij) fn action_until(
    xdg: &Path,
    session: &str,
    args: &[String],
    label: &str,
    mut confirm: impl FnMut() -> Result<(), String>,
) {
    let mut last_observation = "post-condition was not checked".to_owned();
    for attempt in 0..ACTION_ATTEMPTS {
        if attempt > 0 && confirm().is_ok() {
            return;
        }
        let output = scoped_zellij(xdg)
            .args(["--session", session])
            .args(args.iter().map(String::as_str))
            .bounded_output()
            .unwrap_or_else(|err| panic!("{label} failed to run for {session}: {err}"));
        assert!(
            output.status.success(),
            "{label} failed for {session}: {}",
            String::from_utf8_lossy(&output.stderr),
        );
        let deadline = Instant::now() + ACTION_CONFIRM_WINDOW;
        loop {
            match confirm() {
                Ok(()) => return,
                Err(observation) => last_observation = observation,
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(ACTION_CONFIRM_STEP);
        }
    }
    panic!(
        "{label} did not materialize after {ACTION_ATTEMPTS} attempts in {session}; last observation: {last_observation}"
    );
}

pub(in crate::backend::zellij) fn open_new_tab(xdg: &Path, session: &str) {
    let before = PaneSnapshot::expect(xdg, session).tab_ids();
    let args = ["action".to_owned(), "new-tab".to_owned()];
    action_until(xdg, session, &args, "new-tab", || {
        let after = list_panes(xdg, session)?.tab_ids();
        if after.iter().any(|id| !before.contains(id)) {
            Ok(())
        } else {
            Err(format!("tabs still {after:?}; before tabs were {before:?}"))
        }
    });
}

pub(in crate::backend::zellij) fn spawn_sleep_pane(xdg: &Path, session: &str, cwd: &Path) {
    let before = PaneSnapshot::expect(xdg, session).live_work_count();
    let args = [
        "action".to_owned(),
        "new-pane".to_owned(),
        "--cwd".to_owned(),
        cwd.to_string_lossy().into_owned(),
        "--".to_owned(),
        "sleep".to_owned(),
        "600".to_owned(),
    ];
    action_until(xdg, session, &args, "new-pane", || {
        let after = list_panes(xdg, session)?.live_work_count();
        (after > before)
            .then_some(())
            .ok_or_else(|| format!("live work panes still {after}; before was {before}"))
    });
}

pub(in crate::backend::zellij) fn new_tab_template_dump(xdg: &Path, session: &str) -> String {
    let mut last_observation = "dump-layout was not checked".to_owned();
    for attempt in 0..DUMP_LAYOUT_ATTEMPTS {
        if attempt > 0 {
            std::thread::sleep(DUMP_LAYOUT_RETRY_DELAY);
        }
        let output = scoped_zellij(xdg)
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

pub(in crate::backend::zellij) fn reconcile_until_observed(
    xdg: &Path,
    opts: &SidebarPaneOptions,
    live: &SidebarLiveness,
) -> SidebarRecovery {
    reconcile_loop(xdg, opts, live, false)
}

pub(in crate::backend::zellij) fn reconcile_until_converged(
    xdg: &Path,
    opts: &SidebarPaneOptions,
    live: &SidebarLiveness,
) -> SidebarRecovery {
    reconcile_loop(xdg, opts, live, true)
}

fn reconcile_loop(
    xdg: &Path,
    opts: &SidebarPaneOptions,
    live: &SidebarLiveness,
    retry_deferral: bool,
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
        let transient = report == SidebarRecovery::default() || (retry_deferral && deferral_only);
        if !transient || Instant::now() >= deadline {
            return report;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
