//! Amp plugin adapter.
//!
//! Amp has no command-hook or statusline protocol. Rimz installs a small
//! observation-only TypeScript plugin that forwards the active thread's native
//! lifecycle events without entering Amp's tool-decision path.

pub(crate) mod account;
pub(crate) mod payloads;
mod spend;
mod thread;
mod transcript;

use std::path::{Path, PathBuf};

use serde_json::Value;
#[cfg(test)]
use serde_json::json;

#[cfg(test)]
use super::PresetErr;
use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, ConcernCoverage, HookCoverage, IntegrationCoverage,
    LifecycleCoverage, PlanLabel, RealtimeUsageChannel, RemoteControlCapability, ThreadKey,
    ToolClassification,
};
use super::lifecycle::LifecycleSignal;
use super::managed_source::ManagedSource;
use super::{
    AgentAdapter, AgentCost, AgentCurrentUsage, AgentErr, AgentLifecycleObservation,
    AgentTokenUsage, AskKind, ClassifiedHook, LocalContextRefresh, LocalContextRefreshCtx,
    RefreshTrigger, Result, SessionOrigin, TranscriptStat, classify_agent_hook, non_empty_trimmed,
    sanitize_user_prompt,
};
use crate::ids::AgentSessionId;

static AMP_DESCRIPTOR: AgentDescriptor = AgentDescriptor {
    kind: "amp",
    display_name: "Amp",
    brand: Brand {
        emblem: None,
        color: 255,
        color_rgb: (0xee, 0xee, 0xee),
    },
    plan_label: PlanLabel::TitleCaseOnly,
    sub_providers: &[],
    tools: ToolClassification {
        mutating: &["shell_command", "apply_patch", "create_file", "edit_file"],
        editing: &["apply_patch", "create_file", "edit_file"],
        blocking: &[],
    },
    capabilities: Capabilities {
        native_ask_ui: true,
        transcript_tail_context: false,
        registers_lazily: false,
        local_session_discovery: false,
        daemon_hooked_sessions: false,
        realtime_usage: RealtimeUsageChannel {
            windows_defer_to_fresh_realtime: false,
        },
        remote_control: RemoteControlCapability {
            pane_sessions: false,
            background_sessions: false,
        },
    },
    coverage: AMP_COVERAGE,
    lifecycle_hooks: AMP_LIFECYCLE_HOOKS,
    default_context_window: None,
    default_model: None,
    process_names: &["amp", "node"],
    bin_names: &["amp"],
    extra_bin_dirs: &[],
    activity_events: &["session_start", "agent_start", "tool_result", "agent_end"],
    thread_key: ThreadKey::PerFile,
    launch: super::LaunchSpec {
        program: Some("amp"),
        fixed_args: &[],
        prompt: super::PromptStyle::None,
        resume: Some(super::SessionCommand {
            before_id: &["amp", "threads", "continue"],
            after_id: &[],
        }),
        fork: None,
        permission: super::LaunchPermissionArgs::EMPTY,
        ping_args: None,
        max_turn_flag: None,
        compact_command: None,
        presets: super::PresetMatchers {
            model: Some(super::StaticPresetMatcher::Flag(&["--mode"])),
            effort: Some(super::StaticPresetMatcher::Flag(&["--effort"])),
            system_prompt_file: None,
            append_system_prompt_file: None,
        },
    },
};

