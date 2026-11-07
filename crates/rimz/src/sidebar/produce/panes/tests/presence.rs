use super::*;
use crate::ids::MuxClientId;
use crate::mux::{ClientPresence, ClientView};
use crate::pane::ClientPaneView;
use std::collections::BTreeMap;

#[cfg(target_os = "linux")]
#[test]
fn unfocused_terminals_keep_topology_and_afk_evidence_but_are_not_typing_viewers() {
    let pane = PaneId::from_parts(MuxName::Tmux, "%1");
    let client = MuxClientId::Tmux("human".to_owned());
    let clocks = BTreeMap::from([(client.clone(), 1_000)]);
    let view = ClientView {
        clients: vec![ClientPaneView {
            client_id: client.clone(),
            pane_id: pane.clone(),
        }],
        viewed_panes: vec![pane],
        presence: ClientPresence {
            human_clients: 1,
            client_input_ms: clocks.clone(),
            unfocused_clients: [client].into(),
        },
    };
    let (viewed, clients, presence, fresh) =
        frame_client_view(None, None, MuxName::Tmux, || Ok(view.clone()));
    let presence = presence.unwrap();
    assert_eq!(
        (
            viewed,
            clients,
            fresh,
            presence.human_clients,
            presence.last_input_ms,
            presence.client_input_ms,
            presence.client_views,
        ),
        (
            view.viewed_panes,
            view.clients,
            true,
            1,
            Some(1_000),
            clocks,
            Vec::new(),
        )
    );
}

#[test]
fn frame_client_samples_carry_clocks_from_pushed_direct_and_prior_views() {
    let pane_id = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let client = MuxClientId::Zellij(1);
    let clocks = BTreeMap::from([(client.clone(), 1_000)]);
    let view = ClientView {
        clients: vec![ClientPaneView {
            client_id: client,
            pane_id: pane_id.clone(),
        }],
        viewed_panes: vec![pane_id],
        presence: ClientPresence {
            human_clients: 1,
            client_input_ms: clocks.clone(),
            ..Default::default()
        },
    };
    let mut prior = frame(Vec::new());
    prior.client_views = view.clients.clone();
    prior.viewed_panes = view.viewed_panes.clone();
    prior.presence = Some(PresenceSample {
        client_input_ms: clocks.clone(),
        client_views: view.clients.clone(),
        ..presence_sample(1, Some(1_000), 1_000)
    });
    let unavailable = || {
        Err(crate::sidebar::produce::ProduceErr::Fixture {
            path: "unused".into(),
            reason: "client read failed".into(),
        })
    };
    let samples = [
        frame_client_view(Some(view.clone()), Some(&prior), MuxName::Zellij, || {
            panic!("pushed view needs no read")
        }),
        frame_client_view(None, Some(&prior), MuxName::Zellij, || Ok(view.clone())),
        frame_client_view(None, Some(&prior), MuxName::Zellij, unavailable),
        frame_client_view(None, Some(&prior), MuxName::Tmux, unavailable),
    ];
    let actual = samples.map(|(viewed, clients, presence, fresh)| {
        (
            presence.map(|sample| (sample.client_input_ms, sample.client_views)),
            fresh,
            clients == view.clients,
            viewed == view.viewed_panes,
        )
    });
    let mut legacy = serde_json::to_value(&prior).unwrap();
    for field in ["client_input_ms", "client_views"] {
        legacy["presence"].as_object_mut().unwrap().remove(field);
    }
    let legacy: PaneFrame = serde_json::from_value(legacy).unwrap();
    let legacy = legacy.presence.unwrap();
    assert_eq!(
        (
            actual,
            legacy.client_input_ms.is_empty() && legacy.client_views.is_empty()
        ),
        (
            [
                (
                    Some((clocks.clone(), view.clients.clone())),
                    true,
                    true,
                    true
                ),
                (
                    Some((clocks.clone(), view.clients.clone())),
                    true,
                    true,
                    true
                ),
                (Some((clocks, view.clients.clone())), false, true, true),
                (None, false, false, false),
            ],
            true
        )
    );
}

