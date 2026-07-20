//! Claude remote-control host liveness from the bridge pointer.
//!
//! `claude remote-control` records the process serving a project root in
//! `<config>/projects/<project>/bridge-pointer.json`, carrying the pid and the
//! kernel start token that distinguishes it from a recycled pid. Reading that
//! pointer turns host health from "a pane with the right title still exists"
//! into evidence about the process that actually serves the room.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::local_sessions::project_directory_name;
use super::spend::claude_config_dirs;

const POINTER_FILE: &str = "bridge-pointer.json";

/// What the bridge pointer says about the host serving one project root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostLiveness {
    /// No pointer for this project root — the host has not run here.
    Unknown,
    /// A pointer names a process that is gone or was replaced by a recycled pid.
    Down,
    /// The recorded process is still serving.
    Up { pid: u32 },
}

#[derive(Debug, Deserialize)]
struct BridgePointer {
    pid: Option<u32>,
    #[serde(rename = "procStart")]
    proc_start: Option<String>,
}

/// Every config root's pointer for `project_root`, in discovery order.
fn pointer_paths(project_root: &Path) -> Vec<PathBuf> {
    let project = project_directory_name(project_root);
    claude_config_dirs()
        .into_iter()
        .map(|config| config.join("projects").join(&project).join(POINTER_FILE))
        .collect()
}

/// Probe the host serving `project_root`. The first readable pointer decides;
/// a pointer that names a dead process reports `Down` rather than falling
/// through to another config root, because that pointer is the live answer.
pub fn probe(project_root: &Path) -> HostLiveness {
    probe_with(project_root, crate::proc::process_is_live)
}

fn probe_with(
    project_root: &Path,
    mut is_live: impl FnMut(u32, Option<&str>) -> bool,
) -> HostLiveness {
    for path in pointer_paths(project_root) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(pointer) = serde_json::from_str::<BridgePointer>(&text) else {
            tracing::debug!(
                path = %path.display(),
                "Claude bridge pointer did not parse; treating the host as down",
            );
            return HostLiveness::Down;
        };
        return liveness_from(&pointer, &mut is_live);
    }
    HostLiveness::Unknown
}

fn liveness_from(
    pointer: &BridgePointer,
    is_live: &mut impl FnMut(u32, Option<&str>) -> bool,
) -> HostLiveness {
    let Some(pid) = pointer.pid else {
        return HostLiveness::Down;
    };
    if is_live(pid, pointer.proc_start.as_deref()) {
        HostLiveness::Up { pid }
    } else {
        HostLiveness::Down
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pointer(json: &str) -> BridgePointer {
        serde_json::from_str(json).expect("pointer")
    }

    #[test]
    fn a_live_pid_with_a_matching_start_token_is_up() {
        let recorded = pointer(r#"{"pid": 42, "procStart": "440836208"}"#);
        let mut is_live = |pid: u32, start: Option<&str>| {
            assert_eq!(pid, 42);
            assert_eq!(start, Some("440836208"));
            true
        };
        assert_eq!(
            liveness_from(&recorded, &mut is_live),
            HostLiveness::Up { pid: 42 }
        );
    }

    #[test]
    fn a_dead_or_recycled_pid_is_down() {
        let recorded = pointer(r#"{"pid": 42, "procStart": "440836208"}"#);
        let mut is_live = |_: u32, _: Option<&str>| false;
        assert_eq!(liveness_from(&recorded, &mut is_live), HostLiveness::Down);
    }

    #[test]
    fn a_pointer_without_a_pid_is_down() {
        let recorded = pointer(r#"{"procStart": "440836208"}"#);
        let mut is_live = |_: u32, _: Option<&str>| panic!("must not probe without a pid");
        assert_eq!(liveness_from(&recorded, &mut is_live), HostLiveness::Down);
    }

    #[test]
    fn a_pointer_without_a_start_token_still_probes_the_pid() {
        let recorded = pointer(r#"{"pid": 42}"#);
        let mut is_live = |_: u32, start: Option<&str>| {
            assert_eq!(start, None);
            true
        };
        assert_eq!(
            liveness_from(&recorded, &mut is_live),
            HostLiveness::Up { pid: 42 }
        );
    }

    #[test]
    fn extra_pointer_fields_are_ignored() {
        let recorded = pointer(
            r#"{"pid": 42, "procStart": "1", "sessionId": "s", "environmentId": "e", "source": "standalone"}"#,
        );
        let mut is_live = |_: u32, _: Option<&str>| true;
        assert_eq!(
            liveness_from(&recorded, &mut is_live),
            HostLiveness::Up { pid: 42 }
        );
    }

    #[test]
    fn a_project_root_with_no_pointer_is_unknown() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            probe_with(dir.path(), |_: u32, _: Option<&str>| true),
            HostLiveness::Unknown,
            "an unseen project root has no pointer to read",
        );
    }
}
