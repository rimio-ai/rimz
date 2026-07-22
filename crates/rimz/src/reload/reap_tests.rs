use super::*;

fn workspace() -> KnownWorkspace {
    KnownWorkspace {
        workspace_id: crate::WorkspaceId::parse("ws_f89e49906df0621ad2765112").unwrap(),
        project_root: PathBuf::from("/repo"),
        session_name: "rimz-test".to_owned(),
        root_class: crate::workspace::RootClass::Directory,
        rimz_bin: None,
        updated_at: jiff::Timestamp::UNIX_EPOCH,
    }
}

fn process(pid: u32) -> ProcInfo {
    let ws = workspace();
    ProcInfo {
        pid,
        ppid: 1,
        real_uid: 1_000,
        cmdline: format!(
            "rimz sidebar serve --workspace-id {} --session {}",
            ws.workspace_id, ws.session_name
        ),
    }
}

fn pane(raw: &str) -> PaneId {
    PaneId::from_parts(MuxName::Zellij, raw)
}

fn listing(panes: &[PaneId], observed_at_ms: u64) -> PaneListing {
    PaneListing {
        panes: panes
            .iter()
            .cloned()
            .map(crate::pane::PaneRef::from_id)
            .collect(),
        observed_at_ms,
        ..Default::default()
    }
}

#[test]
fn cache_omission_is_spared_and_recorded_when_mux_finds_the_pane() {
    let pane = pane("terminal_5");
    let candidate = ReapCandidate {
        pid: 42,
        pane: pane.clone(),
    };
    let mut listings = [
        Ok::<_, &'static str>(listing(std::slice::from_ref(&pane), 1_000)),
        Ok(listing(std::slice::from_ref(&pane), 1_500)),
    ]
    .into_iter();

    let confirmation =
        confirm_reap_candidates(vec![candidate.clone()], || listings.next().unwrap(), || {})
            .unwrap();

    assert!(confirmation.confirmed.is_empty());
    assert_eq!(confirmation.spared, vec![candidate]);
    assert_eq!(
        pane_cache_divergence_events(
            &confirmation.spared,
            Some(900),
            &confirmation.first_panes,
            confirmation.first_observed_at_ms,
            confirmation.second_observed_at_ms,
        ),
        vec![DiagEvent::PaneCacheDivergence {
            pane_id: pane.to_string(),
            pid: 42,
            cache_observed_at_ms: Some(900),
            authoritative_observed_at_ms: 1_000,
        }]
    );
}

#[test]
fn authoritative_rosters_spare_every_omitted_sidebar() {
    let panes = [pane("terminal_5"), pane("terminal_6")];
    let candidates = panes
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, pane)| ReapCandidate {
            pid: 42 + index as u32,
            pane,
        })
        .collect::<Vec<_>>();
    let roster = panes.iter().cloned().collect::<HashSet<_>>();

    let (confirmed, spared) = partition_confirmed(candidates.clone(), &roster, &roster);

    assert!(confirmed.is_empty());
    assert_eq!(spared, candidates);
}

#[test]
fn either_authoritative_listing_failure_abstains() {
    let candidate = ReapCandidate {
        pid: 42,
        pane: pane("terminal_5"),
    };
    let first_failed = confirm_reap_candidates(
        vec![candidate.clone()],
        || Err::<PaneListing, _>("first failed"),
        || panic!("a first-probe failure must not pause or probe again"),
    );
    assert_eq!(first_failed.unwrap_err(), "first failed");

    let mut listings = [
        Ok::<_, &'static str>(listing(&[], 1_000)),
        Err("second failed"),
    ]
    .into_iter();
    let second_failed =
        confirm_reap_candidates(vec![candidate], || listings.next().unwrap(), || {});
    assert_eq!(second_failed.unwrap_err(), "second failed");
}

#[test]
fn double_authoritative_absence_records_each_kill_and_escalation() {
    let candidate = ReapCandidate {
        pid: 42,
        pane: pane("terminal_5"),
    };
    let (confirmed, spared) =
        partition_confirmed(vec![candidate.clone()], &HashSet::new(), &HashSet::new());
    assert!(spared.is_empty());
    assert_eq!(confirmed, vec![candidate.clone()]);

    assert_eq!(
        sidebar_orphan_reaped_events(
            &confirmed,
            &recovery::KillOutcome {
                signalled: vec![42],
                sigkilled: vec![42],
            },
            1_000,
            1_500,
        ),
        vec![DiagEvent::SidebarOrphanReaped {
            pane_id: candidate.pane.to_string(),
            pid: 42,
            first_confirmed_at_ms: 1_000,
            second_confirmed_at_ms: 1_500,
            sigkilled: true,
        }]
    );
}

#[test]
fn old_reexeced_process_is_nominated_when_the_cache_omits_its_pane() {
    let ws = workspace();
    let proc = process(42);
    let pane = pane("terminal_5");
    let now = jiff::Timestamp::from_second(1_000_000).unwrap();
    let old = jiff::Timestamp::from_second(
        now.as_second() - i64::try_from(crate::sidebar::FRESH_PANE_GRACE.as_secs()).unwrap() - 1,
    )
    .unwrap();

    let candidates = assemble_reap_candidates(
        ReapCandidateInputs {
            procs: &[proc],
            my_uid: 1_000,
            protected: &HashSet::new(),
            mux: MuxName::Zellij,
            workspace: &ws,
            positive_panes: &HashSet::new(),
            now,
        },
        |_| Some(old),
        |_, _| Some(pane.clone()),
        |_| true,
    );

    assert_eq!(candidates, vec![ReapCandidate { pid: 42, pane }]);
}

#[test]
fn foreign_domain_process_is_not_nominated() {
    let ws = workspace();
    let proc = process(42);
    let now = jiff::Timestamp::from_second(1_000_000).unwrap();

    let candidates = assemble_reap_candidates(
        ReapCandidateInputs {
            procs: &[proc],
            my_uid: 1_000,
            protected: &HashSet::new(),
            mux: MuxName::Zellij,
            workspace: &ws,
            positive_panes: &HashSet::new(),
            now,
        },
        |_| None,
        |_, _| Some(pane("terminal_5")),
        |_| false,
    );

    assert!(candidates.is_empty());
}
