//! Pi hook adapter.
//!
//! Pi's integration surface is in-process TypeScript extensions, so the
//! adapter ships one — [`extension.ts`](./extension.ts), embedded at compile
//! time and installed whole-file to `~/.pi/agent/extensions/rimz.ts`. The
//! extension forwards pi's lifecycle events to `rimz hooks feed --source pi`
//! as fire-and-forget children, inverting the Claude/Codex child direction
//! (pi runs RimZ, not the other way around); the wire it posts is the typed
//! shape in [`payloads`], with the model, effort, and context gauge
//! (`context_pct` / `context_window` / `total_tokens`) and cumulative cost
//! stamped on every envelope from the in-process extension — payload-first, so
//! the sidebar's bar and dollar line stay current with the turn-end spend walk
//! reconciling the final total.
//! Lifecycle maps per docs/internals/agents/pi.md: `session_start`
//! registers, `before_agent_start` starts the turn with the prompt,
//! `agent_end` captures its in-band verdict, `agent_settled` ends it once no
//! automatic continuation remains,
//! `tool_execution_end` is the mutating-tool heartbeat and questionnaire
//! resolution boundary, and
//! `session_before_compact`/`session_compact`/`session_shutdown` are the
//! compaction and exit signals. Spend stays in [`spend`].
//!
//! One wired event is an ask: `tool_call`, pi's pre-tool gate, whose extension
//! handler pi awaits. The `@juicesharp/rpiv-ask-user-question` extension draws
//! the `ask_user_question` questionnaire in pi's pane; RimZ records that one
//! blocking tool as a native question and returns neutral so pi can open its
//! UI. The managed extension also normalizes async child runs from the
//! `pi-subagents` and `@tintinweb/pi-subagents` event buses into shared child
//! lifecycle rows. Background tasks stay declared off.

pub(crate) mod account;
mod ask;
pub(crate) mod payloads;
pub(crate) mod spend;
pub(crate) mod transcript;

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde_json::Value;
#[cfg(test)]
use serde_json::json;

use super::AskKind;
use super::context::{
    AgentContext, AgentCost, AgentCurrentUsage, AgentRateLimits, AgentTokenUsage,
};
use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, ConcernCoverage, HookCoverage, IntegrationCoverage,
    LifecycleCoverage, PlanLabel, RealtimeUsageChannel, RemoteControlCapability, ThreadKey,
    ToolClassification,
};
use super::lifecycle::LifecycleSignal;
use super::managed_source::ManagedSource;
use super::observation::payload_context_pct;
use super::pricing::PriceBook;
use super::{
    AgentAdapter, AgentLifecycleObservation, AnswerPlanErr, AnswerStep, AskReply, ClassifiedHook,
    Result, SubagentIdentity, TranscriptMessage, agent_config_path, classify_agent_hook,
    non_empty_trimmed, optional_payload_string, resolve_subagent_identity, sanitize_user_prompt,
};
use crate::ids::AgentSessionId;
use crate::transcript::{AskAnswer, AskQuestion};

