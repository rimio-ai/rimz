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
//! UI. Managed extension instances also identify child Pi sessions through
//! RimZ-owned process-lineage markers and feed lifecycle rows keyed by the
//! child's own session id, so its model, effort, context, and usage envelopes
//! enrich the nested row. Background tasks stay declared off.

pub(crate) mod account;
mod ask;
pub(crate) mod payloads;
pub(crate) mod spend;
pub(crate) mod transcript;

pub(crate) use crate::agents::capabilities::*;

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde_json::Value;

use super::AskKind;
use super::context::{
    AgentContext, AgentCost, AgentCurrentUsage, AgentRateLimits, AgentTokenUsage,
};
use super::definition::{
    AgentSpec, Brand, Capabilities, ConcernCoverage, CoverageAnnotations, HookCoverage,
    LifecycleAnnotations, PlanLabel, RealtimeUsageChannel, RemoteControlCapability, ThreadKey,
    ToolClassification,
};
use super::hook_types::{HookEventSpec, decode_catalog_hook, hook_record};
use super::lifecycle::LifecycleSignal;
use super::managed_source::ManagedSource;
use super::observation::payload_context_pct;
use super::pricing::PriceBook;
use super::{
    AgentLifecycleObservation, AnswerPlanErr, AnswerStep, AskReply, HookOutput, HookRouting,
    Result, SubagentIdentity, TranscriptMessage, agent_config_path, non_empty_trimmed,
    optional_payload_string, resolve_subagent_identity, sanitize_user_prompt,
};
use crate::transcript::AskQuestion;

