//! Edge-triggered evidence of published focus and renderer selection decisions.

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::ids::{PaneId, SidebarInstanceId, WorkspaceId};

pub(crate) fn log_path(state_root: &std::path::Path) -> std::path::PathBuf {
    crate::StatePaths::class_path(
        state_root,
        crate::disk::paths::Class::Audit,
        "focus.log.jsonl",
    )
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FocusOrigin {
    SessionFocus,
    ClientView,
    Prior,
    #[default]
    None,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum FocusTraceEvent {
    FramePublished {
        produced_at_ms: u64,
        observed_at_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        focused_pane: Option<PaneId>,
        focus_origin: FocusOrigin,
        client_view_fresh: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        client_sample_withheld: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        own_pane_listed: Option<bool>,
        pane_count: usize,
        publication: String,
    },
    FoldDecided {
        source: String,
        phase: String,
        seed: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        panes_produced_at_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        panes_observed_at_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        snapshot_focused_pane: Option<PaneId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fused_event_sent_at_ms: Option<u64>,
        own_view: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        baseline: Option<PaneId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selected_before: Option<PaneId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selected_after: Option<PaneId>,
    },
}

#[derive(Serialize)]
struct FocusTraceEnvelope {
    v: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    build: Option<&'static str>,
    workspace_id: WorkspaceId,
    session_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    instance_id: Option<SidebarInstanceId>,
    at_ms: u64,
    event: FocusTraceEvent,
}

pub(super) fn append(
    state_root: &Path,
    workspace_id: WorkspaceId,
    session_name: String,
    instance_id: Option<SidebarInstanceId>,
    at_ms: u64,
    event: FocusTraceEvent,
) {
    crate::disk::rotating::append(
        &log_path(state_root),
        crate::disk::retention::ROTATING_LOG_MAX_BYTES,
        &FocusTraceEnvelope {
            v: "rimz.focus_trace.v1",
            build: crate::build_id::current(),
            workspace_id,
            session_name,
            instance_id,
            at_ms,
            event,
        },
    );
}

type PublicationEdge = (
    Option<PaneId>,
    FocusOrigin,
    bool,
    bool,
    Option<bool>,
    HashSet<PaneId>,
    String,
);
type FoldEdge = (Option<PaneId>, bool, Option<PaneId>, Option<PaneId>);

#[derive(Debug, Default)]
pub(super) struct Edges {
    publication: Option<PublicationEdge>,
    fold: Option<FoldEdge>,
}

impl Edges {
    pub(super) fn admit(&mut self, event: &FocusTraceEvent, pane_ids: HashSet<PaneId>) -> bool {
        match event {
            FocusTraceEvent::FramePublished {
                focused_pane,
                focus_origin,
                client_view_fresh,
                client_sample_withheld,
                own_pane_listed,
                publication,
                ..
            } => {
                let edge = (
                    focused_pane.clone(),
                    *focus_origin,
                    *client_view_fresh,
                    *client_sample_withheld,
                    *own_pane_listed,
                    pane_ids,
                    publication.clone(),
                );
                if self.publication.as_ref() == Some(&edge) {
                    return false;
                }
                self.publication = Some(edge);
            }
            FocusTraceEvent::FoldDecided {
                snapshot_focused_pane,
                own_view,
                baseline,
                selected_after,
                ..
            } => {
                let edge = (
                    snapshot_focused_pane.clone(),
                    *own_view,
                    baseline.clone(),
                    selected_after.clone(),
                );
                if self.fold.as_ref() == Some(&edge) {
                    return false;
                }
                self.fold = Some(edge);
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::DiagSink;
    use crate::ids::{MuxName, SidebarInstanceId, WorkspaceId};
    use std::collections::HashSet;
    use std::path::Path;

    fn pane(raw: &str) -> PaneId {
        PaneId::from_parts(MuxName::Tmux, raw)
    }

    fn publication(produced_at_ms: u64) -> FocusTraceEvent {
        FocusTraceEvent::FramePublished {
            produced_at_ms,
            observed_at_ms: produced_at_ms - 1,
            focused_pane: Some(pane("%1")),
            focus_origin: FocusOrigin::ClientView,
            client_view_fresh: true,
            client_sample_withheld: false,
            own_pane_listed: None,
            pane_count: 2,
            publication: "topology".to_owned(),
        }
    }

    fn fold() -> FocusTraceEvent {
        FocusTraceEvent::FoldDecided {
            source: "published".to_owned(),
            phase: "interim".to_owned(),
            seed: true,
            panes_produced_at_ms: Some(10),
            panes_observed_at_ms: Some(9),
            snapshot_focused_pane: Some(pane("%1")),
            fused_event_sent_at_ms: None,
            own_view: false,
            baseline: None,
            selected_before: None,
            selected_after: None,
        }
    }

    #[test]
    fn withheld_publication_keeps_wire_field() {
        let mut json = serde_json::to_value(publication(10)).unwrap();
        json["client_sample_withheld"] = true.into();
        let event: FocusTraceEvent = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(serde_json::to_value(event).unwrap(), json);
    }

    #[test]
    fn withheld_publication_is_a_new_edge() {
        let mut edges = Edges::default();
        assert!(edges.admit(&publication(10), HashSet::new()));
        let mut json = serde_json::to_value(publication(10)).unwrap();
        json["client_sample_withheld"] = true.into();
        let event: FocusTraceEvent = serde_json::from_value(json).unwrap();
        assert!(edges.admit(&event, HashSet::new()));
        assert!(!edges.admit(&event, HashSet::new()));
        assert!(edges.admit(&publication(11), HashSet::new()));
    }

    #[test]
    fn trace_events_keep_wire_shape() {
        let rows = [
            (
                publication(10),
                serde_json::json!({
                    "kind": "frame_published", "produced_at_ms": 10,
                    "observed_at_ms": 9, "focused_pane": "tmux:%1",
                    "focus_origin": "client_view", "client_view_fresh": true,
                    "pane_count": 2, "publication": "topology",
                }),
            ),
            (
                fold(),
                serde_json::json!({
                    "kind": "fold_decided", "source": "published", "phase": "interim",
                    "seed": true, "panes_produced_at_ms": 10, "panes_observed_at_ms": 9,
                    "snapshot_focused_pane": "tmux:%1", "own_view": false,
                }),
            ),
            (
                FocusTraceEvent::FoldDecided {
                    source: "produced".to_owned(),
                    phase: "final".to_owned(),
                    seed: false,
                    panes_produced_at_ms: None,
                    panes_observed_at_ms: None,
                    snapshot_focused_pane: None,
                    fused_event_sent_at_ms: Some(12),
                    own_view: true,
                    baseline: Some(pane("%2")),
                    selected_before: Some(pane("%1")),
                    selected_after: Some(pane("%2")),
                },
                serde_json::json!({
                    "kind": "fold_decided", "source": "produced", "phase": "final",
                    "seed": false, "fused_event_sent_at_ms": 12, "own_view": true,
                    "baseline": "tmux:%2", "selected_before": "tmux:%1", "selected_after": "tmux:%2",
                }),
            ),
        ];
        for (event, expected) in rows {
            let json = serde_json::to_value(&event).unwrap();
            assert_eq!(json, expected);
            assert_eq!(
                serde_json::from_value::<FocusTraceEvent>(json).unwrap(),
                event
            );
        }
        for (origin, expected) in [
            (FocusOrigin::SessionFocus, "session_focus"),
            (FocusOrigin::ClientView, "client_view"),
            (FocusOrigin::Prior, "prior"),
            (FocusOrigin::None, "none"),
        ] {
            assert_eq!(serde_json::to_value(origin).unwrap(), expected);
        }
    }

    fn records(root: &Path) -> Vec<serde_json::Value> {
        let path = log_path(root);
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn trace_focus_keeps_identity_and_edges_without_rate_limiting() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(Path::new("/focus-trace"));
        let instance = SidebarInstanceId::new();
        let sink = DiagSink::under(
            dir.path().to_owned(),
            workspace.clone(),
            "s",
            Some(instance.clone()),
        );
        let roster = HashSet::from([pane("%1"), pane("%2")]);
        sink.trace_focus_at_ms(publication(10), roster.clone(), 1_000);
        sink.trace_focus_at_ms(publication(20), roster.clone(), 1_001);
        sink.trace_focus_at_ms(fold(), HashSet::new(), 1_002);
        let mut repeated = fold();
        if let FocusTraceEvent::FoldDecided {
            source,
            phase,
            seed,
            panes_produced_at_ms,
            ..
        } = &mut repeated
        {
            *source = "produced".to_owned();
            *phase = "final".to_owned();
            *seed = false;
            *panes_produced_at_ms = Some(20);
        }
        sink.trace_focus_at_ms(repeated, HashSet::new(), 1_003);
        let rows = records(dir.path());
        assert_eq!(
            rows.len(),
            2,
            "identical publication and fold edges append once each"
        );
        for (row, at_ms) in rows.iter().zip([1_000, 1_002]) {
            assert_eq!(row["v"], "rimz.focus_trace.v1");
            assert_eq!(row["build"], crate::build_id::current().unwrap());
            assert_eq!(
                row["workspace_id"],
                serde_json::to_value(&workspace).unwrap()
            );
            assert_eq!(row["session_name"], "s");
            assert_eq!(row["instance_id"], serde_json::to_value(&instance).unwrap());
            assert_eq!(row["at_ms"], at_ms);
        }
        // Same count and focus, different pane identity: this is a roster edge.
        sink.trace_focus_at_ms(
            publication(30),
            HashSet::from([pane("%1"), pane("%3")]),
            1_004,
        );
        let mut viewed = fold();
        if let FocusTraceEvent::FoldDecided { own_view, .. } = &mut viewed {
            *own_view = true;
        }
        sink.trace_focus_at_ms(viewed, HashSet::new(), 1_005);
        let rows = records(dir.path());
        assert_eq!(
            rows.len(),
            4,
            "roster and own-view edges cannot be hidden by unchanged focus"
        );
        assert_eq!(rows[2]["event"]["produced_at_ms"], 30);
        assert_eq!(rows[3]["event"]["own_view"], true);
        let correlated = rows
            .iter()
            .filter(|row| row["event"]["kind"] == "frame_published")
            .filter(|row| row["event"]["produced_at_ms"].as_u64().unwrap() <= 20)
            .max_by_key(|row| row["event"]["produced_at_ms"].as_u64().unwrap())
            .unwrap();
        assert_eq!(
            correlated["event"]["produced_at_ms"], 10,
            "unchanged republications correlate by floor"
        );
        for index in 0..130 {
            let mut event = publication(40 + index);
            if let FocusTraceEvent::FramePublished {
                client_view_fresh, ..
            } = &mut event
            {
                *client_view_fresh = index % 2 != 0;
            }
            sink.trace_focus_at_ms(event, roster.clone(), 2_000 + index);
        }
        assert_eq!(
            records(dir.path()).len(),
            134,
            "no main-log kind ceiling applies"
        );
        let other = sink.for_renderer(SidebarInstanceId::new());
        other.trace_focus_at_ms(fold(), HashSet::new(), 3_000);
        assert_eq!(
            records(dir.path()).len(),
            135,
            "attachments have separate edge histories"
        );
    }
}