#[test]
fn reprobe_publishes_a_non_latest_clients_clock_without_changing_pane_pairs() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = crate::RuntimePaths::under(
        crate::WorkspaceId::from_project_root(dir.path()),
        dir.path(),
    )
    .unwrap();
    runtime.ensure_dirs().unwrap();
    let clients = [
        MuxClientId::Tmux("a".to_owned()),
        MuxClientId::Tmux("b".to_owned()),
    ];
    let mut prior = frame(vec![pane("%1", None, Some("/repo"))]);
    prior.produced_at_ms = unix_now_ms() + 60_000;
    prior.client_views = vec![ClientPaneView {
        client_id: clients[0].clone(),
        pane_id: first(&prior).pane_id.clone(),
    }];
    let pairs = prior.client_views.clone();
    prior.presence = Some(PresenceSample {
        client_input_ms: BTreeMap::from([(clients[0].clone(), 1_000), (clients[1].clone(), 2_000)]),
        client_views: pairs.clone(),
        ..presence_sample(2, Some(2_000), 1)
    });
    atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &prior).unwrap();
    let mut sample = prior.presence.clone().unwrap();
    sample.client_input_ms.insert(clients[0].clone(), 1_500);
    sample.sampled_at_ms = unix_now_ms();
    let mut producer = PaneProducer::seeded(&runtime, &prior.session_name);
    producer
        .tick_with(
            &runtime,
            MuxName::Tmux,
            &prior.session_name,
            None,
            None,
            &crate::diag::DiagSink::disabled(),
            &mut |_, _| panic!("fresh prior must not list panes"),
            &|_, _| Some(sample.clone()),
            &|_| None,
        )
        .unwrap();
    let published: PaneFrame =
        serde_json::from_slice(&std::fs::read(runtime.pane_frame_path()).unwrap()).unwrap();
    assert_eq!(
        (
            published.presence.unwrap().client_input_ms,
            published.client_views
        ),
        (sample.client_input_ms, pairs)
    );
}

#[test]
fn reprobe_publishes_pane_switches_without_changing_topology_or_input_clocks() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = crate::RuntimePaths::under(
        crate::WorkspaceId::from_project_root(dir.path()),
        dir.path(),
    )
    .unwrap();
    runtime.ensure_dirs().unwrap();
    let panes = ["%1", "%2"].map(|raw| PaneId::from_parts(MuxName::Tmux, raw));
    let client = MuxClientId::Tmux("a".to_owned());
    let mut prior = frame(
        panes
            .iter()
            .map(|id| crate::pane::PaneRef {
                pane_id: id.clone(),
                ..pane(id.raw(), None, Some("/repo"))
            })
            .collect(),
    );
    prior.produced_at_ms = unix_now_ms() + 60_000;
    prior.client_views = vec![ClientPaneView {
        client_id: client.clone(),
        pane_id: panes[0].clone(),
    }];
    prior.viewed_panes = vec![panes[0].clone()];
    prior.focused_pane = Some(panes[0].clone());
    prior.client_view_fresh = true;
    prior.presence = Some(PresenceSample {
        client_input_ms: BTreeMap::from([(client, 1_000)]),
        client_views: prior.client_views.clone(),
        ..presence_sample(1, Some(1_000), 1)
    });
    atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &prior).unwrap();
    let mut sample = prior.presence.clone().unwrap();
    sample.client_views[0].pane_id = panes[1].clone();
    sample.sampled_at_ms = unix_now_ms();
    let mut producer = PaneProducer::seeded(&runtime, &prior.session_name);
    producer
        .tick_with(
            &runtime,
            MuxName::Tmux,
            &prior.session_name,
            None,
            None,
            &crate::diag::DiagSink::disabled(),
            &mut |_, _| panic!("fresh prior must not list panes"),
            &|_, _| Some(sample.clone()),
            &|_| None,
        )
        .unwrap();
    let published: PaneFrame =
        serde_json::from_slice(&std::fs::read(runtime.pane_frame_path()).unwrap()).unwrap();
    assert_eq!(published.presence, Some(sample));
    assert_eq!(published.client_views, prior.client_views);
    assert_eq!(published.viewed_panes, prior.viewed_panes);
    assert_eq!(published.focused_pane, prior.focused_pane);
    assert_eq!(published.client_view_fresh, prior.client_view_fresh);
}

#[cfg(target_os = "linux")]
mod terminal_typing {
    use super::*;
    use crate::mux::typing::TypingRecord;
    use std::cell::Cell;
    use std::os::unix::fs::MetadataExt;

