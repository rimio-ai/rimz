use super::*;
use crate::ids::MuxClientId;
use crate::mux::{ClientPresence, ClientView};
use crate::pane::ClientPaneView;
use std::collections::BTreeMap;

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
