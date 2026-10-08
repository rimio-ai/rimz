//! User-global history for system-initiated assistance.
//!
//! User-benefiting automation appends one best-effort JSONL record after its
//! intervention. Readers fold the current file and its single rotated
//! predecessor for the stats dashboard and forensic timeline.

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::disk::paths::logs_dir;
use crate::harness::auto_redeem::RedeemReason;
use crate::ids::{AgentKind, AgentSessionId};

const NAME: &str = "assists.log.jsonl";
const MAX_BYTES: u64 = 4 * 1_048_576;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AssistRecord {
    pub at: Timestamp,
    #[serde(flatten)]
    pub assist: Assist,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "assist")]
pub enum Assist {
    CheckDecline {
        task: String,
        checkout: PathBuf,
        profile: String,
        kind: AgentKind,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
    },
    StallNotice {
        kind: AgentKind,
        agent_id: AgentSessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        parent: String,
        silent_secs: u64,
        message_id: String,
        delivered: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    ResidentLaunch {
        task: String,
        checkout: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        condition: Option<super::schedule::when::ConditionEvidence>,
        /// Handles of the checkout occupants the launch took over from.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        stopped: Vec<String>,
        handles: Vec<String>,
    },
    ModelAlias {
        kind: AgentKind,
        login: crate::ids::LoginKey,
        alias: String,
        from: String,
        to: String,
    },
    TierFallback {
        kind: AgentKind,
        agent_id: AgentSessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        profile: String,
        tier: crate::config::tiers::ModelTier,
        model: String,
        skipped: Vec<crate::agents::TierSkip>,
    },
    AutoRedeem {
        kind: String,
        /// The account the credit was redeemed on; records older than the
        /// field carry none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        login: Option<crate::ids::LoginName>,
        reason: RedeemReason,
        request_id: String,
        credits: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        soonest_expiry: Option<Timestamp>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        natural_reset: Option<Timestamp>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome: Option<String>,
        windows_reset: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        window_resets: Vec<AssistWindowReset>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    AutoContinue {
        kind: AgentKind,
        agent_id: AgentSessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        park: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parked_since: Option<Timestamp>,
        delivered: bool,
        message_id: String,
    },
    AutoCompact {
        kind: AgentKind,
        agent_id: AgentSessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        threshold: crate::store::message::AutoCompact,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        occupied_tokens: Option<u64>,
        message_id: String,
    },
    CacheKeepalive {
        kind: AgentKind,
        agent_id: AgentSessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        idle_secs: u64,
        waits: usize,
        /// The keep-warm horizon that held the agent for this ping; `None` when only a pending wait qualified it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        horizon_secs: Option<u64>,
        message_id: String,
        delivered: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        /// This ping told the agent the keepalive maximum was reached.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        capped: bool,
    },
    IdleCompact {
        kind: AgentKind,
        agent_id: AgentSessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        idle_secs: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        idle_after_secs: Option<u64>,
        occupied_tokens: u64,
        message_id: String,
        delivered: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// A `stop --when-idle` request the helper acted on, whether or not the
    /// stop went through.
    IdleStop {
        kind: AgentKind,
        agent_id: AgentSessionId,
        label: String,
        idle_secs: u64,
        idle_after_secs: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        requested_by: Option<String>,
        stopped: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// A fresh launch whose provider exited before opening a session, and one
    /// relaunch the exec wrapper answered it with.
    LaunchRetry {
        kind: AgentKind,
        label: String,
        /// The run every attempt shares; absent for a launch without one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<crate::ids::RunId>,
        /// Which relaunch of the launch this is, from 1; lines written before
        /// the cap carry none and were the only one.
        #[serde(default = "first_attempt")]
        attempt: u8,
        /// The exited process's exit code; absent when a signal killed it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        /// Spawn to exit of the exited process.
        startup_ms: u64,
        relaunched: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    FlipCompact {
        kind: AgentKind,
        agent_id: AgentSessionId,
        role: String,
        threshold: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
        to: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        occupied_tokens: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
        delivered: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    AutoResume {
        workspace_id: crate::ids::WorkspaceId,
        session_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<crate::store::event::SessionDeathCause>,
        recovered: usize,
        labels: Vec<String>,
    },
    AutoGc {
        workspace_id: crate::ids::WorkspaceId,
        #[serde(default)]
        scope: crate::harness::auto_gc::GcScope,
        older_than_secs: u64,
        reclaimed_bytes: u64,
        #[serde(default)]
        class_bytes: std::collections::BTreeMap<String, u64>,
        worktrees_removed: usize,
        workspaces_pruned: usize,
        files_removed: usize,
        messages_archived: usize,
        problems: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistWindowReset {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_mins: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<Timestamp>,
}

pub fn log_path(state_root: &Path) -> PathBuf {
    state_root.join(NAME)
}

pub fn append(record: &AssistRecord) {
    append_to(&logs_dir(), record, MAX_BYTES);
}

pub fn try_append(record: &AssistRecord) -> std::io::Result<()> {
    crate::disk::rotating::append_rotating_jsonl(&log_path(&logs_dir()), MAX_BYTES, record)
}

pub fn record_tier_fallbacks(identities: &[crate::store::writer::AgentLaunchIdentity]) {
    for identity in identities {
        record_tier_fallback(
            &identity.kind,
            &identity.agent_id,
            Some(&identity.name),
            &identity.launch,
        );
    }
}

pub fn record_tier_fallback(
    kind: &AgentKind,
    agent_id: &AgentSessionId,
    label: Option<&str>,
    launch: &crate::agents::LaunchParams,
) {
    let Some(stamp) = launch
        .tier
        .as_ref()
        .filter(|stamp| !stamp.skipped.is_empty())
    else {
        return;
    };
    append(&AssistRecord {
        at: Timestamp::now(),
        assist: Assist::TierFallback {
            kind: kind.clone(),
            agent_id: agent_id.clone(),
            label: label.map(str::to_owned),
            // Tier stamps originate only in named, materialized profile cells.
            profile: launch.profile.clone().expect("routed launch profile"),
            tier: stamp.tier,
            model: stamp.model.clone(),
            skipped: stamp.skipped.clone(),
        },
    });
}

pub fn recent(state_root: &Path, since: Option<Timestamp>) -> Vec<AssistRecord> {
    let mut records = Vec::new();
    crate::disk::rotating::visit_records(&log_path(state_root), |record: AssistRecord| {
        if since.is_none_or(|since| record.at >= since) {
            records.push(record);
        }
    });
    records.sort_by_key(|record| record.at);
    records
}

const fn first_attempt() -> u8 {
    1
}

fn append_to(state_root: &Path, record: &AssistRecord, max_bytes: u64) {
    crate::disk::rotating::append(&log_path(state_root), max_bytes, record);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(second: i64) -> Timestamp {
        Timestamp::from_second(second).expect("timestamp")
    }

    fn redeem(at: i64, request_id: impl Into<String>) -> AssistRecord {
        AssistRecord {
            at: ts(at),
            assist: Assist::AutoRedeem {
                kind: "codex".to_owned(),
                login: Some("work".parse().expect("login name")),
                reason: RedeemReason::ExpiryRescue,
                request_id: request_id.into(),
                credits: 2,
                soonest_expiry: Some(ts(30)),
                natural_reset: Some(ts(40)),
                outcome: Some("reset".to_owned()),
                windows_reset: true,
                window_resets: vec![AssistWindowReset {
                    duration_mins: Some(300),
                    resets_at: Some(ts(50)),
                }],
                error: None,
            },
        }
    }

    fn resumed(at: i64) -> AssistRecord {
        AssistRecord {
            at: ts(at),
            assist: Assist::AutoContinue {
                kind: AgentKind::new_unchecked("codex"),
                agent_id: AgentSessionId::from("session-1"),
                label: Some("@coder".to_owned()),
                park: "rate_limit_window_reset".to_owned(),
                parked_since: Some(ts(10)),
                delivered: true,
                message_id: "msg_1".to_owned(),
            },
        }
    }

    fn compacted(at: i64) -> AssistRecord {
        AssistRecord {
            at: ts(at),
            assist: Assist::AutoCompact {
                kind: AgentKind::new_unchecked("codex"),
                agent_id: AgentSessionId::from("session-1"),
                label: Some("@coder".to_owned()),
                threshold: crate::store::message::AutoCompact::Percent(70),
                occupied_tokens: Some(210_000),
                message_id: "msg_2".to_owned(),
            },
        }
    }

    fn restored(at: i64) -> AssistRecord {
        AssistRecord {
            at: ts(at),
            assist: Assist::AutoResume {
                workspace_id: crate::ids::WorkspaceId::parse("ws_0123456789abcdef01234567")
                    .expect("workspace"),
                session_name: "rimz-test".to_owned(),
                cause: Some(crate::store::event::SessionDeathCause::Crash),
                recovered: 2,
                labels: vec!["@coder".to_owned(), "@reviewer".to_owned()],
            },
        }
    }

    fn idle_compacted(at: i64) -> AssistRecord {
        AssistRecord {
            at: ts(at),
            assist: Assist::IdleCompact {
                kind: AgentKind::new_unchecked("claude"),
                agent_id: AgentSessionId::from("session-2"),
                label: Some("@planner".to_owned()),
                idle_secs: 3_540,
                idle_after_secs: None,
                occupied_tokens: 180_000,
                message_id: "msg_3".to_owned(),
                delivered: true,
                error: None,
            },
        }
    }

    #[test]
    fn flip_compaction_round_trips_known_and_missing_attempt_evidence() {
        for delivered in [true, false] {
            let record = AssistRecord {
                at: ts(20),
                assist: Assist::FlipCompact {
                    kind: AgentKind::new_unchecked("codex"),
                    agent_id: AgentSessionId::from("session-1"),
                    role: "coder".to_owned(),
                    threshold: 180_000,
                    from: delivered.then(|| "Implement".to_owned()),
                    to: "Review".to_owned(),
                    occupied_tokens: delivered.then_some(204_000),
                    message_id: delivered.then(|| "msg_4".to_owned()),
                    delivered,
                    error: (!delivered).then(|| "no bound pane".to_owned()),
                },
            };
            let json = serde_json::to_value(&record).expect("serialize");
            assert_eq!(json["assist"], "flip_compact");
            assert_eq!(json["role"], "coder");
            assert_eq!(json["threshold"], 180_000);
            assert_eq!(
                serde_json::from_value::<AssistRecord>(json).expect("deserialize"),
                record
            );
        }
    }

    #[test]
    fn variants_round_trip_through_the_wire_shape() {
        let stalled = serde_json::json!({
            "at": "2026-06-02T12:00:00Z", "assist": "stall_notice",
            "kind": "codex", "agent_id": "session-1", "label": "@still-silver",
            "parent": "@planner", "silent_secs": 1920, "message_id": "msg_1",
            "delivered": false
        });
        let decoded = serde_json::from_value::<AssistRecord>(stalled.clone());
        assert!(decoded.is_ok(), "stall notice is an assist: {decoded:?}");
        assert_eq!(serde_json::to_value(decoded.unwrap()).unwrap(), stalled);
        let fallback = serde_json::json!({
            "at": "2026-06-02T12:00:00Z", "assist": "tier_fallback",
            "kind": "codex", "agent_id": "session-1", "profile": "worker",
            "tier": "senior", "model": "gpt-6-astra",
            "skipped": [{"model": "opus", "reason": "daily_cap", "spend_usd": 10, "cap_usd": 5}]
        });
        let decoded = serde_json::from_value::<AssistRecord>(fallback.clone());
        assert!(decoded.is_ok(), "tier fallback is an assist: {decoded:?}");
        assert_eq!(serde_json::to_value(decoded.unwrap()).unwrap(), fallback);
        for record in [
            AssistRecord {
                at: ts(20),
                assist: Assist::ModelAlias {
                    kind: AgentKind::new_unchecked("codex"),
                    login: "codex@default".parse().unwrap(),
                    alias: "sol".into(),
                    from: "gpt-6-sol".into(),
                    to: "gpt-6.1-sol".into(),
                },
            },
            redeem(20, "request-1"),
            resumed(20),
            compacted(20),
            idle_compacted(20),
            AssistRecord {
                at: ts(20),
                assist: Assist::CacheKeepalive {
                    kind: AgentKind::new_unchecked("claude"),
                    agent_id: "session-1".into(),
                    label: Some("@coder".into()),
                    idle_secs: 3540,
                    waits: 2,
                    horizon_secs: Some(7200),
                    message_id: "msg_1".into(),
                    delivered: true,
                    error: None,
                    capped: true,
                },
            },
            restored(20),
            AssistRecord {
                at: ts(20),
                assist: Assist::ResidentLaunch {
                    task: "sweep".into(),
                    checkout: "/repo-worktrees/auth".into(),
                    condition: None,
                    stopped: vec!["@coder#auth".into()],
                    handles: vec!["@sweep".into()],
                },
            },
            AssistRecord {
                at: ts(20),
                assist: Assist::IdleStop {
                    kind: AgentKind::new_unchecked("claude"),
                    agent_id: "session-1".into(),
                    label: "@coder".into(),
                    idle_secs: 200,
                    idle_after_secs: 180,
                    requested_by: Some("@lead".into()),
                    stopped: false,
                    error: Some("agent coder has no bound pane".into()),
                },
            },
            AssistRecord {
                at: ts(20),
                assist: Assist::LaunchRetry {
                    kind: AgentKind::new_unchecked("codex"),
                    label: "@otter".into(),
                    run_id: Some(
                        crate::ids::RunId::parse("run_0123456789abcdef0123456789abcdef")
                            .expect("run id"),
                    ),
                    attempt: 2,
                    exit_code: None,
                    startup_ms: 17_250,
                    relaunched: false,
                    error: Some("No such file or directory".into()),
                },
            },
        ] {
            let json = serde_json::to_string(&record).expect("serialize");
            let decoded: AssistRecord = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(decoded, record);
        }
        let retried = serde_json::json!({
            "at": "2026-06-02T12:00:00Z", "assist": "launch_retry", "kind": "codex",
            "label": "@otter", "run_id": "run_0123456789abcdef0123456789abcdef",
            "exit_code": 1, "startup_ms": 17250, "relaunched": true
        });
        let decoded = serde_json::from_value::<AssistRecord>(retried).expect("pre-cap line");
        assert!(
            matches!(
                decoded.assist,
                Assist::LaunchRetry {
                    run_id: Some(_),
                    attempt: 1,
                    relaunched: true,
                    ..
                }
            ),
            "{decoded:?}"
        );
        let uncapped = serde_json::json!({
            "at": "2026-06-02T12:00:00Z", "assist": "cache_keepalive", "kind": "claude",
            "agent_id": "session-1", "idle_secs": 3540, "waits": 1, "message_id": "msg_1",
            "delivered": true
        });
        let decoded =
            serde_json::from_value::<AssistRecord>(uncapped.clone()).expect("pre-cap line");
        assert!(
            matches!(decoded.assist, Assist::CacheKeepalive { capped: false, .. }),
            "{decoded:?}"
        );
        assert_eq!(serde_json::to_value(decoded).unwrap(), uncapped);
        let rootless = serde_json::json!({
            "at": "2026-06-02T12:00:00Z", "assist": "launch_retry", "kind": "codex",
            "label": "@otter", "attempt": 3, "exit_code": 1, "startup_ms": 900,
            "relaunched": true
        });
        let decoded: AssistRecord = serde_json::from_value(rootless.clone()).expect("no run");
        assert_eq!(serde_json::to_value(decoded).unwrap(), rootless);
        let stopped = serde_json::json!({
            "at": "2026-06-02T12:00:00Z", "assist": "idle_stop", "kind": "claude",
            "agent_id": "session-1", "label": "@coder", "idle_secs": 200,
            "idle_after_secs": 180, "stopped": true
        });
        let decoded: AssistRecord = serde_json::from_value(stopped.clone()).expect("idle stop");
        assert_eq!(serde_json::to_value(decoded).unwrap(), stopped);
    }

    #[test]
    fn a_redeem_record_names_its_account_and_reads_without_one() {
        let mut json = serde_json::to_value(redeem(20, "request-1")).expect("serialize");
        assert_eq!(json["login"], "work");
        json.as_object_mut().expect("object").remove("login");
        let older: AssistRecord = serde_json::from_value(json).expect("deserialize");
        assert!(matches!(
            older.assist,
            Assist::AutoRedeem { login: None, .. }
        ));
    }

    #[test]
    fn append_rotates_and_reader_folds_both_generations() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = redeem(10, "x".repeat(256));
        append_to(dir.path(), &first, 1);
        let second = resumed(20);
        append_to(dir.path(), &second, 1);

        assert!(dir.path().join("assists.log.jsonl").is_file());
        assert_eq!(recent(dir.path(), None), vec![first, second]);
    }

    #[test]
    fn recent_filters_by_inclusive_timestamp_and_skips_bad_lines() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = log_path(dir.path());
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(
            &path,
            format!(
                "{}\nnot-json\n{}\n{}\n",
                serde_json::to_string(&redeem(10, "old")).expect("old"),
                r#"{"at":"1970-01-01T00:00:15Z","assist":"focus_repair","workspace_id":"ws_0123456789abcdef01234567","session_name":"rimz-test","generation":1,"evidence":[],"target":"zellij:terminal_2","outcome":"confirmed"}"#,
                serde_json::to_string(&resumed(20)).expect("new")
            ),
        )
        .expect("write log");

        assert_eq!(recent(dir.path(), Some(ts(20))), vec![resumed(20)]);
    }
}
