//! Factory Droid native-hook adapter.
//!
//! Droid's stock hooks expose basic session, turn, tool, and compaction
//! lifecycle. Its version-pinned private session files add current/cumulative
//! usage, model, effort, and native-question enrichment; the wire still carries
//! no durable asks, error outcome, subagent identity, or account surface.

mod config;
mod install;
mod payloads;
mod process;
mod spend;
mod transcript;

pub use self::process::{HookProcessDisposition, hook_process_disposition};

use std::path::{Path, PathBuf};

use serde_json::Value;

use self::install::{MANAGED_SOURCE, droid_settings_path};
use self::payloads::{parse_session_start, parse_user_prompt_submit};
#[cfg(test)]
use super::AgentHookClass;
use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, ConcernCoverage, HookCoverage, IntegrationCoverage,
    LifecycleCoverage, PlanLabel, RealtimeUsageChannel, RemoteControlCapability, ThreadKey,
    ToolClassification,
};
use super::hook_types::{HookRecord, SessionSource, classify_catalog_hook, hook_record};
use super::lifecycle::LifecycleSignal;
use super::{
    AgentAdapter, AgentLifecycleObservation, AgentTokenUsage, ClassifiedHook, LocalContextRefresh,
    LocalContextRefreshCtx, ManagedSource, RefreshTrigger, Result, SessionOrigin,
    TranscriptMessage, TranscriptPage, TranscriptPosition, optional_payload_string,
    read_transcript_lines, sanitize_user_prompt,
};
#[cfg(test)]
use crate::harness::run::PermissionMode;
use crate::ids::AgentSessionId;

