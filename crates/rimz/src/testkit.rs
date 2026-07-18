//! Test and benchmark fixtures for external targets.
//!
//! This module is behind the `testkit` feature so synthetic fleet builders stay
//! out of the shipped binary while integration tests and benches can share the
//! same event and pane shapes.

pub use crate::proc::testkit::spawn_count;
pub use crate::store::atomic::testkit::fsync_count;
pub use crate::store::event_log::testkit::{bytes_read, bytes_written};

/// Benchmark one workspace derivation from an already-warm spending walker.
pub fn spending_scope_from_warm_walker(
    walker: &mut crate::agents::spending::SpendingWalker,
    cache_path: &std::path::Path,
    files: &[(&'static dyn crate::agents::AgentAdapter, std::path::PathBuf)],
    scope: &crate::agents::spending::SpendScope,
    now_secs: u64,
    spec: &crate::agents::spending::HeadlineSpec,
) -> crate::agents::spending::ScopedSpending {
    walker
        .scoped_from_cache(cache_path, files, &[], scope, now_secs, spec)
        .scoped
}

/// Minimal idle [`crate::agents::AgentState`] for fixtures: identity + clocks, everything else absent.
pub fn agent_state(kind: &str, agent_id: &str, at: jiff::Timestamp) -> crate::agents::AgentState {
    crate::agents::AgentState::seed(
        crate::ids::AgentKind::new_unchecked(kind),
        crate::ids::AgentSessionId::from(agent_id),
        crate::agents::AgentStatus::Idle,
        at,
    )
}

/// Provider-local changed-session fixture used only by allocation/time benches.
pub struct ChangedSessionRefreshFixture {
    adapter: &'static dyn crate::agents::AgentAdapter,
    session_id: String,
    transcript: std::path::PathBuf,
    pricing: std::path::PathBuf,
}

impl ChangedSessionRefreshFixture {
    pub fn refresh(&self) -> Option<crate::agents::LocalContextRefresh> {
        let transcript = self.transcript.to_str()?;
        let ctx = crate::agents::LocalContextRefreshCtx {
            agent_id: &self.session_id,
            model_hint: None,
            current_transcript_path: Some(transcript),
            prior_transcript_path: Some(transcript),
            prior_transcript_stat: None,
            prior_spend_fold: None,
            shared_pricing_cache_path: &self.pricing,
        };
        self.adapter
            .local_context_refresh(crate::agents::RefreshTrigger::Watch, &ctx)
    }
}

pub fn changed_kimi_session_fixture(
    root: &std::path::Path,
    records: usize,
) -> ChangedSessionRefreshFixture {
    let transcript = root.join("kimi/sessions/wd/s1/agents/main/wire.jsonl");
    std::fs::create_dir_all(transcript.parent().expect("fixture parent")).expect("fixture dirs");
    let mut body = String::from("{\"type\":\"metadata\",\"protocol_version\":\"1.4\"}\n");
    for index in 0..records {
        body.push_str(&format!(
            "{{\"type\":\"usage.record\",\"time\":{},\"model\":\"moonshot/kimi-k2.5\",\"usageScope\":\"turn\",\"usage\":{{\"inputOther\":1000,\"output\":100}}}}\n",
            1_770_000_000_000_u64 + index as u64
        ));
    }
    std::fs::write(&transcript, body).expect("Kimi fixture");
    ChangedSessionRefreshFixture {
        adapter: crate::agents::find_adapter("kimi").expect("Kimi fixture adapter is registered"),
        session_id: "s1".to_owned(),
        transcript,
        pricing: root.join("prices.json"),
    }
}

pub fn changed_grok_session_fixture(
    root: &std::path::Path,
    records: usize,
) -> ChangedSessionRefreshFixture {
    let transcript = root.join("grok/s1/updates.jsonl");
    std::fs::create_dir_all(transcript.parent().expect("fixture parent")).expect("fixture dirs");
    let mut body = String::new();
    for index in 0..records {
        body.push_str(&serde_json::json!({
            "timestamp": 1_770_000_000_u64 + index as u64,
            "method": "session/update",
            "params": { "update": { "sessionUpdate": "user_message_chunk", "content": { "type": "text", "text": "prompt" }, "_meta": { "promptIndex": index } } }
        }).to_string());
        body.push('\n');
        body.push_str(&serde_json::json!({
            "timestamp": 1_770_000_001_u64 + index as u64,
            "method": "_x.ai/session/update",
            "params": { "sessionId": "s1", "update": { "sessionUpdate": "turn_completed", "prompt_id": format!("p{index}"), "stop_reason": "end_turn", "usage": { "inputTokens": 1000, "cachedReadTokens": 300, "outputTokens": 100, "costUsdTicks": 10_000_000 } } }
        }).to_string());
        body.push('\n');
    }
    std::fs::write(&transcript, body).expect("Grok fixture");
    ChangedSessionRefreshFixture {
        adapter: crate::agents::find_adapter("grok").expect("Grok fixture adapter is registered"),
        session_id: "s1".to_owned(),
        transcript,
        pricing: root.join("prices.json"),
    }
}

pub fn changed_droid_session_fixture(
    root: &std::path::Path,
    records: usize,
) -> ChangedSessionRefreshFixture {
    let transcript = root.join("droid/s1.jsonl");
    std::fs::create_dir_all(transcript.parent().expect("fixture parent")).expect("fixture dirs");
    let mut body = format!(
        "{{\"type\":\"session_start\",\"version\":2,\"cwd\":{}}}\n",
        serde_json::to_string(&root.to_string_lossy()).expect("cwd")
    );
    for index in 0..records {
        body.push_str(&format!(
            "{{\"type\":\"message\",\"id\":\"m{index}\",\"timestamp\":\"2026-01-01T00:00:00Z\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"done\"}}],\"modelId\":\"gpt-5\"}}}}\n"
        ));
    }
    std::fs::write(&transcript, body).expect("Droid fixture");
    std::fs::write(
        transcript.with_file_name("s1.settings.json"),
        format!(
            "{{\"model\":\"gpt-5\",\"tokenUsage\":{{\"inputTokens\":{},\"outputTokens\":{}}}}}",
            records.saturating_mul(1000),
            records.saturating_mul(100)
        ),
    )
    .expect("Droid settings fixture");
    ChangedSessionRefreshFixture {
        adapter: crate::agents::find_adapter("droid").expect("Droid fixture adapter is registered"),
        session_id: "s1".to_owned(),
        transcript,
        pricing: root.join("prices.json"),
    }
}

pub mod fleet {
    use crate::agents::lifecycle::LifecycleSignal;
    use crate::agents::{AgentLifecycleObservation, LaunchParams};
    use crate::ids::{AgentSessionId, MuxName, PaneId, ViewKind, WorkspaceId};
    use crate::pane::PaneRef;
    use crate::sidebar::produce::ProduceOptions;
    use crate::sidebar::refresh::{AccountsCache, ProviderRecord};
    use crate::store::event::EventEnvelope;
    use crate::store::{StatePaths, event_log};
    use crate::{RuntimePaths, agents, sidebar};

    use std::io;

    pub const SESSION_NAME: &str = "rimz-perf";

    /// One registered agent lifecycle event for a synthetic fleet slot.
    pub fn registered_lifecycle(workspace_id: &WorkspaceId, slot: usize) -> EventEnvelope {
        EventEnvelope::agent_lifecycle(
            workspace_id.clone(),
            format!("sess-{slot}"),
            "claude",
            "SessionStart",
            &registered_observation(slot),
        )
    }

    /// Live session panes shaped like `list-panes` output, with no cwd so the
    /// fleet stays off per-worktree git enrichment unless a test adds roots.
    pub fn synthetic_panes(n: usize) -> Vec<PaneRef> {
        (0..n).map(synthetic_pane).collect()
    }

    /// Append `history_events` lifecycle frames spread across `fleet` slots.
    pub fn seed_fleet_store(
        paths: &StatePaths,
        fleet: usize,
        history_events: usize,
    ) -> event_log::Result<()> {
        if fleet == 0 {
            return Ok(());
        }
        for i in 0..history_events {
            event_log::append(
                &paths.events_log,
                &registered_lifecycle(&paths.workspace_id, i % fleet),
            )?;
        }
        Ok(())
    }

    /// Append lifecycle frames bound to synthetic panes with real worktree paths.
    pub fn seed_fleet_store_with_panes(
        paths: &StatePaths,
        panes: &[PaneRef],
        history_events: usize,
    ) -> event_log::Result<()> {
        if panes.is_empty() {
            return Ok(());
        }
        for i in 0..history_events {
            let slot = i % panes.len();
            event_log::append(
                &paths.events_log,
                &registered_lifecycle_for_pane(&paths.workspace_id, slot, &panes[slot]),
            )?;
        }
        Ok(())
    }

    /// Publish fresh pane, spending, and account sidecars for warm produce.
    pub fn publish_fresh_produce_inputs(runtime: &RuntimePaths, fleet: usize) -> io::Result<()> {
        publish_fresh_produce_inputs_for_panes(runtime, synthetic_panes(fleet))
    }

    /// Publish fresh pane, spending, and account sidecars for custom pane shapes.
    pub fn publish_fresh_produce_inputs_for_panes(
        runtime: &RuntimePaths,
        panes: Vec<PaneRef>,
    ) -> io::Result<()> {
        let now_ms = sidebar::timing::unix_now_ms();
        let frame = sidebar::frame::assemble_frame(panes, now_ms, SESSION_NAME);
        sidebar::produce::publish_test_pane_frame(runtime, &frame).map_err(io::Error::other)?;

        if !agents::spending::write_provider_spending_cache(
            &runtime.shared_provider_spending_path(),
            now_ms,
            &agents::spending::Spending::default(),
        ) {
            return Err(io::Error::other("provider spending cache write failed"));
        }

        let accounts = AccountsCache {
            providers: agents::known_kinds()
                .map(|kind| {
                    (
                        kind.to_owned(),
                        ProviderRecord {
                            probed_at_ms: now_ms,
                            ok: false,
                            account: None,
                        },
                    )
                })
                .collect(),
        };
        let accounts = serde_json::to_vec(&accounts).map_err(io::Error::other)?;
        std::fs::write(runtime.shared_accounts_path(), accounts)?;
        Ok(())
    }

    /// Zellij-shaped produce options for the synthetic fleet.
    pub fn produce_options() -> ProduceOptions {
        ProduceOptions {
            mux: MuxName::Zellij,
            session_name: SESSION_NAME.to_owned(),
            exclude: None,
            min_pane_cache_ms: None,
            diag: crate::diag::DiagSink::disabled(),
        }
    }

    fn registered_lifecycle_for_pane(
        workspace_id: &WorkspaceId,
        slot: usize,
        pane: &PaneRef,
    ) -> EventEnvelope {
        let mut observation = registered_observation(slot);
        observation.worktree_path = pane.cwd.clone();
        observation.pane_id = Some(pane.pane_id.clone());
        EventEnvelope::agent_lifecycle(
            workspace_id.clone(),
            format!("sess-{slot}"),
            "claude",
            "SessionStart",
            &observation,
        )
    }

    fn registered_observation(slot: usize) -> AgentLifecycleObservation {
        AgentLifecycleObservation {
            agent_id: Some(AgentSessionId::from(format!("agent-{slot}"))),
            agent_name: None,
            launch: LaunchParams::default(),
            signal: LifecycleSignal::Registered,
            agent_pid: None,
            agent_process_start: None,
            runtime_owner: None,
            worktree_path: None,
            worktree_branch: Some(format!("wt-{slot}")),
            task: None,
            prompt: None,
            description: None,
            transcript_path: None,
            origin: None,
            usage: crate::agents::AgentUsageSummary::default(),
            pane_id: None,
            pane_stamp: None,
            parent_agent_id: None,
        }
    }

    fn synthetic_pane(i: usize) -> PaneRef {
        PaneRef {
            pane_id: PaneId::from_parts(MuxName::Zellij, format!("terminal_{i}")),
            session_name: SESSION_NAME.to_owned(),
            view_id: Some(format!("tab_{}", i % 8)),
            view_kind: Some(ViewKind::Tab),
            view_name: None,
            title: None,
            is_floating: false,
            command: Some("zsh".to_owned()),
            foreground_cmdline: None,
            spawn_command: None,
            cwd: None,
            pane_pid: None,
            pane_process_start: None,
            hosted_agent_kind: None,
            hosted_agent_process_start: None,
            resumed_session_id: None,
            elevated_agent: None,
            first_seen_at_ms: None,
        }
    }
}