/// Everything `const` about Pi, in one place. See [`AgentSpec`] for the
/// spec-vs-trait split.
static PI_DESCRIPTOR: AgentSpec = AgentSpec {
    kind: "pi",
    aliases: &[],
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
        direct_account_usage: true,
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

const PI_COVERAGE: CoverageAnnotations = CoverageAnnotations {
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
        via: "child pi sessions self-identify through RimZ process-lineage markers and feed lifecycle keyed by their own session id",
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

const PI_LIFECYCLE_HOOKS: LifecycleAnnotations = LifecycleAnnotations {
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

/// Everything the extension wires, in `pi.on(...)` registration order. Selector
/// and context-update records remain lifecycle-classified enrichment markers.
const PI_HOOKS: &[HookEventSpec] = &[
    hook_record!(lifecycle, "session_start", r#"{"session_id":"sess-1"}"#).progress(),
    hook_record!(lifecycle, "before_agent_start", r#"{"session_id":"sess-1","prompt":"fix auth"}"#).progress(),
    hook_record!(lifecycle, "agent_end", r#"{"session_id":"sess-1","stop_reason":"stop"}"#).progress(),
    hook_record!(lifecycle, "agent_settled", r#"{"session_id":"sess-1","stop_reason":"stop"}"#),
    hook_record!(lifecycle, "turn_end", r#"{"session_id":"sess-1"}"#).progress(),
    hook_record!(lifecycle, "after_provider_response", r#"{"session_id":"sess-1"}"#),
    hook_record!(lifecycle, "message_update", r#"{"session_id":"sess-1"}"#).progress(),
    hook_record!(lifecycle, "session_info_changed", r#"{"session_id":"sess-1","session_name":"Parser cleanup"}"#),
    hook_record!(lifecycle, "tool_execution_end", r#"{"session_id":"sess-1","tool_call_id":"sibling-call","tool_name":"bash"}"#).progress(),
    hook_record!(lifecycle, "model_select", r#"{"session_id":"sess-1","model":"gpt-5.5"}"#),
    hook_record!(lifecycle, "thinking_level_select", r#"{"session_id":"sess-1","effort":"high"}"#),
    hook_record!(lifecycle, "session_before_compact", r#"{"session_id":"sess-1"}"#),
    hook_record!(lifecycle, "session_compact", r#"{"session_id":"sess-1"}"#),
    hook_record!(lifecycle, "session_shutdown", r#"{"session_id":"sess-1"}"#).session_ended(),
    hook_record!(lifecycle, "subagent_started", r#"{"session_id":"sess-1","cwd":"/work/project","subagent_id":"run-1#0","subagent_label":"scout","subagent_source":"pi-session"}"#),
    hook_record!(lifecycle, "subagent_stopped", r#"{"session_id":"sess-1","cwd":"/work/project","subagent_id":"run-1#0","subagent_label":"scout","subagent_source":"pi-session","errored":true,"total_tokens":1200}"#),
    hook_record!(blocking, "tool_call", r#"{"session_id":"sess-1","tool_call_id":"ask-call","tool_name":"ask_user_question","tool_input":{"questions":[{"question":"Which route?","header":"Route","options":[{"label":"Safe","description":"Stage it"},{"label":"Fast","description":"Ship it"}]}]}}"#, AskKind::Question)
        .synchronous(),
];

/// The RimZ pi extension, embedded at compile time and written whole-file on
/// install. Carries [`super::managed_source::RIMZ_MANAGED_MARKER`] on its first
/// line.
const EXTENSION_SOURCE: &str = include_str!("extension.ts");

const PI_MANAGED_SOURCE: ManagedSource = ManagedSource::new(
    "pi",
    EXTENSION_SOURCE,
    PI_HOOKS,
    "extension",
    pi_extension_path,
    true,
);

#[derive(Clone, Debug, Default)]
pub struct PiAdapter;

impl crate::agents::capabilities::CoreCapability for PiAdapter {
    fn spec(&self) -> &'static AgentSpec {
        &PI_DESCRIPTOR
    }
}

impl crate::agents::capabilities::LaunchCapability for PiAdapter {}

impl crate::agents::capabilities::HookCapability for PiAdapter {
    fn decode_hook(&self, event_name: &str, payload: &Value) -> Result<HookOutput> {
        // Only the rpiv questionnaire blocks on native UI. Ordinary tool calls
        // remain neutral, and headless calls cannot strand a waiting row.
        let ask_kind = (event_name == "tool_call"
            && payload.get("has_ui").and_then(Value::as_bool) != Some(false))
        .then(|| {
            self.spec()
                .blocking_tool_kind(payload.get("tool_name").and_then(Value::as_str))
        })
        .flatten();
        let mut decoded = decode_catalog_hook(PI_HOOKS, event_name, ask_kind);
        let agent_id = optional_payload_string(payload, &["session_id"]);
        decoded.set_routing(
            HookRouting::session(agent_id.map(Into::into))
                .with_worktree(optional_payload_string(payload, &["worktree_path", "cwd"])),
        );
        let questions = if event_name == "tool_call"
            && payload.get("has_ui").and_then(Value::as_bool) != Some(false)
        {
            payload
                .get("tool_name")
                .and_then(Value::as_str)
                .and_then(|tool_name| ask::question_detail(tool_name, payload.get("tool_input")?))
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        decoded.set_ask(questions, None);
        decoded.set_native_answers(
            (event_name == "tool_execution_end"
                && payload.get("tool_name").and_then(Value::as_str) == Some("ask_user_question"))
            .then(|| ask::answer_detail(payload))
            .flatten(),
        );
        if [
            "model",
            "effort",
            "rate_limits",
            "total_cost_usd",
            "context_window",
            "total_tokens",
            "context_pct",
        ]
        .into_iter()
        .any(|key| payload.get(key).is_some())
        {
            decoded.set_observed_context(pi_observed_context(self.spec().kind, payload));
        }
        if matches!(event_name, "subagent_started" | "subagent_stopped") {
            let signal = if event_name == "subagent_started" {
                LifecycleSignal::SubagentStarted
            } else {
                LifecycleSignal::SubagentStopped {
                    errored: payload
                        .get("errored")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                }
            };
            let (agent_id, parent_agent_id) = match resolve_subagent_identity(
                self.spec().kind,
                event_name,
                payload.get("subagent_id").and_then(Value::as_str),
                payload.get("session_id").and_then(Value::as_str),
                payload,
            ) {
                SubagentIdentity::Resolved {
                    agent_id,
                    parent_agent_id,
                } => (agent_id, parent_agent_id),
                SubagentIdentity::Quarantined => return Ok(decoded),
            };
            let mut observation = AgentLifecycleObservation::new(Some(agent_id), signal)
                .with_worktree_from_payload(payload);
            observation.parent_agent_id = Some(parent_agent_id);
            observation.task = optional_payload_string(payload, &["subagent_label"])
                .and_then(|value| non_empty_trimmed(&value));
            if event_name == "subagent_stopped" {
                observation.usage.total_tokens =
                    payload.get("total_tokens").and_then(Value::as_u64);
            }
            decoded.attach_lifecycle(observation);
            return Ok(decoded);
        }

        let parsed = payloads::parse_payload(payload);
        let tool_name = payload.get("tool_name").and_then(Value::as_str);
        let blocking_kind = (payload.get("has_ui").and_then(Value::as_bool) != Some(false))
            .then(|| self.spec().blocking_tool_kind(tool_name))
            .flatten();
        let signal = match event_name {
            "session_start" => Some(LifecycleSignal::Registered),
            "before_agent_start" => Some(LifecycleSignal::TurnStarted),
            "agent_settled" if parsed.stop_reason.as_deref() == Some("aborted") => {
                Some(LifecycleSignal::TurnInterrupted)
            }
            "agent_settled" => Some(LifecycleSignal::TurnEnded {
                errored: payloads::agent_end_errored(&parsed),
                parked_on_background: false,
            }),
            "tool_call" => blocking_kind.map(|kind| LifecycleSignal::AwaitingInput {
                kind,
                ask_id: None,
                detail: None,
                native_key: optional_payload_string(payload, &["tool_call_id"]),
            }),
            "tool_execution_end" if tool_name == Some("ask_user_question") => {
                Some(LifecycleSignal::ToolUsed {
                    mutates: false,
                    edits: false,
                    native_key: optional_payload_string(payload, &["tool_call_id"]),
                })
            }
            "tool_execution_end" if self.spec().tool_mutates(payload) => {
                Some(LifecycleSignal::ToolUsed {
                    mutates: true,
                    edits: self.spec().tool_edits_files(payload),
                    native_key: optional_payload_string(payload, &["tool_call_id"]),
                })
            }
            "session_before_compact" => Some(LifecycleSignal::Compacting),
            "session_compact" => Some(LifecycleSignal::CompactionEnded {
                auto: parsed
                    .compaction_reason
                    .as_ref()
                    .and_then(payloads::PiCompactionReason::auto_flag),
            }),
            "session_shutdown" => Some(LifecycleSignal::Ended),
            _ => None,
        };
        let Some(signal) = signal else {
            return Ok(decoded);
        };
        let agent_id = decoded.event_agent_id().cloned();
        let mut observation =
            AgentLifecycleObservation::new(agent_id, signal).with_worktree_from_payload(payload);
        observation.task = sanitize_user_prompt(parsed.prompt.as_deref());
        observation.prompt = sanitize_user_prompt(parsed.prompt.as_deref());
        observation.launch.model = parsed.model;
        observation.launch.effort = parsed.effort;
        observation.usage.context_pct = payload_context_pct(payload, None);
        observation.usage.context_window = parsed.context_window;
        observation.usage.total_tokens = parsed.total_tokens;
        observation.usage.cache_read_input_tokens = parsed.cache_read_input_tokens;
        observation.usage.cache_write_input_tokens = parsed.cache_write_input_tokens;
        observation.usage.fresh_input_tokens = parsed.input_tokens;
        observation.usage.output_tokens = parsed.output_tokens;
        decoded.set_final_message(
            (event_name == "agent_settled")
                .then_some(parsed.last_assistant_message)
                .flatten()
                .as_deref()
                .and_then(non_empty_trimmed),
        );
        decoded.attach_lifecycle(observation);
        Ok(decoded)
    }

    #[cfg(test)]
    fn conformance(&self) -> super::AdapterConformance {
        let mut samples = super::hook_types::catalog_classification_corpus(PI_HOOKS);
        samples.push(super::ClassificationSample::new(
            "tool_execution_end",
            serde_json::json!({
                "session_id": "sess-1",
                "tool_call_id": "ask-call",
                "tool_name": "ask_user_question"
            }),
            super::AgentHookClass::Lifecycle,
            None,
        ));
        super::AdapterConformance {
            classification: samples,
            spend: Some(super::SpendFixture {
                session_id: "sess-1",
                file_name: "2026-06-02T10-00-00-000Z_sess-1.jsonl",
                body: super::SpendFixtureBody::Jsonl(
                    r#"{"type":"message","timestamp":"2026-06-02T10:00:00.000Z","message":{"role":"assistant","model":"gpt-5","usage":{"input":100,"output":50,"cost":{"total":0.42}}}}"#,
                ),
            }),
            ..super::AdapterConformance::default()
        }
    }

    fn answer_plan(
        &self,
        kind: AskKind,
        questions: &[AskQuestion],
        answers: &[AskReply],
    ) -> std::result::Result<Vec<AnswerStep>, AnswerPlanErr> {
        ask::answer_plan(kind, questions, answers)
    }
}

impl crate::agents::capabilities::InstallationCapability for PiAdapter {
    fn managed_integration(&self) -> Option<&'static dyn super::ManagedIntegration> {
        Some(&PI_MANAGED_SOURCE)
    }
}

impl crate::agents::capabilities::TranscriptCapability for PiAdapter {
    fn parse_transcript_messages(&self, lines: &str) -> Vec<TranscriptMessage> {
        transcript::parse_messages(lines)
    }
}

impl crate::agents::capabilities::ContextCapability for PiAdapter {
    fn observe_context(&self, source: &str, payload: &Value) -> Option<super::ContextObservation> {
        pi_observed_context(source, payload)
    }
}

impl crate::agents::capabilities::AccountCapability for PiAdapter {
    fn probe_account(&self) -> crate::agents::account::AccountProbe {
        account::probe()
    }

    fn probe_account_usage(&self) -> crate::agents::AccountUsageProbe {
        account::probe_usage()
    }
}

impl crate::agents::capabilities::SpendingCapability for PiAdapter {
    fn spending_sources(&self) -> Vec<crate::agents::spending::SpendingSource> {
        spend::pi_session_roots()
            .into_iter()
            .filter_map(|root| crate::agents::spending::SpendingSourceTree::new(root, "**/*.jsonl"))
            .map(|tree| crate::agents::spending::SpendingSource::group(vec![tree]))
            .collect()
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
}

fn pi_observed_context(source: &str, payload: &Value) -> Option<super::ContextObservation> {
    let parsed = payloads::parse_payload(payload);
    let agent_id = parsed.session_id.clone()?;
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
    let context = AgentContext {
        session_name: parsed.session_name,
        model_id: parsed.model,
        effort: parsed.effort,
        cost,
        tokens,
        rate_limits,
        ..AgentContext::new(source, Timestamp::now())
    };
    super::ContextObservation::new(agent_id, context)
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
