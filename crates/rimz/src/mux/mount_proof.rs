//! Current-build heartbeat proof for sidebar panes mounted during repair.

use std::time::{Duration, Instant};

use crate::ids::{MuxName, PaneId};
use crate::mux::SidebarPaneOptions;

/// Wait until the newly-mounted pane publishes a fresh heartbeat for the
/// expected executable generation. Executors call this before committing a
/// replacement by closing its old pane.
fn wait_for_sidebar_heartbeat(
    opts: &SidebarPaneOptions,
    mux: MuxName,
    pane: &PaneId,
    build: &str,
) -> bool {
    #[cfg(feature = "testkit")]
    if opts
        .extra_env
        .get("RIMZ_TEST_ASSUME_SIDEBAR_HEARTBEAT")
        .is_some_and(|value| value == "1")
    {
        return true;
    }
    let Ok(runtime) = crate::store::RuntimePaths::for_workspace(opts.workspace_id.clone()) else {
        return false;
    };
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        if crate::sidebar::fresh_sidebar_heartbeats(&runtime)
            .into_iter()
            .any(|heartbeat| {
                heartbeat.mux == mux
                    && heartbeat.session_name == opts.session_name
                    && heartbeat.pane_id.as_ref() == Some(pane)
                    && heartbeat.build.as_deref() == Some(build)
            })
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub(super) fn sidebar_build_identity(opts: &SidebarPaneOptions) -> crate::mux::Result<String> {
    crate::build_id::of_file(&opts.rimz_bin).map_err(|err| crate::mux::MuxErr::Output {
        program: opts.rimz_bin.display().to_string(),
        reason: format!("cannot verify sidebar repair build: {err}"),
    })
}

/// Prove a newly-added pane belongs to the current build. Failed proof invokes
/// backend-native best-effort cleanup before the caller constructs its exact
/// transport error.
pub(super) fn prove_sidebar_mount(
    opts: &SidebarPaneOptions,
    mux: MuxName,
    pane: &PaneId,
    build: &str,
    cleanup: impl FnOnce(),
) -> bool {
    finish_mount_proof(wait_for_sidebar_heartbeat(opts, mux, pane, build), cleanup)
}

fn finish_mount_proof(verified: bool, cleanup: impl FnOnce()) -> bool {
    if verified {
        true
    } else {
        cleanup();
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_mount_proof_cleans_up_only_after_failure() {
        let cleaned = std::cell::Cell::new(0);
        assert!(finish_mount_proof(true, || cleaned.set(cleaned.get() + 1)));
        assert_eq!(cleaned.get(), 0);
        assert!(!finish_mount_proof(false, || cleaned.set(cleaned.get() + 1)));
        assert_eq!(cleaned.get(), 1);
    }
}