static DROID_DESCRIPTOR: AgentDescriptor = AgentDescriptor {
    kind: "droid",
    display_name: "Droid",
    brand: Brand {
        emblem: None,
        color: 252,
        color_rgb: (0xd8, 0xd8, 0xd8),
    },
    plan_label: PlanLabel::TitleCaseOnly,
    sub_providers: &[],
    expected_windows: &[],
    tools: ToolClassification {
        mutating: &["Create", "Edit", "ApplyPatch", "Execute"],
        editing: &["Create", "Edit", "ApplyPatch"],
        blocking: &[],
    },
    capabilities: Capabilities {
        // Droid renders native permission prompts, but its hooks expose no
        // structured prompt event RimZ can route or answer.
        native_ask_ui: true,
        transcript_tail_context: true,
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
    coverage: DROID_COVERAGE,
    lifecycle_hooks: DROID_LIFECYCLE_HOOKS,
    default_context_window: None,
    default_model: None,
    process_names: &["droid"],
    bin_names: &["droid"],
    extra_bin_dirs: &[],
    activity_events: &["SessionStart", "UserPromptSubmit", "PostToolUse", "Stop"],
    thread_key: ThreadKey::PerFile,
    launch: super::LaunchSpec {
        program: Some("droid"),
        fixed_args: &[],
        prompt: super::PromptStyle::PositionalAfterDoubleDash,
        resume: Some(super::SessionCommand {
            before_id: &["droid", "--resume"],
            after_id: &[],
        }),
        fork: Some(super::SessionCommand {
            before_id: &["droid", "--fork"],
            after_id: &[],
        }),
        permission: super::LaunchPermissionArgs {
            ask: &[],
            auto: &["--auto", "medium"],
            yolo: &[],
            plan: &["--use-spec"],
        },
        ping_args: None,
        max_turn_flag: None,
        compact_command: Some("/compact"),
        presets: super::PresetMatchers {
            append_system_prompt_file: Some(super::StaticPresetMatcher::Flag(&[
                "--append-system-prompt-file",
            ])),
            ..super::PresetMatchers::EMPTY
        },
    },
};

const DROID_COVERAGE: IntegrationCoverage = IntegrationCoverage {
    turn_lifecycle: ConcernCoverage::Wired {
        via: "SessionStart/UserPromptSubmit/Stop",
    },
    permission: ConcernCoverage::Unsupported {
        reason: "no PermissionRequest hook or structured Notification discriminator",
    },
    plan_approval: ConcernCoverage::Unsupported {
        reason: "no plan-approval hook; spec-mode exit is invisible",
    },
    user_question: ConcernCoverage::Partial {
        via: "v2 transcript AskUser tool calls project a native waiting card",
        gap: "there is no durable RimZ ask or out-of-band answer API",
    },
    answer: ConcernCoverage::Unsupported {
        reason: "native prompt choreography is not mapped",
    },
    compaction: ConcernCoverage::Wired {
        via: "PreCompact/SessionStart:compact",
    },
    subagents: ConcernCoverage::Unsupported {
        reason: "SubagentStop carries no child identity",
    },
    background_parking: ConcernCoverage::Unsupported {
        reason: "no background-task parking",
    },
    session_end: ConcernCoverage::Wired { via: "SessionEnd" },
    idle_notification: ConcernCoverage::Wired {
        via: "Notification",
    },
    context_usage: ConcernCoverage::Wired {
        via: "v2 session settings lastCallTokenUsage/tokenUsage plus exact configured/model capacity",
    },
    realtime_cost: ConcernCoverage::Partial {
        via: "v2 session tokenUsage priced through an exact canonical model id",
        gap: "local USD is estimated rather than authoritative provider billing",
    },
    rich_context: ConcernCoverage::Partial {
        via: "v2 session settings plus typed custom-model configuration and current-call usage",
        gap: "no authoritative USD, account, or quota metadata",
    },
    hook_install: ConcernCoverage::Wired {
        via: "~/.factory/settings.json",
    },
    account_spend: ConcernCoverage::Unsupported {
        reason: "no machine-readable auth or usage surface",
    },
    remote_control: ConcernCoverage::Unsupported {
        reason: "no remote-control surface",
    },
};

const DROID_LIFECYCLE_HOOKS: LifecycleCoverage = LifecycleCoverage {
    registered: HookCoverage::Native {
        event: "SessionStart",
    },
    turn_started: HookCoverage::Native {
        event: "UserPromptSubmit",
    },
    turn_ended: HookCoverage::Native { event: "Stop" },
    tool_used: HookCoverage::Native {
        event: "PostToolUse",
    },
    awaiting_input: HookCoverage::Derived {
        via: "v2 transcript AskUser tool call",
        gap: "no hook or durable ask record",
    },
    subagent_started: HookCoverage::Absent {
        reason: "no child identity",
    },
    subagent_stopped: HookCoverage::Absent {
        reason: "no child identity",
    },
    compacting: HookCoverage::Native {
        event: "PreCompact",
    },
    compaction_ended: HookCoverage::Native {
        event: "SessionStart",
    },
    ended: HookCoverage::Native {
        event: "SessionEnd",
    },
    lost: HookCoverage::Derived {
        via: "rimz exec wrapper",
        gap: "native hooks do not report mux-session death",
    },
};

const DROID_HOOK_TIMEOUT_SECS: u64 = 10;
const DROID_HOOKS: &[HookRecord] = &[
    hook_record!(lifecycle, "SessionStart", r#"{"session_id":"sess-1"}"#),
    hook_record!(lifecycle, "UserPromptSubmit", r#"{"session_id":"sess-1"}"#),
    hook_record!(lifecycle, "PostToolUse", r#"{"session_id":"sess-1"}"#),
    hook_record!(lifecycle, "Notification", r#"{"session_id":"sess-1"}"#),
    hook_record!(lifecycle, "Stop", r#"{"session_id":"sess-1"}"#),
    hook_record!(lifecycle, "PreCompact", r#"{"session_id":"sess-1"}"#),
    hook_record!(lifecycle, "SessionEnd", r#"{"session_id":"sess-1"}"#),
];
const RIMZ_HOOK_COMMAND: &str = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source droid";
const RIMZ_HOOK_MARKER: &str = "rimz hooks feed --source droid";

#[derive(Clone, Debug, Default)]
pub struct DroidAdapter;

impl AgentAdapter for DroidAdapter {
    fn descriptor(&self) -> &'static AgentDescriptor {
        &DROID_DESCRIPTOR
    }

    fn classify_hook(&self, event_name: &str, _payload: &Value) -> ClassifiedHook {
        classify_catalog_hook(DROID_HOOKS, event_name, None)
    }

    #[cfg(test)]
    fn native_hook_events(&self) -> Vec<&'static str> {
        super::hook_types::catalog_event_names(DROID_HOOKS)
    }

    #[cfg(test)]
    fn classification_corpus(&self) -> Vec<super::ClassificationSample> {
        let mut samples = super::hook_types::catalog_classification_corpus(DROID_HOOKS);
        samples.push(super::ClassificationSample::new(
            "SessionStart",
            serde_json::json!({"session_id": "sess-1", "source": "compact"}),
            AgentHookClass::Lifecycle,
            None,
        ));
        samples
    }

    #[cfg(test)]
    fn spend_fixture(&self) -> Option<super::SpendFixture> {
        Some(super::SpendFixture {
            session_id: "droid-conformance",
            file_name: "droid-conformance.settings.json",
            body: super::SpendFixtureBody::Jsonl(
                r#"{"model":"gpt-5","tokenUsage":{"inputTokens":100,"outputTokens":20,"cacheCreationTokens":10,"cacheReadTokens":30,"thinkingTokens":5}}"#,
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
        let session_start = (event_name == "SessionStart").then(|| parse_session_start(payload));
        let signal = match event_name {
            "SessionStart" => match session_start.as_ref()?.source {
                SessionSource::Compact => LifecycleSignal::CompactionEnded { auto: None },
                _ => LifecycleSignal::Registered,
            },
            "UserPromptSubmit" => LifecycleSignal::TurnStarted,
            "PostToolUse" => LifecycleSignal::ToolUsed {
                mutates: self.descriptor().tool_mutates(payload),
                edits: self.descriptor().tool_edits_files(payload),
                native_key: None,
            },
            // Droid has no structured failure hook. Display status and the
            // stall window surface failures without guessing from silence.
            "Stop" => LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
            "PreCompact" => LifecycleSignal::Compacting,
            "SessionEnd" => LifecycleSignal::Ended,
            _ => return None,
        };
        let agent_id = optional_payload_string(payload, &["session_id"]).map(AgentSessionId::from);
        let mut observation =
            AgentLifecycleObservation::new(agent_id, signal).with_worktree_from_payload(payload);
        if event_name == "UserPromptSubmit" {
            let prompt = parse_user_prompt_submit(payload).prompt;
            observation.task = sanitize_user_prompt(prompt.as_deref());
            observation.prompt = sanitize_user_prompt(prompt.as_deref());
        }
        observation.transcript_path = optional_payload_string(payload, &["transcript_path"]);
        if let Some(path) = observation.transcript_path.as_deref() {
            let (model, effort) = transcript::identity(Path::new(path));
            observation.launch.model = model;
            observation.launch.effort = effort;
        }
        if matches!(observation.signal, LifecycleSignal::Registered)
            && session_start.as_ref().is_some_and(|start| {
                matches!(start.source, SessionSource::Startup | SessionSource::Clear)
            })
        {
            observation.origin = Some(SessionOrigin::Fresh);
        }
        Some(observation)
    }

    fn last_assistant_message(
        &self,
        event_name: &str,
        _payload: &Value,
        observation: &AgentLifecycleObservation,
    ) -> Option<String> {
        (event_name == "Stop")
            .then_some(observation.transcript_path.as_deref())
            .flatten()
            .and_then(|path| transcript::last_assistant_message(Path::new(path)))
    }

    fn parse_transcript_messages(&self, lines: &str) -> Vec<TranscriptMessage> {
        transcript::parse_messages(lines)
    }

    fn stream_assistant_messages(&self, new_lines: &str) -> Vec<String> {
        transcript::parse_assistant_suffix(new_lines)
    }

    fn transcript_position(
        &self,
        path: &Path,
        _session_id: Option<&AgentSessionId>,
    ) -> Option<TranscriptPosition> {
        transcript::supported_file(path)
            .then(|| std::fs::metadata(path).ok())
            .flatten()
            .map(|meta| TranscriptPosition::new(meta.len()))
    }

    fn read_assistant_transcript_page(
        &self,
        path: &Path,
        _session_id: Option<&AgentSessionId>,
        position: TranscriptPosition,
    ) -> Option<TranscriptPage> {
        if !transcript::supported_file(path) {
            return None;
        }
        let (bytes, next) = read_transcript_lines(path, position.get())?;
        Some(TranscriptPage {
            next: TranscriptPosition::new(next),
            messages: transcript::parse_assistant_suffix(&String::from_utf8_lossy(&bytes)),
        })
    }

    fn local_context_refresh(
        &self,
        _trigger: RefreshTrigger<'_>,
        ctx: &LocalContextRefreshCtx<'_>,
    ) -> Option<LocalContextRefresh> {
        let source = ctx
            .current_transcript_path
            .or(ctx.prior_transcript_path)
            .map(Path::new)?;
        let refresh = transcript::telemetry(source, ctx.prior_transcript_stat)?;
        let raw_model = refresh.telemetry.model.as_deref().map(str::trim);
        let resolved = raw_model
            .filter(|model| model.starts_with("custom:"))
            .and_then(|model| {
                let user_settings = droid_settings_path().ok()?;
                config::resolve_custom_model(model, &refresh.settings_path, &user_settings)
            });
        let model_id = match raw_model {
            Some(model) if !model.starts_with("custom:") && !model.is_empty() => {
                Some(model.to_owned())
            }
            _ => resolved.as_ref().map(|model| model.model_id.clone()),
        };
        let model_display_name = resolved
            .as_ref()
            .map(|model| model.display_name.clone())
            .or_else(|| raw_model.and_then(super::model_display::display_factory_custom_selector));
        let prices = super::pricing::cached_book(ctx.shared_pricing_cache_path);
        let context_window_size = resolved
            .as_ref()
            .and_then(|model| model.max_context_limit)
            .or_else(|| {
                model_id
                    .as_deref()
                    .and_then(|model| prices.exact_price(model))
                    .and_then(|price| price.max_input_tokens)
            });
        let has_tokens = context_window_size.is_some()
            || refresh.telemetry.current_usage.is_some()
            || refresh.telemetry.session_usage.is_some();
        let tokens = has_tokens.then_some(AgentTokenUsage {
            context_window_size,
            used_percentage: None,
            remaining_percentage: None,
            current_context_tokens: None,
            current_usage: refresh.telemetry.current_usage,
            session_usage: refresh.telemetry.session_usage,
        });
        let cost =
            super::spending::session_cost_usd(self, ctx.agent_id, &refresh.settings_path, &prices);
        Some(LocalContextRefresh {
            model_id,
            model_display_name,
            effort: refresh.telemetry.reasoning_effort,
            tokens,
            cost,
            native_permission_wait: refresh.telemetry.native_permission_wait,
            transcript_path: Some(refresh.transcript_path.to_string_lossy().into_owned()),
            transcript_stat: Some(refresh.stat),
            ..LocalContextRefresh::default()
        })
    }

    fn session_transcript(&self, _session_id: &str, prior_path: Option<&Path>) -> Option<PathBuf> {
        let prior = prior_path?;
        let settings = transcript::settings_path(prior)?;
        settings.is_file().then_some(settings)
    }

    fn parse_spend(
        &self,
        path: &Path,
        _resume: Option<&super::spending::SpendCursor>,
        prices: &super::PriceBook,
    ) -> super::spending::SpendParse {
        spend::parse(path, prices)
    }

    fn managed_source(&self) -> Option<&'static ManagedSource> {
        Some(&MANAGED_SOURCE)
    }
}

#[cfg(test)]
mod tests;
