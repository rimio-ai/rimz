//! Pi hook adapter.
//!
//! Pi's integration surface is in-process TypeScript extensions, so the
//! adapter ships one — [`extension.ts`](./extension.ts), embedded at compile
//! time and installed whole-file to `~/.pi/agent/extensions/rimz.ts`. The
//! extension forwards pi's lifecycle events to `rimz hooks feed --source pi`
//! as fire-and-forget children, inverting the Claude/Codex child direction
//! (pi runs Rimz, not the other way around); the wire it posts is the typed
//! shape in [`payloads`], with the model, effort, and context gauge
//! (`context_pct` / `context_window` / `total_tokens`) and cumulative cost
//! stamped on every envelope from the in-process extension — payload-first, so
//! the sidebar's bar and dollar line stay current with the turn-end spend walk
//! reconciling the final total.
//! Lifecycle maps per docs/internals/agents/pi.md: `session_start`
//! registers, `before_agent_start` starts the
//! turn with the prompt, `agent_end` ends it carrying the in-band error bit,
//! `tool_execution_end` is the mutating-tool heartbeat, and
//! `session_before_compact`/`session_compact`/`session_shutdown` are the
//! compaction and exit signals. Spend stays in [`spend`].
//!
//! One wired event is an ask: `tool_call`, pi's pre-tool gate, whose extension
//! handler pi awaits. Pi draws no permission prompt of its own
//! (`native_ask_ui: false`), so the hook returns neutral immediately and Rimz
//! records no waiting state. Subagents and background tasks stay declared off
//! (`docs/externals/agent-adapter/pi-reference.md`) and the absences render
//! deliberately.

pub(crate) mod account;
pub(crate) mod oauth_usage;
pub(crate) mod payloads;
pub(crate) mod spend;

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde_json::Value;
#[cfg(test)]
use serde_json::json;

use super::AskKind;
use super::context::{
    AgentContext, AgentCost, AgentCurrentUsage, AgentRateLimits, AgentTokenUsage, RateLimitWindow,
    WindowSource,
};
use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, ConcernCoverage, HookCoverage, IntegrationConcern,
    PlanLabel, RealtimeUsageChannel, RemoteControlCapability, ThreadKey, ToolClassification,
};
use super::lifecycle::{LifecycleSignal, LifecycleSignalKind};
use super::managed_source::ManagedSource;
use super::observation::payload_context_pct;
use super::pricing::PriceBook;
use super::{
    AgentAdapter, AgentLifecycleObservation, ClassifiedHook, HookInstallPreview, HookInstallReport,
    HookUninstallReport, Result, agent_config_path, classify_agent_hook, optional_payload_string,
    sanitize_user_prompt,
};
use crate::ids::AgentSessionId;

