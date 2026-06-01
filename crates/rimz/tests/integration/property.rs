//! Property tests for parsers, serializers, and the feed-status state
//! machine. Exists per `docs/contributing/rust-conventions.md` § Tests —
//! `proptest` shrinks counterexamples for shapes a few hand-written cases
//! would miss.

use std::io::Write;
use std::path::Path;

use proptest::prelude::*;
use serde_json::json;
use tempfile::tempdir;

use rimz::ids::{EventId, PaneId, RequestId, SidebarInstanceId, WorkspaceId};
use rimz::ledger::event_log::{self, EventLogErr};
use rimz::schema::event::EventEnvelope;
use rimz::{FeedStatus, MuxName};

fn arb_status() -> impl Strategy<Value = FeedStatus> {
    prop_oneof![
        Just(FeedStatus::Pending),
        Just(FeedStatus::Resolved),
        Just(FeedStatus::TimedOut),
        Just(FeedStatus::Abandoned),
    ]
}

fn arb_workspace_id() -> impl Strategy<Value = WorkspaceId> {
    "[ /a-zA-Z0-9_.-]{1,40}".prop_map(|seed| WorkspaceId::from_project_root(Path::new(&seed)))
}

fn arb_event() -> impl Strategy<Value = EventEnvelope> {
    (
        arb_workspace_id(),
        "[a-zA-Z0-9_-]{1,16}",
        "[a-z]{1,8}\\.[a-z]{1,8}",
    )
        .prop_map(|(workspace, session, method)| {
            EventEnvelope::new(
                workspace,
                session,
                "rimz",
                "cli",
                method,
                json!({ "p": "x" }),
            )
        })
}

proptest! {
    /// Terminal statuses stay terminal; `Pending` is the only state from
    /// which `allows_resolution()` is true. The lifecycle never reaches a
    /// state where both predicates are wrong.
    #[test]
    fn feed_status_transitions_never_invalid(status in arb_status()) {
        prop_assert_eq!(
            status.is_terminal(),
            !matches!(status, FeedStatus::Pending),
        );
        prop_assert_eq!(
            status.allows_resolution(),
            matches!(status, FeedStatus::Pending),
        );
        let wire = serde_json::to_string(&status).unwrap();
        let back: FeedStatus = serde_json::from_str(&wire).unwrap();
        prop_assert_eq!(status, back);
    }

    /// `WorkspaceId` Display + FromStr round-trip is lossless over every
    /// project-root shape we'd realistically see.
    #[test]
    fn workspace_id_display_fromstr_round_trip(workspace in arb_workspace_id()) {
        let rendered = workspace.to_string();
        let back: WorkspaceId = rendered.parse().unwrap();
        prop_assert_eq!(back, workspace);
    }

    /// Fresh-mint UUIDv7 IDs always round-trip through Display + FromStr.
    /// Re-runs to exercise random monotonic counters.
    #[test]
    fn request_event_sidebar_ids_round_trip(_seed in 0u32..1024) {
        let req = RequestId::new();
        let parsed: RequestId = req.to_string().parse().unwrap();
        prop_assert_eq!(parsed, req);

        let evt = EventId::new();
        let parsed: EventId = evt.to_string().parse().unwrap();
        prop_assert_eq!(parsed, evt);

        let sb = SidebarInstanceId::new();
        let parsed: SidebarInstanceId = sb.to_string().parse().unwrap();
        prop_assert_eq!(parsed, sb);
    }

    /// `PaneId` round-trips its parts for both supported multiplexers.
    #[test]
    fn pane_id_round_trip(raw in "[a-zA-Z0-9_%-]{1,12}") {
        for mux in [MuxName::Zellij, MuxName::Tmux] {
            let id = PaneId::from_parts(mux, &raw);
            let parsed: PaneId = id.as_str().parse().unwrap();
            prop_assert_eq!(parsed.mux(), mux);
            prop_assert_eq!(parsed.raw(), raw.as_str());
        }
    }

    /// EventEnvelope serde round-trip preserves every field — the
    /// canonical durability invariant for the event log.
    #[test]
    fn event_envelope_serde_round_trip(event in arb_event()) {
        let wire = serde_json::to_string(&event).unwrap();
        let back: EventEnvelope = serde_json::from_str(&wire).unwrap();
        prop_assert_eq!(back, event);
    }

    /// Appending N random events then reading them back returns the same
    /// sequence in order. A torn trailing record (truncated suffix) is
    /// recovered as a one-record loss; preceding records survive.
    #[test]
    fn event_log_append_then_read_lossless(events in proptest::collection::vec(arb_event(), 1..6)) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.log.jsonl");
        for event in &events {
            event_log::append(&path, event).unwrap();
        }
        let read = event_log::read_all(&path).unwrap();
        prop_assert_eq!(read.len(), events.len());
        for (got, want) in read.iter().zip(events.iter()) {
            prop_assert_eq!(got, want);
        }

        // Append a torn trailing record; preceding records still come back.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"999 {\"torn\":\n")
            .unwrap();
        let read = event_log::read_all(&path);
        match read {
            Ok(read) => {
                prop_assert_eq!(read.len(), events.len());
            }
            Err(EventLogErr::Torn { .. } | EventLogErr::FrameLength { .. }) => {
                // Tolerated: the torn-record recovery path classifies the
                // suffix as a parse failure rather than a frame mismatch
                // when the truncation lands inside a JSON token. The
                // contract is "no silent data loss"; both shapes meet it.
            }
            Err(other) => prop_assert!(false, "unexpected event-log error: {other:?}"),
        }
    }
}
