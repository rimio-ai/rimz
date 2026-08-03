//! What the live pane frame contributes to the fold: the link-health sidecar,
//! viewed panes, and the presence verdict `project_local` classifies.

use super::*;

#[test]
fn link_stats_freshness_ages_from_fresh_through_stale_to_expired() {
    // One sidecar sampled at 1_000, read back at three ages.
    for (now_ms, expected) in [
        (
            1_500,
            Some(crate::store::snapshot::SidebarLinkFreshness::Fresh),
        ),
        (
            12_000,
            Some(crate::store::snapshot::SidebarLinkFreshness::Stale),
        ),
        (122_001, None),
    ] {
        let (_dir, runtime, mut snapshot) = runtime();
        let file = LinkStatsFile::new(1_000, "client".to_owned(), stats(Some(230), 4));
        atomic::write_temp_then_rename_cache(&crate::remote::link::stats_path(&runtime), &file)
            .unwrap();

        fold_link_stats(&mut snapshot, &runtime, now_ms);

        let Some(freshness) = expected else {
            assert!(
                snapshot.link.is_none(),
                "expired stats disappear at {now_ms}"
            );
            continue;
        };
        let link = snapshot.link.expect("link badge");
        assert_eq!(link.freshness, freshness, "at {now_ms}");
        assert_eq!(link.rtt_ms, Some(230));
        assert_eq!(link.miss_pct, 4);
        assert_eq!(link.tier, LinkTier::Degraded);
        assert_eq!(link.sampled_at_ms, 1_000);
    }
}

#[test]
fn corrupt_or_wrong_version_stats_disappear() {
    let (_dir, runtime, mut snapshot) = runtime();
    let path = crate::remote::link::stats_path(&runtime);

    atomic::write_bytes_atomically(&path, b"not json").unwrap();
    fold_link_stats(&mut snapshot, &runtime, 1_000);
    assert!(snapshot.link.is_none(), "unparseable sidecar");

    let mut file = LinkStatsFile::new(1_000, "client".to_owned(), stats(Some(42), 0));
    file.v = "rimz.link.v0".to_owned();
    atomic::write_temp_then_rename_cache(&path, &file).unwrap();
    fold_link_stats(&mut snapshot, &runtime, 1_000);
    assert!(snapshot.link.is_none(), "wrong schema version");
}

#[test]
fn frame_fold_carries_viewed_panes_onto_snapshot() {
    let (_dir, runtime, snapshot) = runtime();
    let pane_id = crate::ids::PaneId::from_parts(crate::ids::MuxName::Zellij, "terminal_1");
    let frame = crate::sidebar::frame::PaneFrame {
        produced_at_ms: 1_000,
        observed_at_ms: 1_000,
        topology_stamp_ms: None,
        metrics_stamp_ms: None,
        build: None,
        session_name: "rimz-test".to_owned(),
        tabs: Vec::new(),
        carried_panes: Vec::new(),
        viewed_panes: vec![pane_id.clone()],
        client_views: Vec::new(),
        focused_pane: Some(pane_id.clone()),
        presence: None,
    };

    let snapshot = fold_cached(snapshot, Some(&frame), &runtime);

    assert_eq!(snapshot.viewed_panes, vec![pane_id]);
    assert_eq!(
        snapshot.focused_pane,
        snapshot.viewed_panes.first().cloned()
    );
}

#[test]
fn frame_fold_carries_presence_onto_snapshot() {
    let (_dir, runtime, snapshot) = runtime();
    let mut frame = crate::sidebar::frame::assemble_frame(Vec::new(), 1_000, "rimz-test");
    frame.presence = Some(crate::store::snapshot::PresenceSample {
        human_clients: 0,
        last_input_ms: None,
        sampled_at_ms: 1_000,
    });

    let snapshot = fold_cached(snapshot, Some(&frame), &runtime);

    assert_eq!(
        snapshot.presence,
        Some(crate::store::snapshot::SidebarPresence::Detached)
    );
}

#[test]
fn enrich_workspace_rejects_sidebar_chrome_and_defers_presence() {
    let (_dir, runtime, mut snapshot) = runtime();
    snapshot.now = Timestamp::from_millisecond(1_700_000_000_000).unwrap();
    let own = pane(
        "terminal_sidebar",
        crate::pane::SIDEBAR_CHROME_TITLE,
        "/repo",
    );
    let own_id = own.pane_id.clone();
    let working = pane("terminal_work", "zsh", "/repo");
    let mut frame =
        crate::sidebar::frame::assemble_frame(vec![own, working], 1_700_000_000_000, "rimz-test");
    frame.presence = Some(crate::store::snapshot::PresenceSample {
        human_clients: 1,
        last_input_ms: Some(1_699_999_999_000),
        sampled_at_ms: 1_700_000_000_000,
    });

    let workspace = enrich_workspace(
        snapshot,
        Some(&frame),
        &runtime,
        None,
        cached_opts(),
        &crate::diag::DiagSink::disabled(),
    );

    assert!(
        workspace
            .snapshot()
            .rows()
            .all(|row| row.pane.as_ref().is_none_or(|pane| pane.pane_id != own_id)),
        "sidebar chrome is rejected before shared pairing and admission"
    );
    assert!(
        workspace
            .snapshot()
            .agent_panes
            .iter()
            .all(|agent| agent.pane_id != own_id)
    );
    assert_eq!(
        workspace.snapshot().presence,
        None,
        "presence stays unclassified until project_local runs"
    );
}

/// Every row asserts the same idle verdict; each pins a separate fix, so the
/// inputs and the assertion are carried over verbatim.
#[test]
fn presence_reads_idle_from_the_snapshot_clock_on_local_and_remote_rooms() {
    let cases = [
        // 0763535a fix(sidebar): keep remote rooms present while attached
        ("local room", false, 10_000),
        // 7af97227 fix(sidebar): honor remote tmux idle window
        ("remote room with live link stats", true, 10_000),
        // 1d523b22 fix(sidebar): keep AFK presence live
        ("idle duration tracks snapshot now", false, 30_000),
    ];

    for (label, remote, sampled_ago_ms) in cases {
        let (_dir, runtime, snapshot) = runtime();
        let snapshot = SidebarSnapshot::build(
            snapshot.workspace_id.clone(),
            Vec::new(),
            Timestamp::from_second(1_750_000_000).unwrap(),
        );
        if remote {
            let file = LinkStatsFile::new(unix_now_ms(), "client".to_owned(), stats(Some(42), 0));
            atomic::write_temp_then_rename_cache(&crate::remote::link::stats_path(&runtime), &file)
                .unwrap();
        }
        let now_ms = snapshot_now_ms(&snapshot);
        let mut frame = crate::sidebar::frame::assemble_frame(Vec::new(), 1_000, "rimz-test");
        frame.presence = Some(crate::store::snapshot::PresenceSample {
            human_clients: 1,
            last_input_ms: Some(now_ms - 999_000),
            sampled_at_ms: now_ms - sampled_ago_ms,
        });

        let snapshot = fold_producing(snapshot, Some(&frame), &runtime);

        assert_eq!(
            snapshot.presence,
            Some(crate::store::snapshot::SidebarPresence::Idle { idle_ms: 999_000 }),
            "{label}"
        );
    }
}