/// Everything `const` about Pi, in one place. See [`AgentDescriptor`] for the
/// descriptor-vs-trait split.
static PI_DESCRIPTOR: AgentDescriptor = AgentDescriptor {
    kind: "pi",
    display_name: "Pi",
    brand: Brand {
        emblem: None,
        color: 29,
        color_rgb: (0x27, 0xa0, 0x77),
    },
    // Pi sessions span whatever provider account the user wired, so no single
    // brand prefix is honest — the tier renders bare.
    plan_label: PlanLabel::TitleCaseOnly,
    // Pi is the multi-provider client: it runs *on* other providers'
    // subscriptions rather than metering one of its own.
    sub_providers: &[],
    expected_windows: &[],
    // Pi's built-in tool set: `edit`/`write` edit files; `bash` mutates
    // without editing, so the reasoning phase survives it. The rpiv
    // questionnaire extension contributes the one blocking ask tool.
    tools: ToolClassification {
        mutating: &["bash", "edit", "write"],
        editing: &["edit", "write"],
        blocking: &[("ask_user_question", AskKind::Question)],
    },
    capabilities: Capabilities {
        // Pi itself runs tools unasked; the rpiv questionnaire extension owns
        // a native blocking question UI on the same awaited `tool_call` gate.
        native_ask_ui: true,
        transcript_tail_context: false,
        registers_lazily: false,
        local_session_discovery: false,
        daemon_hooked_sessions: false,
        same_pane_session: super::SamePaneSessionPolicy::KeepPrimary,
        realtime_usage: RealtimeUsageChannel {
            windows_defer_to_fresh_realtime: false,
        },
        remote_control: RemoteControlCapability {
            pane_sessions: false,
            background_sessions: false,
        },
    },
    coverage: PI_COVERAGE,
    lifecycle_hooks: PI_LIFECYCLE_HOOKS,
    default_context_window: None,
    default_model: None,
    process_names: &["pi"],
    bin_names: &["pi"],
    extra_bin_dirs: &[],
    // Pi's progress-proving events, in its own wire vocabulary. The blocking
    // `tool_call` is excluded like Claude's `PreToolUse`: it fires while the
    // ask is being created, so touching on it would instantly un-block the
    // row. Every *completed* tool still touches via `tool_execution_end`.
    activity_events: &[
        "session_start",
        "before_agent_start",
        "agent_end",
        "message_update",
        "turn_end",
        "tool_execution_end",
    ],
    thread_key: ThreadKey::PerFile,
    launch: super::LaunchSpec {
        program: Some("pi"),
        fixed_args: &[],
        prompt: super::PromptStyle::PositionalAfterDoubleDash,
        resume: Some(super::SessionCommand {
            before_id: &["pi", "--session"],
            after_id: &[],
        }),
        fork: Some(super::SessionCommand {
            before_id: &["pi", "--fork"],
            after_id: &[],
        }),
        permission: super::LaunchPermissionArgs::EMPTY,
        ping_args: None,
        max_turn_flag: None,
        compact_command: Some("/compact"),
        presets: super::PresetMatchers {
            model: Some(super::StaticPresetMatcher::Flag(&["--model"])),
            effort: Some(super::StaticPresetMatcher::Flag(&["--thinking"])),
            system_prompt_file: None,
            append_system_prompt_file: None,
        },
    },
};

const PI_COVERAGE: IntegrationCoverage = IntegrationCoverage {
    turn_lifecycle: ConcernCoverage::Wired {
        via: "session_start/before_agent_start/agent_settled",
    },
    permission: ConcernCoverage::Unsupported {
        reason: "pi runs tools unasked; the pre-tool gate stays neutral",
    },
    plan_approval: ConcernCoverage::Unsupported {
        reason: "no plan-approval gate",
    },
    user_question: ConcernCoverage::Wired {
        via: "tool_call (ask_user_question, rpiv extension tool)",
    },
    answer: ConcernCoverage::Wired {
        via: "answer_plan questionnaire choreography",
    },
    compaction: ConcernCoverage::Wired {
        via: "session_before_compact/session_compact",
    },
    subagents: ConcernCoverage::Wired {
        via: "native subagent_started/subagent_stopped bus events bridged in full by the rimz extension",
    },
    background_parking: ConcernCoverage::Unsupported {
        reason: "no background-task parking",
    },
    session_end: ConcernCoverage::Wired {
        via: "session_shutdown",
    },
    // `agent_settled` is the native final-idle boundary, while the shared
    // coverage concern also asks for an idle-timeout Notification nudge.
    idle_notification: ConcernCoverage::Partial {
        via: "agent_settled + stall window",
        gap: "no idle-timeout Notification nudge",
    },
    context_usage: ConcernCoverage::Wired {
        via: "extension context usage (row gauge + AgentContext.tokens)",
    },
    realtime_cost: ConcernCoverage::Wired {
        via: "extension cumulative-cost push reconciled to the authoritative turn-end session-transcript spend sum",
    },
    rich_context: ConcernCoverage::Wired {
        via: "extension envelopes on every value-changing event, including throttled streaming updates and resume hydration",
    },
    hook_install: ConcernCoverage::Wired {
        via: "~/.pi/agent/extensions/rimz.ts",
    },
    account_spend: ConcernCoverage::Wired {
        via: "auth.json/session spend + after_provider_response headers + OAuth usage probe",
    },
    remote_control: ConcernCoverage::Unsupported {
        reason: "no remote-control surface",
    },
};

