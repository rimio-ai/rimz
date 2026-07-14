use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tempfile::TempDir;

use super::*;

struct Harness {
    _dir: TempDir,
    runtime: RuntimePaths,
    workspace_id: WorkspaceId,
}

impl Harness {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        Self {
            _dir: dir,
            runtime,
            workspace_id,
        }
    }

    fn write(&self, id: &SidebarInstanceId) -> PathBuf {
        let heartbeat = SidebarHeartbeat::new(
            self.workspace_id.clone(),
            id.clone(),
            MuxName::Tmux,
            "session",
            self.runtime.sock_dir.join("sidebar.sock"),
            None,
        );
        let path = self.runtime.sidebar_heartbeat_path(id);
        std::fs::write(&path, serde_json::to_vec(&heartbeat).unwrap()).unwrap();
        path
    }
}

fn instance(hex_tail: &str) -> SidebarInstanceId {
    SidebarInstanceId::parse(&format!("sb_{hex_tail:0>32}")).unwrap()
}

fn make_stale(path: &Path) {
    std::fs::File::open(path)
        .unwrap()
        .set_modified(SystemTime::now() - SIDEBAR_HEARTBEAT_TTL - Duration::from_secs(1))
        .unwrap();
}

#[test]
fn producer_election_tracker_invalid_elder_falls_back_to_next_valid() {
    #[derive(Clone, Copy)]
    enum Invalid {
        Removed,
        Stale,
        Malformed,
        Protocol,
        Workspace,
        Identity,
    }

    for invalid in [
        Invalid::Removed,
        Invalid::Stale,
        Invalid::Malformed,
        Invalid::Protocol,
        Invalid::Workspace,
        Invalid::Identity,
    ] {
        let h = Harness::new();
        let first = instance("01");
        let backup = instance("02");
        let own = instance("09");
        let first_path = h.write(&first);
        let backup_path = h.write(&backup);
        let modified = SystemTime::now() - Duration::from_secs(1);
        for path in [&first_path, &backup_path] {
            std::fs::File::open(path)
                .unwrap()
                .set_modified(modified)
                .unwrap();
        }
        let tracker = ProducerElectionTracker::new(h.runtime.clone(), own.clone());
        assert_eq!(tracker.elder_instance_at(modified), Some(first.clone()));

        match invalid {
            Invalid::Removed => std::fs::remove_file(&first_path).unwrap(),
            Invalid::Stale => make_stale(&first_path),
            Invalid::Malformed => std::fs::write(&first_path, b"{ not json").unwrap(),
            Invalid::Protocol | Invalid::Workspace | Invalid::Identity => {
                let mut heartbeat = SidebarHeartbeat::read_from(&first_path).unwrap();
                match invalid {
                    Invalid::Protocol => heartbeat.protocol_version = "rimz.plugin.v0".into(),
                    Invalid::Workspace => {
                        heartbeat.workspace_id =
                            WorkspaceId::from_project_root(&h.runtime.root.join("other"));
                    }
                    Invalid::Identity => heartbeat.instance_id = own.clone(),
                    _ => unreachable!(),
                }
                std::fs::write(&first_path, serde_json::to_vec(&heartbeat).unwrap()).unwrap();
            }
        }

        assert_eq!(
            tracker.elder_instance_at(modified + SIDEBAR_HEARTBEAT_TTL),
            Some(backup),
        );
        assert_eq!(tracker.full_scan_count(), 2);
    }
}
