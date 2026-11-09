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
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        if crate::wakeup::heartbeat::fresh_sidebar_heartbeats(&opts.runtime)
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

/// Whether a newly-added pane proved its mount, and the cleanup's result when not.
#[derive(Debug)]
pub(super) enum MountProof {
    Verified,
    Unproven { cleanup: crate::mux::Result<()> },
}

/// Prove a newly-added pane belongs to the current build. Failed proof invokes
/// backend-native cleanup and returns its result, so the caller can report a
/// failed kill beside its exact transport error.
pub(super) fn prove_sidebar_mount(
    opts: &SidebarPaneOptions,
    mux: MuxName,
    pane: &PaneId,
    build: &str,
    cleanup: impl FnOnce() -> crate::mux::Result<()>,
) -> MountProof {
    finish_mount_proof(wait_for_sidebar_heartbeat(opts, mux, pane, build), cleanup)
}

fn finish_mount_proof(
    verified: bool,
    cleanup: impl FnOnce() -> crate::mux::Result<()>,
) -> MountProof {
    if verified {
        MountProof::Verified
    } else {
        MountProof::Unproven { cleanup: cleanup() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_mount_proof_cleans_up_only_after_failure() {
        let cleaned = std::cell::Cell::new(0);
        let clean = || {
            cleaned.set(cleaned.get() + 1);
            Ok(())
        };
        assert!(matches!(
            finish_mount_proof(true, clean),
            MountProof::Verified
        ));
        assert_eq!(cleaned.get(), 0);
        assert!(matches!(
            finish_mount_proof(false, clean),
            MountProof::Unproven { cleanup: Ok(()) }
        ));
        assert_eq!(cleaned.get(), 1);
    }

    #[test]
    fn failed_mount_proof_surfaces_a_failed_cleanup_kill() {
        let proof = finish_mount_proof(false, || {
            Err(crate::mux::MuxErr::Output {
                program: "tmux".to_owned(),
                reason: "kill-pane refused".to_owned(),
            })
        });
        let MountProof::Unproven { cleanup: Err(err) } = proof else {
            panic!("a failed cleanup kill must reach the caller: {proof:?}");
        };
        assert!(err.to_string().contains("kill-pane refused"), "{err}");
    }
}