const PI_LIFECYCLE_HOOKS: LifecycleCoverage = LifecycleCoverage {
    registered: HookCoverage::Native {
        event: "session_start",
    },
    turn_started: HookCoverage::Native {
        event: "before_agent_start",
    },
    turn_ended: HookCoverage::Native {
        event: "agent_settled",
    },
    tool_used: HookCoverage::Native {
        event: "tool_execution_end",
    },
    awaiting_input: HookCoverage::Native { event: "tool_call" },
    subagent_started: HookCoverage::Native {
        event: "subagent_started",
    },
    subagent_stopped: HookCoverage::Native {
        event: "subagent_stopped",
    },
    compacting: HookCoverage::Native {
        event: "session_before_compact",
    },
    compaction_ended: HookCoverage::Native {
        event: "session_compact",
    },
    // Extension skips `/reload` shutdown: same session re-registers in place,
    // so every shutdown reaching RimZ is a real end.
    ended: HookCoverage::Native {
        event: "session_shutdown",
    },
    lost: HookCoverage::Derived {
        via: "rimz exec wrapper",
        gap: "native hooks do not report mux-session death",
    },
};

/// The non-blocking events the embedded extension forwards — the lifecycle
/// channel, the single source of truth for classification. The selectors and
/// context-update events are enrichment-only markers: they run the context
/// merge without emitting a lifecycle signal. Mirrors the `pi.on(...)` registrations in
/// [`extension.ts`](./extension.ts) (asserted by test).
const LIFECYCLE_EVENTS: &[&str] = &[
    "session_start",
    "before_agent_start",
    "agent_end",
    "agent_settled",
    "turn_end",
    "after_provider_response",
    "message_update",
    "session_info_changed",
    "tool_execution_end",
    "model_select",
    "thinking_level_select",
    "session_before_compact",
    "session_compact",
    "session_shutdown",
    "subagent_started",
    "subagent_stopped",
];

/// Everything the extension wires, for the install/uninstall reports: the
/// lifecycle set plus the blocking `tool_call` gate.
const WIRED_EVENTS: &[&str] = &[
    "session_start",
    "before_agent_start",
    "agent_end",
    "agent_settled",
    "turn_end",
    "after_provider_response",
    "message_update",
    "session_info_changed",
    "tool_execution_end",
    "model_select",
    "thinking_level_select",
    "session_before_compact",
    "session_compact",
    "session_shutdown",
    "subagent_started",
    "subagent_stopped",
    "tool_call",
];

/// The RimZ pi extension, embedded at compile time and written whole-file on
/// install. Carries [`super::managed_source::RIMZ_MANAGED_MARKER`] on its first
/// line.
const EXTENSION_SOURCE: &str = include_str!("extension.ts");

const PI_MANAGED_SOURCE: ManagedSource = ManagedSource::new(
    "pi",
    EXTENSION_SOURCE,
    WIRED_EVENTS,
    "extension",
    pi_extension_path,
    true,
);

#[derive(Clone, Debug, Default)]
pub struct PiAdapter;

