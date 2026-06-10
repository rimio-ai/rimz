use super::*;
use crate::remote::link::{LinkStats, LinkStatsFile, LinkTier};

fn runtime() -> (tempfile::TempDir, RuntimePaths, SidebarSnapshot) {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new(), Timestamp::now());
    (dir, runtime, snapshot)
}

fn stats(rtt_ms: Option<u32>, miss_pct: u16) -> LinkStats {
    LinkStats {
        rtt_ms,
        miss_pct,
        window: 30,
        bandwidth_bps: None,
    }
}

#[test]
fn link_stats_sidecar_folds_into_snapshot() {
    let (_dir, runtime, mut snapshot) = runtime();
    let file = LinkStatsFile::new(1_000, "client".to_owned(), stats(Some(230), 4));
    atomic::write_temp_then_rename_cache(&crate::remote::link::stats_path(&runtime), &file)
        .unwrap();

    fold_link_stats(&mut snapshot, &runtime, 1_500);

    let link = snapshot.link.expect("link badge");
    assert_eq!(link.rtt_ms, Some(230));
    assert_eq!(link.miss_pct, 4);
    assert_eq!(link.tier, LinkTier::Degraded);
    assert_eq!(link.freshness, crate::SidebarLinkFreshness::Fresh);
    assert_eq!(link.sampled_at_ms, 1_000);
}

#[test]
fn stale_link_stats_render_as_stale_until_expired() {
    let (_dir, runtime, mut snapshot) = runtime();
    let file = LinkStatsFile::new(1_000, "client".to_owned(), stats(Some(42), 0));
    atomic::write_temp_then_rename_cache(&crate::remote::link::stats_path(&runtime), &file)
        .unwrap();

    fold_link_stats(&mut snapshot, &runtime, 12_000);
    assert_eq!(
        snapshot.link.as_ref().unwrap().freshness,
        crate::SidebarLinkFreshness::Stale
    );

    fold_link_stats(&mut snapshot, &runtime, 122_001);
    assert!(snapshot.link.is_none(), "expired stats disappear");
}

#[test]
fn corrupt_or_wrong_version_stats_disappear() {
    let (_dir, runtime, mut snapshot) = runtime();
    atomic::write_bytes_atomically(&crate::remote::link::stats_path(&runtime), b"not json")
        .unwrap();
    fold_link_stats(&mut snapshot, &runtime, 1_000);
    assert!(snapshot.link.is_none());

    let path = crate::remote::link::stats_path(&runtime);
    let mut file = LinkStatsFile::new(1_000, "client".to_owned(), stats(Some(42), 0));
    file.v = "rimz.link.v0".to_owned();
    atomic::write_temp_then_rename_cache(&path, &file).unwrap();
    fold_link_stats(&mut snapshot, &runtime, 1_000);
    assert!(snapshot.link.is_none());
}