const AMP_COVERAGE: IntegrationCoverage = IntegrationCoverage {
    turn_lifecycle: ConcernCoverage::Wired {
        via: "agent_start/agent_end",
    },
    permission: ConcernCoverage::Wired {
        via: "thread-state awaiting-approval",
    },
    plan_approval: ConcernCoverage::Unsupported {
        reason: "no native event",
    },
    user_question: ConcernCoverage::Unsupported {
        reason: "no native event",
    },
    answer: ConcernCoverage::Unsupported {
        reason: "no external resolver",
    },
    compaction: ConcernCoverage::Unsupported {
        reason: "automatic compaction has no event",
    },
    subagents: ConcernCoverage::Unsupported {
        reason: "interactive events expose no durable child identity",
    },
    background_parking: ConcernCoverage::Unsupported {
        reason: "no background-task parking signal",
    },
    session_end: ConcernCoverage::Partial {
        via: "pane liveness + rollup reaper",
        gap: "no session-end event",
    },
    idle_notification: ConcernCoverage::Partial {
        via: "turn-end + awaiting-approval + stall window",
        gap: "no notification event",
    },
    context_usage: ConcernCoverage::Partial {
        via: "private thread cache on turn boundaries + producer tick",
        gap: "no stable context-window divisor",
    },
    realtime_cost: ConcernCoverage::Partial {
        via: "private thread cache tokens + estimated model pricing",
        gap: "60s mid-turn cadence and unknown models can omit dollars",
    },
    rich_context: ConcernCoverage::Unsupported {
        reason: "no out-of-band context transport",
    },
    hook_install: ConcernCoverage::Wired {
        via: "~/.config/amp/plugins/rimz.ts",
    },
    account_spend: ConcernCoverage::Partial {
        via: "private rewritten thread-cache fold",
        gap: "estimated pricing does not reconcile Amp credits or workspace billing",
    },
    remote_control: ConcernCoverage::Unsupported {
        reason: "readiness is not detectable",
    },
};

const AMP_LIFECYCLE_HOOKS: LifecycleCoverage = LifecycleCoverage {
    registered: HookCoverage::Native {
        event: "session_start",
    },
    turn_started: HookCoverage::Native {
        event: "agent_start",
    },
    turn_ended: HookCoverage::Native { event: "agent_end" },
    tool_used: HookCoverage::Native {
        event: "tool_result",
    },
    awaiting_input: HookCoverage::Native {
        event: "permission_ask",
    },
    subagent_started: HookCoverage::Absent {
        reason: "no interactive subagent event",
    },
    subagent_stopped: HookCoverage::Absent {
        reason: "no interactive subagent event",
    },
    compacting: HookCoverage::Absent {
        reason: "automatic compaction has no event",
    },
    compaction_ended: HookCoverage::Absent {
        reason: "automatic compaction has no event",
    },
    ended: HookCoverage::Derived {
        via: "pane liveness + rollup reaper",
        gap: "no session-end event",
    },
    lost: HookCoverage::Derived {
        via: "rimz exec wrapper",
        gap: "native hooks do not report mux-session death",
    },
};

const LIFECYCLE_EVENTS: &[&str] = &["session_start", "agent_start", "tool_result", "agent_end"];
const WIRED_EVENTS: &[&str] = &[
    "session_start",
    "agent_start",
    "tool_result",
    "agent_end",
    "permission_ask",
];
const PLUGIN_SOURCE: &str = include_str!("plugin.ts");
const AMP_MANAGED_SOURCE: ManagedSource = ManagedSource::new(
    "amp",
    PLUGIN_SOURCE,
    WIRED_EVENTS,
    "plugin",
    amp_plugin_path,
    false,
);

#[derive(Clone, Debug, Default)]
pub struct AmpAdapter;