    struct Fixture {
        _dir: tempfile::TempDir,
        runtime: crate::RuntimePaths,
        producer: PaneProducer,
        ttys: HashMap<PaneId, PathBuf>,
        sample: PresenceSample,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let runtime = crate::RuntimePaths::under(
                crate::WorkspaceId::from_project_root(dir.path()),
                dir.path(),
            )
            .unwrap();
            runtime.ensure_dirs().unwrap();
            let mut prior = frame(vec![
                pane("terminal_1", None, None),
                pane("terminal_2", None, None),
            ]);
            prior.produced_at_ms = unix_now_ms() + 60_000;
            prior.client_views = prior
                .pane_states()
                .enumerate()
                .map(|(i, pane)| ClientPaneView {
                    client_id: MuxClientId::Zellij(u32::try_from(i).unwrap() + 1),
                    pane_id: pane.pane_id.clone(),
                })
                .collect();
            prior.viewed_panes = prior
                .client_views
                .iter()
                .map(|c| c.pane_id.clone())
                .collect();
            prior.focused_pane = Some(prior.viewed_panes[0].clone());
            prior.client_view_fresh = true;
            let sample = PresenceSample {
                client_views: prior.client_views.clone(),
                ..presence_sample(2, None, prior.produced_at_ms)
            };
            prior.presence = Some(sample.clone());
            let ttys = prior
                .pane_states()
                .map(|pane| {
                    let path = dir.path().join(pane.pane_id.raw());
                    std::fs::write(&path, b"").unwrap();
                    (pane.pane_id.clone(), path)
                })
                .collect();
            atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &prior).unwrap();
            let fixture = Self {
                producer: PaneProducer::seeded(&runtime, "s"),
                _dir: dir,
                runtime,
                ttys,
                sample,
            };
            fixture.input(100);
            fixture
        }

        fn input(&self, seconds: u64) {
            for path in self.ttys.values() {
                std::fs::File::open(path)
                    .unwrap()
                    .set_times(
                        std::fs::FileTimes::new()
                            .set_accessed(std::time::UNIX_EPOCH + Duration::from_secs(seconds)),
                    )
                    .unwrap();
            }
        }

        fn tick(&mut self, sample: &PresenceSample, reads: &Cell<usize>) -> Arc<PaneFrame> {
            self.producer
                .tick_with(
                    &self.runtime,
                    MuxName::Zellij,
                    "s",
                    None,
                    None,
                    &crate::diag::DiagSink::disabled(),
                    &mut |_, _| panic!("fresh prior must not list panes"),
                    &|_, _| {
                        reads.set(reads.get() + 1);
                        Some(sample.clone())
                    },
                    &|pane| self.ttys.get(&pane.pane_id).cloned(),
                )
                .unwrap()
        }

        fn record(&self) -> serde_json::Value {
            serde_json::to_value(TypingRecord::load(&self.runtime)).unwrap()
        }
    }

    #[test]
    fn ticks_baseline_then_leave_unchanged_records_and_frames_unwritten() {
        let mut fixture = Fixture::new();
        let reads = Cell::new(0);
        let sample = fixture.sample.clone();
        let frame_bytes = std::fs::read(fixture.runtime.pane_frame_path()).unwrap();
        fixture.tick(&sample, &reads);
        assert_eq!(
            (
                fixture.record()["panes"]["zellij:terminal_1"]["atime_s"].as_u64(),
                reads.get()
            ),
            (Some(100), 0)
        );
        let record_path = fixture.runtime.typing_path();
        let inode = std::fs::metadata(&record_path).unwrap().ino();
        fixture.tick(&sample, &reads);
        assert_eq!(std::fs::metadata(record_path).unwrap().ino(), inode);
        assert_eq!(
            reads.get(),
            0,
            "unchanged atimes need no direct client read"
        );
        assert_eq!(
            std::fs::read(fixture.runtime.pane_frame_path()).unwrap(),
            frame_bytes
        );
    }

    #[test]
    fn pending_advances_share_one_post_stat_read_and_departure_never_counts() {
        let mut fixture = Fixture::new();
        let reads = Cell::new(0);
        let mut sample = fixture.sample.clone();
        let prior = fixture.tick(&sample, &reads);
        fixture.input(200);
        sample.client_input_ms = sample
            .client_views
            .iter()
            .map(|client| (client.client_id.clone(), 200_000))
            .collect();
        sample.last_input_ms = Some(200_000);
        fixture.tick(&sample, &reads);
        let record = fixture.record();
        assert_eq!(
            (
                reads.get(),
                record["panes"]["zellij:terminal_1"]["input_ms"].as_u64(),
                record["panes"]["zellij:terminal_2"]["input_ms"].as_u64()
            ),
            (1, Some(200_000), Some(200_000))
        );
        fixture.input(300);
        let departed = PresenceSample {
            client_views: Vec::new(),
            ..sample
        };
        let returned = fixture.tick(&departed, &reads);
        let record = fixture.record();
        assert_eq!(reads.get(), 2);
        for pane in ["zellij:terminal_1", "zellij:terminal_2"] {
            assert_eq!(record["panes"][pane]["atime_s"], 300);
            assert_eq!(record["panes"][pane]["input_ms"], 200_000);
        }
        let published: PaneFrame =
            serde_json::from_slice(&std::fs::read(fixture.runtime.pane_frame_path()).unwrap())
                .unwrap();
        assert_eq!(returned.presence, Some(departed.clone()));
        assert_eq!(published.presence, Some(departed));
        assert_eq!(published.client_views, prior.client_views);
        assert_eq!(published.viewed_panes, prior.viewed_panes);
        assert_eq!(published.focused_pane, prior.focused_pane);
        assert_eq!(published.client_view_fresh, prior.client_view_fresh);
        assert_eq!(published.topology_stamp_ms, prior.topology_stamp_ms);
    }

    #[test]
    fn absent_entries_survive_live_input_or_exclusion_but_expired_entries_drop() {
        let mut fixture = Fixture::new();
        let mut entries = serde_json::Map::new();
        for (name, input, quiet) in [
            ("input", Some(unix_now_ms() + 60_000), 0),
            ("quiet", None, unix_now_ms() + 60_000),
            ("expired", Some(1), 1),
        ] {
            entries.insert(
                format!("zellij:{name}"),
                serde_json::json!({
                    "tty": null, "atime_s": 1, "viewers": [],
                    "quiet_until_ms": quiet, "input_ms": input
                }),
            );
        }
        atomic::write_temp_then_rename_cache(
            &fixture.runtime.typing_path(),
            &serde_json::json!({"panes": entries}),
        )
        .unwrap();
        fixture.tick(&fixture.sample.clone(), &Cell::new(0));
        let record = fixture.record();
        assert!(record["panes"].get("zellij:input").is_some());
        assert!(record["panes"].get("zellij:quiet").is_some());
        assert!(record["panes"].get("zellij:expired").is_none());
    }

    #[test]
    fn pending_client_reads_leave_the_typing_lock_available() {
        let mut fixture = Fixture::new();
        let sample = fixture.sample.clone();
        fixture.tick(&sample, &Cell::new(0));
        fixture.input(200);
        let lock_available = Cell::new(false);
        fixture
            .producer
            .tick_with(
                &fixture.runtime,
                MuxName::Zellij,
                "s",
                None,
                None,
                &crate::diag::DiagSink::disabled(),
                &mut |_, _| panic!("fresh prior must not list panes"),
                &|_, _| {
                    lock_available.set(TypingRecord::update(&fixture.runtime, |_| {}).is_ok());
                    Some(sample.clone())
                },
                &|pane| fixture.ttys.get(&pane.pane_id).cloned(),
            )
            .unwrap();
        assert!(
            lock_available.get(),
            "client read must not exclude pane-write stamps"
        );
    }

    #[test]
    fn tty_resolution_is_reused_until_the_pane_pid_changes() {
        let mut fixture = Fixture::new();
        let sample = fixture.sample.clone();
        let resolutions = Cell::new(0);
        for _ in 0..2 {
            fixture
                .producer
                .tick_with(
                    &fixture.runtime,
                    MuxName::Zellij,
                    "s",
                    None,
                    None,
                    &crate::diag::DiagSink::disabled(),
                    &mut |_, _| panic!("fresh prior must not list panes"),
                    &|_, _| Some(sample.clone()),
                    &|pane| {
                        resolutions.set(resolutions.get() + 1);
                        fixture.ttys.get(&pane.pane_id).cloned()
                    },
                )
                .unwrap();
        }
        assert_eq!(
            resolutions.get(),
            2,
            "unchanged pids reuse both tty resolutions"
        );
        let frame = Arc::make_mut(fixture.producer.prior.as_mut().unwrap());
        first_mut(frame).current.pid = Some(u32::MAX);
        first_mut(frame).current.command = Some("rimz sidebar serve".into());
        fixture
            .producer
            .tick_with(
                &fixture.runtime,
                MuxName::Zellij,
                "s",
                None,
                None,
                &crate::diag::DiagSink::disabled(),
                &mut |_, _| panic!("fresh prior must not list panes"),
                &|_, _| Some(sample.clone()),
                &|pane| {
                    resolutions.set(resolutions.get() + 1);
                    fixture.ttys.get(&pane.pane_id).cloned()
                },
            )
            .unwrap();
        assert_eq!(resolutions.get(), 3, "only the changed pid is re-resolved");
    }
}