impl AgentAdapter for PiAdapter {
    fn descriptor(&self) -> &'static AgentDescriptor {
        &PI_DESCRIPTOR
    }

    fn classify_hook(&self, event_name: &str, payload: &Value) -> ClassifiedHook {
        // Only the rpiv questionnaire blocks on native UI. Ordinary tool calls
        // remain neutral, and headless calls cannot strand a waiting row.
        let ask_kind = (event_name == "tool_call"
            && payload.get("has_ui").and_then(Value::as_bool) != Some(false))
        .then(|| {
            self.descriptor()
                .blocking_tool_kind(payload.get("tool_name").and_then(Value::as_str))
        })
        .flatten();
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
                "tool_call",
                json!({
                    "session_id": "sess-1",
                    "tool_call_id": "ask-call",
                    "tool_name": "ask_user_question",
                    "tool_input": {
                        "questions": [{
                            "question": "Which route?",
                            "header": "Route",
                            "options": [
                                { "label": "Safe", "description": "Stage it" },
                                { "label": "Fast", "description": "Ship it" }
                            ]
                        }]
                    }
                }),
                AgentHookClass::AwaitingUser,
                Some(AskKind::Question),
            ),
            ClassificationSample::new(
                "session_start",
                json!({ "session_id": "sess-1" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "subagent_started",
                json!({
                    "session_id": "sess-1",
                    "cwd": "/work/project",
                    "subagent_id": "run-1#0",
                    "subagent_label": "scout",
                    "subagent_source": "pi-subagents"
                }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "subagent_stopped",
                json!({
                    "session_id": "sess-1",
                    "cwd": "/work/project",
                    "subagent_id": "run-1#0",
                    "subagent_label": "scout",
                    "subagent_source": "pi-subagents",
                    "errored": true,
                    "total_tokens": 1200
                }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "before_agent_start",
                json!({ "session_id": "sess-1", "prompt": "fix auth" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "agent_end",
                json!({ "session_id": "sess-1", "stop_reason": "stop" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "agent_settled",
                json!({ "session_id": "sess-1", "stop_reason": "stop" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "turn_end",
                json!({ "session_id": "sess-1" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "after_provider_response",
                json!({ "session_id": "sess-1" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "message_update",
                json!({ "session_id": "sess-1" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "session_info_changed",
                json!({ "session_id": "sess-1", "session_name": "Parser cleanup" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "tool_execution_end",
                json!({
                    "session_id": "sess-1",
                    "tool_call_id": "sibling-call",
                    "tool_name": "bash"
                }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "tool_execution_end",
                json!({
                    "session_id": "sess-1",
                    "tool_call_id": "ask-call",
                    "tool_name": "ask_user_question"
                }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "model_select",
                json!({ "session_id": "sess-1", "model": "gpt-5.5" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "thinking_level_select",
                json!({ "session_id": "sess-1", "effort": "high" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "session_before_compact",
                json!({ "session_id": "sess-1" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "session_compact",
                json!({ "session_id": "sess-1" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "session_shutdown",
                json!({ "session_id": "sess-1" }),
                AgentHookClass::Lifecycle,
                None,
            ),
        ]
    }

    #[cfg(test)]
    fn spend_fixture(&self) -> Option<super::SpendFixture> {
        Some(super::SpendFixture {
            session_id: "sess-1",
            file_name: "2026-06-02T10-00-00-000Z_sess-1.jsonl",
            body: super::SpendFixtureBody::Jsonl(
                r#"{"type":"message","timestamp":"2026-06-02T10:00:00.000Z","message":{"role":"assistant","model":"gpt-5","usage":{"input":100,"output":50,"cost":{"total":0.42}}}}"#,
            ),
        })
    }

    fn render_neutral(&self, _event_name: &str) -> Result<Option<Value>> {
        // Empty stdout is the extension's allow: ordinary tools run unasked,
        // while the questionnaire tool proceeds into its own native UI.
        Ok(None)
    }

    fn ask_question_detail(&self, event_name: &str, payload: &Value) -> Option<Vec<AskQuestion>> {
        if event_name != "tool_call"
            || payload.get("has_ui").and_then(Value::as_bool) == Some(false)
        {
            return None;
        }
        ask::question_detail(
            payload.get("tool_name")?.as_str()?,
            payload.get("tool_input")?,
        )
    }

    fn answer_plan(
        &self,
        kind: AskKind,
        questions: &[AskQuestion],
        answers: &[AskReply],
    ) -> std::result::Result<Vec<AnswerStep>, AnswerPlanErr> {
        ask::answer_plan(kind, questions, answers)
    }

    fn native_ask_answer(&self, event_name: &str, payload: &Value) -> Option<Vec<AskAnswer>> {
        (event_name == "tool_execution_end"
            && payload.get("tool_name").and_then(Value::as_str) == Some("ask_user_question"))
        .then(|| ask::answer_detail(payload))
        .flatten()
    }

    fn observe_lifecycle(
        &self,
        event_name: &str,
        payload: &Value,
    ) -> Option<AgentLifecycleObservation> {
        if matches!(event_name, "subagent_started" | "subagent_stopped") {
            let signal = match event_name {
                "subagent_started" => LifecycleSignal::SubagentStarted,
                "subagent_stopped" => LifecycleSignal::SubagentStopped {
                    errored: payload
                        .get("errored")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                },
                _ => return None,
            };
            let (agent_id, parent_agent_id) = match resolve_subagent_identity(
                self.descriptor().kind,
                event_name,
                payload.get("subagent_id").and_then(Value::as_str),
                payload.get("session_id").and_then(Value::as_str),
                payload,
            ) {
                SubagentIdentity::Resolved {
                    agent_id,
                    parent_agent_id,
                } => (agent_id, parent_agent_id),
                SubagentIdentity::Quarantined => return None,
            };
            let mut observation = AgentLifecycleObservation::new(Some(agent_id), signal)
                .with_worktree_from_payload(payload);
            observation.parent_agent_id = Some(parent_agent_id);
            observation.task = optional_payload_string(payload, &["subagent_label"])
                .and_then(|value| non_empty_trimmed(&value));
            if event_name == "subagent_stopped" {
                observation.total_tokens = payload.get("total_tokens").and_then(Value::as_u64);
            }
            return Some(observation);
        }

        let parsed = payloads::parse_payload(payload);
        // The status decision lives in the shared `lifecycle::step` table —
        // here the adapter only names the intent. The native-event → signal
        // mapping is docs/internals/agents/pi.md.
        let tool_name = payload.get("tool_name").and_then(Value::as_str);
        let blocking_kind = (payload.get("has_ui").and_then(Value::as_bool) != Some(false))
            .then(|| self.descriptor().blocking_tool_kind(tool_name))
            .flatten();
        let signal = match event_name {
            "session_start" => LifecycleSignal::Registered,
            // `before_agent_start` carries the prompt. `agent_end` can still
            // be followed by retry, compaction, or queued continuation, so it
            // is enrichment-only; `agent_settled` is the true final boundary.
            "before_agent_start" => LifecycleSignal::TurnStarted,
            // The last assistant message is the in-band death certificate:
            // `stopReason: "error" | "aborted"` plus `errorMessage`, no
            // transcript forensics needed. Pi has no background-task parking.
            "agent_settled" if parsed.stop_reason.as_deref() == Some("aborted") => {
                LifecycleSignal::TurnInterrupted
            }
            "agent_settled" => LifecycleSignal::TurnEnded {
                errored: payloads::agent_end_errored(&parsed),
                parked_on_background: false,
            },
            "tool_call" if blocking_kind.is_some() => LifecycleSignal::AwaitingInput {
                kind: blocking_kind?,
                ask_id: None,
                detail: None,
                native_key: optional_payload_string(payload, &["tool_call_id"]),
            },
            // The questionnaire's completed tool boundary clears waiting for
            // answers, cancellation, validation failure, and headless no-UI.
            "tool_execution_end" if tool_name == Some("ask_user_question") => {
                LifecycleSignal::ToolUsed {
                    mutates: false,
                    edits: false,
                    native_key: optional_payload_string(payload, &["tool_call_id"]),
                }
            }
            // Only a *mutating* tool rides the lifecycle channel: it is proof
            // of real work (read-only tools stay silent). The `edits` bit
            // marks the file-writing subset, which ends the turn's thinking
            // head.
            "tool_execution_end" if self.descriptor().tool_mutates(payload) => {
                LifecycleSignal::ToolUsed {
                    mutates: true,
                    edits: self.descriptor().tool_edits_files(payload),
                    native_key: optional_payload_string(payload, &["tool_call_id"]),
                }
            }
            // A leading signal, like Claude's `PreCompact`.
            "session_before_compact" => LifecycleSignal::Compacting,
            "session_compact" => LifecycleSignal::CompactionEnded {
                auto: parsed
                    .compaction_reason
                    .as_ref()
                    .and_then(payloads::PiCompactionReason::auto_flag),
            },
            // Fires on quit including Ctrl+C/SIGHUP/SIGTERM and on every
            // session replacement (`/new`, `/resume`) — a true session end.
            "session_shutdown" => LifecycleSignal::Ended,
            _ => return None,
        };
        let agent_id = optional_payload_string(payload, &["session_id"]).map(AgentSessionId::from);
        let mut observation =
            AgentLifecycleObservation::new(agent_id, signal).with_worktree_from_payload(payload);
        // A pi row labels with the user's *sanitized* prompt, so harness
        // control text never reaches the row; absent fields are carry-forward.
        observation.task = sanitize_user_prompt(parsed.prompt.as_deref());
        observation.prompt = sanitize_user_prompt(parsed.prompt.as_deref());
        observation.launch.model = parsed.model;
        observation.launch.effort = parsed.effort;
        // The gauge is payload-first and payload-only: the extension stamps
        // it on every envelope from the in-process `ctx.getContextUsage()`,
        // so no transcript tail is ever read (the `None` fallback).
        observation.context_pct = payload_context_pct(payload, None);
        observation.context_window = parsed.context_window;
        observation.total_tokens = parsed.total_tokens;
        observation.cache_read_input_tokens = parsed.cache_read_input_tokens;
        observation.cache_write_input_tokens = parsed.cache_write_input_tokens;
        observation.fresh_input_tokens = parsed.input_tokens;
        observation.output_tokens = parsed.output_tokens;
        Some(observation)
    }

    fn observe_context(&self, source: &str, payload: &Value) -> Option<AgentContext> {
        pi_observed_context(source, payload)
    }

    fn last_assistant_message(
        &self,
        event_name: &str,
        payload: &Value,
        _observation: &AgentLifecycleObservation,
    ) -> Option<String> {
        (event_name == "agent_settled")
            .then(|| payloads::parse_payload(payload).last_assistant_message)
            .flatten()
            .as_deref()
            .and_then(non_empty_trimmed)
    }

    fn parse_transcript_messages(&self, lines: &str) -> Vec<TranscriptMessage> {
        transcript::parse_messages(lines)
    }

    fn transcript_files(&self) -> Vec<PathBuf> {
        spend::pi_session_files()
    }

    /// Pi's present non-negative `costUSD` is authoritative. Token-bearing
    /// records without a usable direct cost fall back to the shared price book.
    /// The resume cursor carries the session header cwd so appended usage
    /// entries retain their workspace origin.
    fn parse_spend(
        &self,
        path: &Path,
        resume: Option<&crate::agents::spending::SpendCursor>,
        prices: &PriceBook,
    ) -> crate::agents::spending::SpendParse {
        spend::parse_pi_spend(path, resume, prices)
    }

    fn managed_source(&self) -> Option<&'static ManagedSource> {
        Some(&PI_MANAGED_SOURCE)
    }

    fn probe_account(&self) -> crate::agents::account::AccountProbe {
        account::probe()
    }

    fn probe_account_usage(&self) -> crate::agents::AccountUsageProbe {
        account::probe_usage()
    }

    fn account_usage_identity(&self) -> Option<crate::agents::AccountUsageIdentity> {
        Some(account::account_usage_identity())
    }
}

fn pi_observed_context(source: &str, payload: &Value) -> Option<AgentContext> {
    let parsed = payloads::parse_payload(payload);
    let current_usage = pi_current_usage(&parsed);
    let tokens = {
        let usage = AgentTokenUsage {
            context_window_size: parsed.context_window,
            used_percentage: payload_context_pct(payload, None),
            remaining_percentage: None,
            current_context_tokens: None,
            current_usage,
            session_usage: None,
        };
        (usage.context_window_size.is_some()
            || usage.used_percentage.is_some()
            || usage.current_usage.is_some())
        .then_some(usage)
    };
    let cost = parsed
        .total_cost_usd
        .filter(|cost| *cost > 0.0)
        .map(|total_cost_usd| AgentCost {
            total_cost_usd: Some(total_cost_usd),
            ..AgentCost::default()
        });
    let windows = parsed
        .rate_limits
        .iter()
        .map(payloads::PiRateLimitWindow::to_domain)
        .collect::<Vec<_>>();
    let rate_limits = (!windows.is_empty()).then_some(AgentRateLimits { windows });
    if parsed.model.is_none()
        && parsed.session_name.is_none()
        && parsed.effort.is_none()
        && tokens.is_none()
        && cost.is_none()
        && rate_limits.is_none()
    {
        return None;
    }
    Some(AgentContext {
        session_name: parsed.session_name,
        model_id: parsed.model,
        effort: parsed.effort,
        cost,
        tokens,
        rate_limits,
        ..AgentContext::new(source, Timestamp::now())
    })
}

fn pi_current_usage(parsed: &payloads::PiHookPayload) -> Option<AgentCurrentUsage> {
    let usage = AgentCurrentUsage {
        input_tokens: parsed.input_tokens,
        output_tokens: parsed.output_tokens,
        cache_creation_input_tokens: parsed.cache_write_input_tokens,
        cache_read_input_tokens: parsed.cache_read_input_tokens,
    };
    (!usage.is_zero()).then_some(usage)
}

fn pi_extension_path() -> Result<PathBuf> {
    // Honour an explicit override (`RIMZ_PI_EXTENSION`) so tests and tooling
    // can point the installer at a tempdir without touching real config. Pi
    // auto-discovers `*.ts`/`*.js` under this directory; install is
    // deliberately user-global — never pi's *project-local* discovery dir
    // (`<project>/.pi/extensions/`, a different path) — so the project trust
    // hash is untouched.
    agent_config_path(
        "pi",
        "RIMZ_PI_EXTENSION",
        Path::new(".pi/agent/extensions/rimz.ts"),
    )
}

#[cfg(test)]
mod tests;