impl AgentAdapter for AmpAdapter {
    fn descriptor(&self) -> &'static AgentDescriptor {
        &AMP_DESCRIPTOR
    }

    fn classify_hook(&self, event_name: &str, _payload: &Value) -> ClassifiedHook {
        let ask_kind = (event_name == "permission_ask").then_some(AskKind::Permission);
        classify_agent_hook(event_name, ask_kind, LIFECYCLE_EVENTS)
    }

    #[cfg(test)]
    fn native_hook_events(&self) -> Vec<&'static str> {
        WIRED_EVENTS.to_vec()
    }

    #[cfg(test)]
    fn classification_corpus(&self) -> Vec<super::ClassificationSample> {
        use super::{AgentHookClass, ClassificationSample};

        vec![
            ClassificationSample::new(
                "session_start",
                json!({ "session_id": "T-abc123", "cwd": "/tmp/repo" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "agent_start",
                json!({ "session_id": "T-abc123", "prompt": "fix auth" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "tool_result",
                json!({ "session_id": "T-abc123", "tool_name": "apply_patch", "status": "done", "files_modified": true }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "agent_end",
                json!({ "session_id": "T-abc123", "status": "done" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "permission_ask",
                json!({ "session_id": "T-abc123" }),
                AgentHookClass::AwaitingUser,
                Some(AskKind::Permission),
            ),
        ]
    }

    #[cfg(test)]
    fn spend_fixture(&self) -> Option<super::SpendFixture> {
        Some(super::SpendFixture {
            session_id: "T-conformance",
            file_name: "T-conformance.json",
            body: super::SpendFixtureBody::Jsonl(
                r#"{"id":"T-conformance","messages":[{"role":"assistant","messageId":"m1","content":"done","usage":{"timestamp":"2026-01-01T00:00:00Z","model":"gpt-5","inputTokens":100,"outputTokens":20}}]}"#,
            ),
        })
    }

    fn render_neutral(&self, _event_name: &str) -> Result<Option<Value>> {
        Ok(None)
    }

    fn observe_lifecycle(
        &self,
        event_name: &str,
        payload: &Value,
    ) -> Option<AgentLifecycleObservation> {
        let parsed = payloads::parse_payload(payload);
        let session_id = parsed.session_id.as_deref()?.trim();
        if session_id.is_empty() {
            return None;
        }
        let signal = match event_name {
            "session_start" => LifecycleSignal::Registered,
            "agent_start" => LifecycleSignal::TurnStarted,
            "tool_result" => LifecycleSignal::ToolUsed {
                mutates: true,
                edits: parsed
                    .files_modified
                    .unwrap_or_else(|| self.descriptor().tool_edits_files(payload)),
            },
            "agent_end" => LifecycleSignal::TurnEnded {
                errored: parsed.status.as_deref() != Some("done"),
                parked_on_background: false,
            },
            "permission_ask" => LifecycleSignal::AwaitingInput {
                kind: AskKind::Permission,
                ask_id: None,
                detail: None,
            },
            _ => return None,
        };

        let mut observation =
            AgentLifecycleObservation::new(Some(AgentSessionId::from(session_id)), signal)
                .with_worktree_from_payload(payload);
        let prompt = sanitize_user_prompt(parsed.prompt.as_deref());
        observation.task = prompt.clone();
        observation.prompt = prompt;
        observation.launch.model = parsed.model;
        observation.launch.effort = parsed.effort;
        stamp_transcript_path(&mut observation, session_id, &spend::data_root());
        if event_name == "session_start" {
            // Amp's Fresh lineage means fresh pane occupancy, not a fresh
            // conversation: focusing an existing thread must supersede the
            // previously focused thread in the same pane.
            observation.origin = Some(SessionOrigin::Fresh);
        }
        Some(observation)
    }

    fn local_context_refresh(
        &self,
        trigger: RefreshTrigger<'_>,
        ctx: &LocalContextRefreshCtx<'_>,
    ) -> Option<LocalContextRefresh> {
        if let RefreshTrigger::Hook(event_name) = trigger
            && !matches!(event_name, "session_start" | "agent_end")
        {
            return None;
        }
        let path =
            spend::resolve_session_file(ctx.agent_id, ctx.prior_transcript_path.map(Path::new))?;
        let stat = TranscriptStat::from_path(&path)?;
        if ctx.prior_transcript_stat == Some(&stat) {
            return None;
        }
        let parsed = thread::AmpThread::read(&path).ok()?;
        if parsed.id != ctx.agent_id {
            return None;
        }
        let latest = parsed.usage.iter().max_by_key(|usage| usage.at);
        let model_id = latest
            .map(|usage| usage.model.clone())
            .or_else(|| ctx.model_hint.map(ToOwned::to_owned));
        let tokens = latest.map(|usage| AgentTokenUsage {
            context_window_size: None,
            used_percentage: None,
            remaining_percentage: None,
            current_usage: Some(AgentCurrentUsage {
                input_tokens: Some(usage.input),
                output_tokens: Some(usage.output),
                cache_creation_input_tokens: Some(usage.cache_write),
                cache_read_input_tokens: Some(usage.cache_read),
            }),
            session_usage: None,
        });
        let prices = super::pricing::cached_book(ctx.shared_pricing_cache_path);
        let (entries, _) = spend::entries_from_thread(&parsed, &prices);
        let cost_usd = entries.iter().map(|entry| entry.cost_usd).sum::<f64>();
        Some(LocalContextRefresh {
            model_id,
            tokens,
            cost: (cost_usd > 0.0).then_some(AgentCost {
                total_cost_usd: Some(cost_usd),
                ..AgentCost::default()
            }),
            transcript_path: Some(path.to_string_lossy().into_owned()),
            transcript_stat: Some(stat),
            ..LocalContextRefresh::default()
        })
    }

    fn transcript_files(&self) -> Vec<PathBuf> {
        spend::session_files()
    }

    fn session_transcript(&self, session_id: &str, prior_path: Option<&Path>) -> Option<PathBuf> {
        spend::resolve_session_file(session_id, prior_path)
    }

    fn read_transcript_messages(
        &self,
        path: &Path,
        session_id: Option<&AgentSessionId>,
    ) -> std::io::Result<Vec<crate::agents::TranscriptMessage>> {
        transcript::read_messages(path, session_id)
    }

    fn transcript_position(
        &self,
        path: &Path,
        session_id: Option<&AgentSessionId>,
    ) -> Option<crate::agents::TranscriptPosition> {
        transcript::position(path, session_id)
    }

    fn read_assistant_transcript_page(
        &self,
        path: &Path,
        session_id: Option<&AgentSessionId>,
        position: crate::agents::TranscriptPosition,
    ) -> Option<crate::agents::TranscriptPage> {
        transcript::read_assistant_page(path, session_id, position)
    }

    fn parse_spend(
        &self,
        path: &Path,
        _resume: Option<&super::spending::SpendCursor>,
        prices: &super::PriceBook,
    ) -> super::spending::SpendParse {
        spend::parse(path, prices)
    }

    fn last_assistant_message(
        &self,
        event_name: &str,
        payload: &Value,
        _observation: &AgentLifecycleObservation,
    ) -> Option<String> {
        (event_name == "agent_end")
            .then(|| payloads::parse_payload(payload).last_assistant_message)
            .flatten()
            .as_deref()
            .and_then(non_empty_trimmed)
    }

    fn launch_command(&self, extra_args: &[String], prompt: Option<&str>) -> Option<Vec<String>> {
        let mut argv = vec!["amp".to_owned()];
        argv.extend(extra_args.iter().cloned());
        if let Some(prompt) = prompt {
            argv.extend([
                "-x".to_owned(),
                prompt.to_owned(),
                "--plugin-ready-timeout".to_owned(),
                "30".to_owned(),
            ]);
        }
        Some(argv)
    }

    fn managed_source(&self) -> Option<&'static ManagedSource> {
        Some(&AMP_MANAGED_SOURCE)
    }

    fn probe_account(&self) -> crate::agents::account::AccountProbe {
        account::probe()
    }
}

fn amp_plugin_path() -> Result<PathBuf> {
    if let Some(raw) = std::env::var_os("RIMZ_AMP_PLUGIN").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(raw));
    }
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .map(|home| home.join(".config"))
        })
        .ok_or_else(|| AgentErr::Install {
            agent: "amp",
            reason: "$HOME is not set; cannot resolve ~/.config/amp/plugins/rimz.ts".to_owned(),
        })?;
    Ok(config_home.join("amp/plugins/rimz.ts"))
}

fn stamp_transcript_path(
    observation: &mut AgentLifecycleObservation,
    session_id: &str,
    data_root: &Path,
) {
    observation.transcript_path = spend::resolve_session_file_at(data_root, session_id)
        .map(|path| path.to_string_lossy().into_owned());
}

#[cfg(test)]
mod tests;