/// Everything `const` about Pi, in one place. See [`AgentDescriptor`] for the
/// descriptor-vs-trait split.
static PI_DESCRIPTOR: AgentDescriptor = AgentDescriptor {
    kind: "pi",
    display_name: "Pi",
    brand: Brand {
        emblem: "
 █▜███▛█
▝▜▛▀▀▀▜▛▘
 ▝▘   ▝▘",
        color: 29,
        color_rgb: (0x27, 0xa0, 0x77),
    },
    // Pi sessions span whatever provider account the user wired, so no single
    // brand prefix is honest — the tier renders bare.
    plan_label: PlanLabel::TitleCaseOnly,
    // Pi is the multi-provider client: it runs *on* other providers'
    // subscriptions rather than metering one of its own.
    sub_providers: &[],
    // Pi's built-in tool set: `edit`/`write` edit files; `bash` mutates
    // without editing, so the reasoning phase survives it.
    tools: ToolClassification {
        mutating: &["bash", "edit", "write"],
        editing: &["edit", "write"],
        blocking: &[],
    },
    capabilities: Capabilities {
        // `tool_call` is pi's awaited pre-tool gate. Pi has no native ask UI,
        // so Rimz records no ask and returns neutral.
        blocking_asks: true,
        // Pi never asks natively — no permission prompts, plan approvals, or
        // questions — so Rimz has no surface to route to. It returns neutral
        // with no waiting state.
        native_ask_ui: false,
        rich_context: false,
        transcript_tail_context: false,
        context_usage: true,
        account_spend: true,
        subagents: false,
        background_tasks: false,
        registers_lazily: false,
        daemon_hooked_sessions: false,
        hook_install: true,
        realtime_usage: RealtimeUsageChannel {
            covers_account_while_live: false,
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
    extra_bin_dirs: &[],
    // Pi's progress-proving events, in its own wire vocabulary. The blocking
    // `tool_call` is excluded like Claude's `PreToolUse`: it fires while the
    // ask is being created, so touching on it would instantly un-block the
    // row. Every *completed* tool still touches via `tool_execution_end`.
    activity_events: &[
        "session_start",
        "before_agent_start",
        "agent_end",
        "tool_execution_end",
    ],
    hook_install_unavailable: None,
    thread_key: ThreadKey::PerFile,
};

const PI_COVERAGE: &[(IntegrationConcern, ConcernCoverage)] = &[
    (
        IntegrationConcern::TurnLifecycle,
        ConcernCoverage::Wired {
            via: "session_start/before_agent_start/agent_end",
        },
    ),
    (
        IntegrationConcern::Permission,
        ConcernCoverage::Wired { via: "tool_call" },
    ),
    (
        IntegrationConcern::PlanApproval,
        ConcernCoverage::Unsupported {
            reason: "no plan-approval gate",
        },
    ),
    (
        IntegrationConcern::UserQuestion,
        ConcernCoverage::Unsupported {
            reason: "no native question tool",
        },
    ),
    (
        IntegrationConcern::Answer,
        ConcernCoverage::Unsupported {
            reason: "native prompt choreography is not mapped",
        },
    ),
    (
        IntegrationConcern::Compaction,
        ConcernCoverage::Wired {
            via: "session_before_compact/session_compact",
        },
    ),
    (
        IntegrationConcern::Subagents,
        ConcernCoverage::Unsupported {
            reason: "no subagent hook surface",
        },
    ),
    (
        IntegrationConcern::BackgroundParking,
        ConcernCoverage::Unsupported {
            reason: "no background-task parking",
        },
    ),
    (
        IntegrationConcern::SessionEnd,
        ConcernCoverage::Wired {
            via: "session_shutdown",
        },
    ),
    (
        // No idle Notification event, but the attention slice is
        // reconstructed: `agent_end` settles a finished turn to a calm state
        // and the stall window escalates a silent `running` row — Codex's
        // partial minus the `request_user_input` leg pi has no tool for.
        IntegrationConcern::IdleNotification,
        ConcernCoverage::Partial {
            via: "turn-end + stall window",
            gap: "no idle Notification hook; no idle-timeout nudge",
        },
    ),
    (
        IntegrationConcern::ContextUsage,
        ConcernCoverage::Wired {
            via: "extension context usage (row gauge + AgentContext.tokens)",
        },
    ),
    (
        IntegrationConcern::RealtimeCost,
        ConcernCoverage::Partial {
            via: "extension cumulative-cost push + turn-end session-transcript spend sum",
            gap: "in-process accumulator is best-effort and resets on resume; the turn-end walk reconciles to the authoritative session total",
        },
    ),
    (
        IntegrationConcern::RichContext,
        ConcernCoverage::Partial {
            via: "extension-envelope observe_context (model/effort/cost/account windows)",
            gap: "rides the lifecycle channel — no out-of-band transport refreshing it between turns, unlike a statusline or app-server poll",
        },
    ),
    (
        IntegrationConcern::HookInstall,
        ConcernCoverage::Wired {
            via: "~/.pi/agent/extensions/rimz.ts",
        },
    ),
    (
        IntegrationConcern::AccountSpend,
        ConcernCoverage::Wired {
            via: "auth.json/session spend + after_provider_response headers + OAuth usage probe",
        },
    ),
    (
        IntegrationConcern::RemoteControl,
        ConcernCoverage::Unsupported {
            reason: "no remote-control surface",
        },
    ),
];

const PI_LIFECYCLE_HOOKS: &[(LifecycleSignalKind, HookCoverage)] = &[
    (
        LifecycleSignalKind::Registered,
        HookCoverage::Native {
            event: "session_start",
        },
    ),
    (
        LifecycleSignalKind::TurnStarted,
        HookCoverage::Native {
            event: "before_agent_start",
        },
    ),
    (
        LifecycleSignalKind::TurnEnded,
        HookCoverage::Native { event: "agent_end" },
    ),
    (
        LifecycleSignalKind::ToolUsed,
        HookCoverage::Native {
            event: "tool_execution_end",
        },
    ),
    (
        LifecycleSignalKind::AwaitingInput,
        HookCoverage::Absent {
            reason: "pi has no native ask UI",
        },
    ),
    (
        LifecycleSignalKind::SubagentStarted,
        HookCoverage::Absent {
            reason: "pi has no subagents",
        },
    ),
    (
        LifecycleSignalKind::SubagentStopped,
        HookCoverage::Absent {
            reason: "pi has no subagents",
        },
    ),
    (
        LifecycleSignalKind::Compacting,
        HookCoverage::Native {
            event: "session_before_compact",
        },
    ),
    (
        LifecycleSignalKind::CompactionEnded,
        HookCoverage::Native {
            event: "session_compact",
        },
    ),
    (
        LifecycleSignalKind::Ended,
        HookCoverage::Native {
            event: "session_shutdown",
        },
    ),
    (
        LifecycleSignalKind::Lost,
        HookCoverage::Derived {
            via: "rimz exec wrapper",
            gap: "native hooks do not report mux-session death",
        },
    ),
];

/// The non-blocking events the embedded extension forwards — the lifecycle
/// channel, the single source of truth for classification. The model/thinking
/// selectors are enrichment-only markers: they run the context merge without
/// emitting a lifecycle signal. Mirrors the `pi.on(...)` registrations in
/// [`extension.ts`](./extension.ts) (asserted by test).
const LIFECYCLE_EVENTS: &[&str] = &[
    "session_start",
    "before_agent_start",
    "agent_end",
    "tool_execution_end",
    "model_select",
    "thinking_level_select",
    "session_before_compact",
    "session_compact",
    "session_shutdown",
];

/// Everything the extension wires, for the install/uninstall reports: the
/// lifecycle set plus the blocking `tool_call` gate.
const WIRED_EVENTS: &[&str] = &[
    "session_start",
    "before_agent_start",
    "agent_end",
    "tool_execution_end",
    "model_select",
    "thinking_level_select",
    "session_before_compact",
    "session_compact",
    "session_shutdown",
    "tool_call",
];

/// The Rimz pi extension, embedded at compile time and written whole-file on
/// install. Carries [`super::managed_source::RIMZ_MANAGED_MARKER`] on its first
/// line.
const EXTENSION_SOURCE: &str = include_str!("extension.ts");

const PI_MANAGED_SOURCE: ManagedSource = ManagedSource {
    agent: "pi",
    source: EXTENSION_SOURCE,
    wired_events: WIRED_EVENTS,
    artifact_noun: "extension",
};

#[derive(Clone, Debug, Default)]
pub struct PiAdapter;

impl AgentAdapter for PiAdapter {
    fn descriptor(&self) -> &'static AgentDescriptor {
        &PI_DESCRIPTOR
    }

    fn classify_hook(&self, event_name: &str, _payload: &Value) -> ClassifiedHook {
        // `tool_call` is pi's only blocking gate — every tool routes through
        // it, so it classifies as a permission ask. Everything else rides the
        // lifecycle channel.
        let ask_kind = (event_name == "tool_call").then_some(AskKind::Permission);
        classify_agent_hook(event_name, ask_kind, LIFECYCLE_EVENTS)
    }

    #[cfg(test)]
    fn installed_hook_events(&self) -> Vec<&'static str> {
        WIRED_EVENTS.to_vec()
    }

    #[cfg(test)]
    fn classification_corpus(&self) -> Vec<super::ClassificationSample> {
        use super::{AgentHookClass, ClassificationSample};

        vec![
            ClassificationSample::new(
                "tool_call",
                json!({ "session_id": "sess-1", "tool_name": "bash" }),
                AgentHookClass::AwaitingUser,
                Some(AskKind::Permission),
            ),
            ClassificationSample::new(
                "session_start",
                json!({ "session_id": "sess-1" }),
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
                "tool_execution_end",
                json!({ "session_id": "sess-1", "tool_name": "bash" }),
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
        // Empty stdout is the extension's allow: pi has no native prompt to
        // fall back to, so "no answer" must let the tool run.
        Ok(None)
    }

    fn observe_lifecycle(
        &self,
        event_name: &str,
        payload: &Value,
    ) -> Option<AgentLifecycleObservation> {
        let parsed = payloads::parse_payload(payload);
        // The status decision lives in the shared `lifecycle::step` table —
        // here the adapter only names the intent. The native-event → signal
        // mapping is docs/internals/agents/pi.md.
        let signal = match event_name {
            "session_start" => LifecycleSignal::Registered,
            // Pi's `agent_start`/`agent_end` bracket one user prompt — pi's
            // `agent_*` pair is what Rimz calls a turn. `before_agent_start`
            // carries the prompt.
            "before_agent_start" => LifecycleSignal::TurnStarted,
            // The last assistant message is the in-band death certificate:
            // `stopReason: "error" | "aborted"` plus `errorMessage`, no
            // transcript forensics needed. Pi has no background-task parking.
            "agent_end" => LifecycleSignal::TurnEnded {
                errored: payloads::agent_end_errored(&parsed),
                parked_on_background: false,
            },
            // Only a *mutating* tool rides the lifecycle channel: it is proof
            // of real work (read-only tools stay silent). The `edits` bit
            // marks the file-writing subset, which ends the turn's thinking
            // head.
            "tool_execution_end" if self.descriptor().tool_mutates(payload) => {
                LifecycleSignal::ToolUsed {
                    mutates: true,
                    edits: self.descriptor().tool_edits_files(payload),
                }
            }
            // A leading signal, like Claude's `PreCompact`.
            "session_before_compact" => LifecycleSignal::Compacting,
            // Pi's extension hook reports no manual/auto trigger, so this
            // only clears the transient head and preserves the prior state.
            "session_compact" => LifecycleSignal::CompactionEnded { auto: None },
            // Fires on quit including Ctrl+C/SIGHUP/SIGTERM and on every
            // session replacement (`/new`, `/resume`) — a true session end.
            "session_shutdown" => LifecycleSignal::Ended,
            _ => return None,
        };
        // No subagents: every pi event keys on its own session id, no parent
        // link, no quarantine path.
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

    fn ends_session(&self, event_name: &str) -> bool {
        // The extension skips the `/reload` shutdown (the same session id
        // re-registers in place, and a fire-and-forget tombstone would race
        // it), so every shutdown that arrives here is a real end: quit, or a
        // new/resume/fork replacing this session.
        event_name == "session_shutdown"
    }

    fn moves_on(&self, event_name: &str) -> bool {
        // A new prompt starts a fresh turn; an agent_end completes the current
        // one. Pi raises no native asks today, so this only future-proofs
        // enrichment-sourced ask handling.
        matches!(event_name, "before_agent_start" | "agent_end")
    }

    fn transcript_files(&self) -> Vec<PathBuf> {
        spend::pi_session_files()
    }

    fn session_transcript(&self, session_id: &str, prior_path: Option<&Path>) -> Option<PathBuf> {
        if let Some(path) = prior_path.filter(|path| path.is_file()) {
            return Some(path.to_path_buf());
        }
        let session_id = session_id.trim();
        if session_id.is_empty() {
            return None;
        }
        self.transcript_files().into_iter().find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains(session_id))
        })
    }

    /// Pi logs `costUSD` directly, so the price book is unused. The resume
    /// cursor carries the session header cwd so appended usage entries retain
    /// their workspace origin.
    fn parse_spend(
        &self,
        path: &Path,
        resume: Option<&crate::agents::spending::SpendCursor>,
        _prices: &PriceBook,
    ) -> crate::agents::spending::SpendParse {
        spend::parse_pi_spend(path, resume)
    }

    /// `pi --session <id>` resolves the session (a partial UUID suffices) and
    /// restores it interactively; the launching pane sets the cwd. The
    /// extension re-fires `session_start` with `reason: "resume"`.
    fn resume_command(&self, session_id: &str, _cwd: &Path) -> Option<Vec<String>> {
        Some(vec![
            "pi".to_owned(),
            "--session".to_owned(),
            session_id.to_owned(),
        ])
    }

    fn compact_command(&self) -> Option<&'static str> {
        Some("/compact")
    }

    fn render_preset(
        &self,
        preset: &super::LaunchPreset,
    ) -> std::result::Result<Vec<String>, super::PresetErr> {
        let mut argv = Vec::new();
        if let Some(model) = preset.model.as_deref().filter(|model| !model.is_empty()) {
            argv.extend(["--model".to_owned(), model.to_owned()]);
        }
        if let Some(effort) = preset.effort.as_deref().filter(|effort| !effort.is_empty()) {
            argv.extend(["--thinking".to_owned(), effort.to_owned()]);
        }
        if preset.system_prompt_file.is_some() {
            return Err(super::PresetErr::UnsupportedField {
                agent: self.descriptor().kind,
                field: "system-prompt-file",
            });
        }
        if preset.append_system_prompt_file.is_some() {
            return Err(super::PresetErr::UnsupportedField {
                agent: self.descriptor().kind,
                field: "append-system-prompt-file",
            });
        }
        Ok(argv)
    }

    fn launch_command(&self, extra_args: &[String], prompt: Option<&str>) -> Option<Vec<String>> {
        Some(super::positional_prompt_argv("pi", extra_args, prompt))
    }

    fn install_hooks(&self) -> Result<HookInstallReport> {
        let path = pi_extension_path()?;
        PI_MANAGED_SOURCE.install_into(&path)
    }

    fn preview_hook_install(&self) -> Result<HookInstallPreview> {
        let path = pi_extension_path()?;
        PI_MANAGED_SOURCE.preview_at(&path)
    }

    fn uninstall_hooks(&self) -> Result<HookUninstallReport> {
        let path = pi_extension_path()?;
        PI_MANAGED_SOURCE.uninstall_from(&path)
    }

    fn hooks_installed(&self) -> bool {
        pi_extension_path().is_ok_and(|path| PI_MANAGED_SOURCE.installed_at(&path))
    }

    fn managed_hook_artifacts_present(&self) -> bool {
        self.hooks_installed()
    }

    fn probe_account(&self) -> crate::agents::account::AccountProbe {
        account::probe()
    }

    fn probe_oauth_usage(&self) -> crate::agents::OauthUsageProbe {
        crate::agents::credits::map_probe_snapshot(oauth_usage::fetch(), "pi.oauth_usage")
    }

    fn oauth_credentials_stamp(&self) -> Option<u64> {
        oauth_usage::credentials_stamp()
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
            current_usage,
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
    let windows: Vec<RateLimitWindow> = payload
        .get("rate_limits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(parse_rate_limit_window)
        .collect();
    let rate_limits = (!windows.is_empty()).then_some(AgentRateLimits { windows });
    if parsed.model.is_none()
        && parsed.effort.is_none()
        && tokens.is_none()
        && cost.is_none()
        && rate_limits.is_none()
    {
        return None;
    }
    Some(AgentContext {
        source: source.to_owned(),
        session_name: None,
        session_preview: None,
        model_id: parsed.model,
        model_display_name: None,
        effort: parsed.effort,
        thinking_enabled: None,
        output_style: None,
        vim_mode: None,
        agent_version: None,
        exceeds_200k_tokens: None,
        cost,
        tokens,
        rate_limits,
        pr: None,
        account: None,
        turn_error: None,
        turn_complete: None,
        turn_interrupted: None,
        observed_at: Timestamp::now(),
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

fn parse_rate_limit_window(value: &Value) -> Option<RateLimitWindow> {
    let used_percentage = value
        .get("used_percentage")
        .or_else(|| value.get("usedPercent"))
        .and_then(value_f64)
        .map(|value| value.round().clamp(0.0, 100.0) as u8);
    let resets_at = value
        .get("resets_at")
        .or_else(|| value.get("resetsAt"))
        .and_then(timestamp_from_value);
    let duration_mins = value
        .get("duration_mins")
        .or_else(|| value.get("durationMins"))
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok());
    let observed_at = value
        .get("observed_at")
        .or_else(|| value.get("observedAt"))
        .and_then(timestamp_from_value);
    (used_percentage.is_some() || resets_at.is_some() || duration_mins.is_some()).then_some(
        RateLimitWindow {
            used_percentage,
            resets_at,
            duration_mins,
            observed_at,
            source: WindowSource::BestEffort,
        },
    )
}

fn value_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(raw) => raw.trim().parse::<f64>().ok(),
        _ => None,
    }
    .filter(|value| value.is_finite())
}

fn timestamp_from_value(value: &Value) -> Option<Timestamp> {
    match value {
        Value::Number(number) => number
            .as_i64()
            .and_then(|secs| Timestamp::from_second(secs).ok()),
        Value::String(raw) => raw.parse::<Timestamp>().ok().or_else(|| {
            raw.trim()
                .parse::<i64>()
                .ok()
                .and_then(|secs| Timestamp::from_second(secs).ok())
        }),
        _ => None,
    }
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
