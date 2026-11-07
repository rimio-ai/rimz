use super::*;
use crate::ids::{MuxClientId, MuxName};
use crate::mux::typing::{InputSource, TypingRecord};
use crate::mux::{ClientPresence, ClientView};
use crate::pane::ClientPaneView;
use crate::store::snapshot::PresenceSample;
use std::collections::HashMap;
use std::time::Duration;

fn write_inputs(runtime: &RuntimePaths, inputs: &[(&PaneId, u64)]) -> TypingRecord {
    let panes: serde_json::Map<_, _> = inputs
        .iter()
        .map(|(pane, input)| {
            (
                pane.to_string(),
                serde_json::json!({
                    "tty": null, "atime_s": null, "viewers": [],
                    "quiet_until_ms": 0, "input_ms": input
                }),
            )
        })
        .collect();
    atomic::write_temp_then_rename_cache(
        &runtime.typing_path(),
        &serde_json::json!({"panes": panes}),
    )
    .unwrap();
    TypingRecord::load(runtime)
}

#[test]
fn frame_typing_deadlines_match_live_send_and_clear_without_a_frame_or_hold() {
    let (_dir, runtime, mut snapshot) = runtime();
    snapshot.now = Timestamp::from_millisecond(115_000).unwrap();
    let panes = ["%1", "%2", "%3"].map(|raw| PaneId::from_parts(MuxName::Tmux, raw));
    let clients = ["a", "b", "c", "unknown", "detached"].map(|id| MuxClientId::Tmux(id.to_owned()));
    let view = ClientView {
        clients: [
            (&clients[0], &panes[0]),
            (&clients[1], &panes[1]),
            (&clients[2], &panes[0]),
            (&clients[3], &panes[2]),
        ]
        .map(|(client, pane)| ClientPaneView {
            client_id: client.clone(),
            pane_id: pane.clone(),
        })
        .to_vec(),
        viewed_panes: panes.to_vec(),
        presence: ClientPresence {
            human_clients: 4,
            client_input_ms: BTreeMap::from([
                (clients[0].clone(), 100_000),
                (clients[1].clone(), 200_000),
                (clients[2].clone(), 110_000),
                (clients[4].clone(), 300_000),
            ]),
            ..Default::default()
        },
    };
    let hold = Duration::from_secs(30);
    let record = write_inputs(&runtime, &[(&panes[0], 100_000), (&panes[1], 150_000)]);
    let source = InputSource::for_platform(&record, &view.clients, &view.presence.client_input_ms);
    let expected: HashMap<_, _> = panes
        .iter()
        .filter_map(|pane| {
            Some((
                pane.clone(),
                Timestamp::from_millisecond(source.deadline(pane, Some(hold))? as i64).unwrap(),
            ))
        })
        .collect();
    assert_eq!(
        expected,
        HashMap::from([
            (
                panes[0].clone(),
                Timestamp::from_millisecond(if InputSource::uses_tty() {
                    130_000
                } else {
                    140_000
                })
                .unwrap()
            ),
            (
                panes[1].clone(),
                Timestamp::from_millisecond(if InputSource::uses_tty() {
                    180_000
                } else {
                    230_000
                })
                .unwrap()
            ),
        ])
    );
    let mut frame = crate::sidebar::frame::assemble_frame(Vec::new(), 115_000, "rimz-test");
    frame.client_views = view.clients.clone();
    frame.presence = Some(PresenceSample {
        human_clients: view.presence.human_clients,
        last_input_ms: view.presence.last_input_ms(),
        client_input_ms: view.presence.client_input_ms.clone(),
        client_views: view.clients.clone(),
        sampled_at_ms: 115_000,
    });
    let mut actual = Vec::new();
    for topology_pairs in [view.clients.clone(), Vec::new()] {
        frame.client_views = topology_pairs;
        for mut opts in [cached_opts(), producing_opts()] {
            let mut config = crate::config::MachineConfig::default();
            config.harness.typing_hold = Some(hold);
            opts.config = Some(Arc::new(config));
            actual.push(fold(snapshot.clone(), Some(&frame), &runtime, opts).typing_until);
        }
    }
    snapshot.typing_until = expected.clone();
    actual.push(fold_cached(snapshot.clone(), None, &runtime).typing_until);
    let mut config = crate::config::MachineConfig::default();
    config.harness.typing_hold = None;
    actual.push(
        fold(
            snapshot.clone(),
            Some(&frame),
            &runtime,
            FoldOpts {
                config: Some(Arc::new(config)),
                ..cached_opts()
            },
        )
        .typing_until,
    );
    frame.presence = None;
    actual.push(fold_cached(snapshot, Some(&frame), &runtime).typing_until);
    assert_eq!(
        actual,
        vec![
            expected.clone(),
            expected.clone(),
            expected.clone(),
            expected.clone(),
            HashMap::new(),
            HashMap::new(),
            if InputSource::uses_tty() {
                expected
            } else {
                HashMap::new()
            }
        ]
    );
}

#[test]
fn typing_deadlines_reread_pane_input_without_moving_topology_focus() {
    let (_dir, runtime, _) = runtime();
    let panes = ["%1", "%2"].map(|raw| PaneId::from_parts(MuxName::Tmux, raw));
    let client = MuxClientId::Tmux("a".to_owned());
    let mut frame = crate::sidebar::frame::assemble_frame(Vec::new(), 1, "rimz-test");
    frame.client_views = vec![ClientPaneView {
        client_id: client.clone(),
        pane_id: panes[0].clone(),
    }];
    frame.focused_pane = Some(panes[0].clone());
    frame.presence = Some(PresenceSample {
        human_clients: 1,
        last_input_ms: Some(100_000),
        client_input_ms: BTreeMap::from([(client.clone(), 100_000)]),
        client_views: vec![ClientPaneView {
            client_id: client,
            pane_id: panes[1].clone(),
        }],
        sampled_at_ms: 101_000,
    });
    let mut actual = Vec::new();
    for input in [200_000, 300_000] {
        write_inputs(&runtime, &[(&panes[0], input)]);
        actual.push(typing_deadlines(
            &runtime,
            Some(&frame),
            Some(Duration::from_secs(30)),
        ));
    }
    let expected = if InputSource::uses_tty() {
        [230_000, 330_000].map(|until| {
            HashMap::from([(
                panes[0].clone(),
                Timestamp::from_millisecond(until).unwrap(),
            )])
        })
    } else {
        [130_000, 130_000].map(|until| {
            HashMap::from([(
                panes[1].clone(),
                Timestamp::from_millisecond(until).unwrap(),
            )])
        })
    };
    assert_eq!(actual, expected);
    assert_eq!(frame.client_views[0].pane_id, panes[0]);
    assert_eq!(frame.focused_pane, Some(panes[0].clone()));
}
