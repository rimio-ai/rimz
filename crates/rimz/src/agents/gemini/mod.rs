//! Gemini CLI hook adapter.
//!
//! Gemini runs user-global command hooks as children of the interactive pane.
//! Eight native events register the session, bracket turns and compaction,
//! observe native asks, and prove completed mutating tools. Project-scoped
//! session JSONL supplies the live model/context gauge and token-priced spend.

mod account;
mod install;
mod payloads;
mod spend;

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::Value;
use serde_json::json;

use super::context::{AgentCost, AgentCurrentUsage, AgentTokenUsage};
use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, ConcernCoverage, HookCoverage, IntegrationConcern,
    PlanLabel, RealtimeUsageChannel, RemoteControlCapability, ThreadKey, ToolClassification,
};
use super::lifecycle::{AskKind, LifecycleSignal, LifecycleSignalKind};
use super::pricing;
use super::{
    AgentAdapter, AgentLifecycleObservation, ClassifiedHook, HookInstallPreview, HookInstallReport,
    HookUninstallReport, LocalContextRefresh, LocalContextRefreshCtx, RefreshTrigger, Result,
    SessionOrigin, TranscriptStat, classify_agent_hook, non_empty_trimmed, sanitize_user_prompt,
};
use crate::harness::run::PermissionMode;
use crate::ids::AgentSessionId;
use crate::transcript::{AskOption, AskQuestion};

const GEMINI_CONTEXT_WINDOW: u64 = 1_048_576;
const GEMMA_CONTEXT_WINDOW: u64 = 256_000;

pub(super) const INSTALLED_EVENTS: &[&str] = &[
    "SessionStart",
    "SessionEnd",
    "BeforeAgent",
    "AfterAgent",
    "BeforeTool",
    "AfterTool",
    "Notification",
    "PreCompress",
];

const LIFECYCLE_EVENTS: &[&str] = INSTALLED_EVENTS;
pub(super) const RIMZ_HOOK_COMMAND: &str =
    "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source gemini";

static GEMINI_DESCRIPTOR: AgentDescriptor = AgentDescriptor {
    kind: "gemini",
    display_name: "Gemini",
    brand: Brand {
        emblem: None,
        color: 33,
        color_rgb: (0x42, 0x85, 0xf4),
    },
    plan_label: PlanLabel::Prefixed { prefix: "Gemini" },
    sub_providers: &["google"],
    tools: ToolClassification {
        mutating: &["write_file", "replace", "run_shell_command"],
        editing: &["write_file", "replace"],
        blocking: &[
            ("ask_user", AskKind::Question),
            ("exit_plan_mode", AskKind::PlanApproval),
        ],
    },
    capabilities: Capabilities {
        blocking_asks: true,
        native_ask_ui: true,
        rich_context: false,
        transcript_tail_context: true,
        context_usage: true,
        account_spend: true,
        subagents: false,
        background_tasks: false,
        registers_lazily: false,
        daemon_hooked_sessions: false,
        hook_install: true,
        implicit_unlimited_window_mins: &[],
        realtime_usage: RealtimeUsageChannel {
            covers_account_while_live: false,
            windows_defer_to_fresh_realtime: false,
        },
        remote_control: RemoteControlCapability {
            pane_sessions: false,
            background_sessions: false,
        },
    },
    coverage: GEMINI_COVERAGE,
    lifecycle_hooks: GEMINI_LIFECYCLE_HOOKS,
    default_context_window: Some(GEMINI_CONTEXT_WINDOW),
    default_model: None,
    process_names: &["gemini", "node"],
    bin_names: &["gemini"],
    extra_bin_dirs: &[],
    activity_events: &["SessionStart", "BeforeAgent", "AfterAgent", "AfterTool"],
    hook_install_unavailable: None,
    thread_key: ThreadKey::PerFile,
};

const GEMINI_COVERAGE: &[(IntegrationConcern, ConcernCoverage)] = &[
    (
        IntegrationConcern::TurnLifecycle,
        ConcernCoverage::Wired {
            via: "BeforeAgent/AfterAgent",
        },
    ),
    (
        IntegrationConcern::Permission,
        ConcernCoverage::Wired {
            via: "Notification(ToolPermission)",
        },
    ),
    (
        IntegrationConcern::PlanApproval,
        ConcernCoverage::Wired {
            via: "BeforeTool exit_plan_mode",
        },
    ),
    (
        IntegrationConcern::UserQuestion,
        ConcernCoverage::Wired {
            via: "BeforeTool ask_user",
        },
    ),
    (
        IntegrationConcern::Answer,
        ConcernCoverage::Unsupported {
            reason: "native TUI answer choreography is not mapped",
        },
    ),
    (
        IntegrationConcern::Compaction,
        ConcernCoverage::Partial {
            via: "PreCompress + next lifecycle signal",
            gap: "no explicit post-compress event",
        },
    ),
    (
        IntegrationConcern::Subagents,
        ConcernCoverage::Unsupported {
            reason: "child hook behavior is not live-verified",
        },
    ),
    (
        IntegrationConcern::BackgroundParking,
        ConcernCoverage::Unsupported {
            reason: "no background-task parking signal",
        },
    ),
    (
        IntegrationConcern::SessionEnd,
        ConcernCoverage::Wired {
            via: "best-effort asynchronous SessionEnd; pane liveness is the backstop",
        },
    ),
    (
        IntegrationConcern::IdleNotification,
        ConcernCoverage::Partial {
            via: "turn boundaries + native asks + stall window",
            gap: "no idle-timeout notification",
        },
    ),
    (
        IntegrationConcern::ContextUsage,
        ConcernCoverage::Wired {
            via: "session transcript tail",
        },
    ),
    (
        IntegrationConcern::RealtimeCost,
        ConcernCoverage::Partial {
            via: "turn-end transcript tail with priced tokens",
            gap: "reconstructed on turn end, not provider-pushed",
        },
    ),
    (
        IntegrationConcern::RichContext,
        ConcernCoverage::Unsupported {
            reason: "no provider-owned rich context channel",
        },
    ),
    (
        IntegrationConcern::HookInstall,
        ConcernCoverage::Wired {
            via: "~/.gemini/settings.json",
        },
    ),
    (
        IntegrationConcern::AccountSpend,
        ConcernCoverage::Partial {
            via: "local auth identity + transcript spend",
            gap: "Code Assist quota probe is deferred",
        },
    ),
    (
        IntegrationConcern::RemoteControl,
        ConcernCoverage::Unsupported {
            reason: "ACP does not observe an existing TUI session",
        },
    ),
];

const GEMINI_LIFECYCLE_HOOKS: &[(LifecycleSignalKind, HookCoverage)] = &[
    (
        LifecycleSignalKind::Registered,
        HookCoverage::Native {
            event: "SessionStart",
        },
    ),
    (
        LifecycleSignalKind::TurnStarted,
        HookCoverage::Native {
            event: "BeforeAgent",
        },
    ),
    (
        LifecycleSignalKind::TurnEnded,
        HookCoverage::Native {
            event: "AfterAgent",
        },
    ),
    (
        LifecycleSignalKind::ToolUsed,
        HookCoverage::Native { event: "AfterTool" },
    ),
    (
        LifecycleSignalKind::AwaitingInput,
        HookCoverage::Native {
            event: "Notification",
        },
    ),
    (
        LifecycleSignalKind::SubagentStarted,
        HookCoverage::Absent {
            reason: "no verified child lifecycle hook",
        },
    ),
    (
        LifecycleSignalKind::SubagentStopped,
        HookCoverage::Absent {
            reason: "no verified child lifecycle hook",
        },
    ),
    (
        LifecycleSignalKind::Compacting,
        HookCoverage::Native {
            event: "PreCompress",
        },
    ),
    (
        LifecycleSignalKind::CompactionEnded,
        HookCoverage::Derived {
            via: "next lifecycle signal closes the bracket",
            gap: "no post-compress event; landing follows the next signal, not the trigger",
        },
    ),
    (
        LifecycleSignalKind::Ended,
        HookCoverage::Native {
            event: "SessionEnd",
        },
    ),
    (
        LifecycleSignalKind::Lost,
        HookCoverage::Derived {
            via: "rimz exec wrapper and pane liveness",
            gap: "native hooks do not report mux-session death",
        },
    ),
];

#[derive(Clone, Debug, Default)]
pub struct GeminiAdapter;

impl AgentAdapter for GeminiAdapter {
    fn descriptor(&self) -> &'static AgentDescriptor {
        &GEMINI_DESCRIPTOR
    }

    fn classify_hook(&self, event_name: &str, payload: &Value) -> ClassifiedHook {
        let parsed = payloads::parse_hook(payload);
        let has_session = parsed
            .session_id
            .as_deref()
            .is_some_and(|session_id| !session_id.trim().is_empty());
        let ask_kind = match event_name {
            "Notification"
                if has_session
                    && parsed.notification_type.as_deref() == Some("ToolPermission")
                    && !matches!(
                        notification_ask_kind(parsed.details.as_ref()),
                        AskKind::Question | AskKind::PlanApproval
                    ) =>
            {
                Some(AskKind::Permission)
            }
            "BeforeTool" => self
                .descriptor()
                .blocking_tool_kind(parsed.tool_name.as_deref())
                .filter(|_| has_session),
            _ => None,
        };
        classify_agent_hook(event_name, ask_kind, LIFECYCLE_EVENTS)
    }

    #[cfg(test)]
    fn installed_hook_events(&self) -> Vec<&'static str> {
        INSTALLED_EVENTS.to_vec()
    }

    #[cfg(test)]
    fn classification_corpus(&self) -> Vec<super::ClassificationSample> {
        use super::{AgentHookClass, ClassificationSample};
        vec![
            ClassificationSample::new(
                "SessionStart",
                json!({"session_id":"sess-1","source":"startup"}),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "SessionEnd",
                json!({"session_id":"sess-1","reason":"exit"}),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "BeforeAgent",
                json!({"session_id":"sess-1","prompt":"fix auth"}),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "AfterAgent",
                json!({"session_id":"sess-1","prompt_response":"done"}),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "BeforeTool",
                json!({"session_id":"sess-1","tool_name":"read_file"}),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "BeforeTool",
                json!({"session_id":"sess-1","tool_name":"ask_user"}),
                AgentHookClass::AwaitingUser,
                Some(AskKind::Question),
            ),
            ClassificationSample::new(
                "BeforeTool",
                json!({"session_id":"sess-1","tool_name":"exit_plan_mode"}),
                AgentHookClass::AwaitingUser,
                Some(AskKind::PlanApproval),
            ),
            ClassificationSample::new(
                "AfterTool",
                json!({"session_id":"sess-1","tool_name":"write_file"}),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "Notification",
                json!({"session_id":"sess-1","notification_type":"ToolPermission","details":{"type":"exec"}}),
                AgentHookClass::AwaitingUser,
                Some(AskKind::Permission),
            ),
            ClassificationSample::new(
                "Notification",
                json!({"session_id":"sess-1","notification_type":"ToolPermission","details":{"type":"edit"}}),
                AgentHookClass::AwaitingUser,
                Some(AskKind::Permission),
            ),
            ClassificationSample::new(
                "Notification",
                json!({"session_id":"sess-1","notification_type":"ToolPermission","details":{"type":"ask_user"}}),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "PreCompress",
                json!({"session_id":"sess-1","trigger":"manual"}),
                AgentHookClass::Lifecycle,
                None,
            ),
        ]
    }

    #[cfg(test)]
    fn spend_fixture(&self) -> Option<super::SpendFixture> {
        Some(super::SpendFixture {
            session_id: "sess-1",
            file_name: "session-2026-06-02T10-00-sess-1.jsonl",
            body: super::SpendFixtureBody::Jsonl(
                r#"{"sessionId":"sess-1"}
{"id":"m1","timestamp":"2026-06-02T10:00:00Z","type":"gemini","model":"gemini-3-pro-preview","tokens":{"input":100,"output":50,"cached":20,"thoughts":10,"total":160}}"#,
            ),
        })
    }

    fn render_neutral(&self, event_name: &str) -> Result<Option<Value>> {
        Ok(INSTALLED_EVENTS.contains(&event_name).then(|| json!({})))
    }

    fn observe_lifecycle(
        &self,
        event_name: &str,
        payload: &Value,
    ) -> Option<AgentLifecycleObservation> {
        let parsed = payloads::parse_hook(payload);
        let agent_id = parsed
            .session_id
            .as_deref()
            .map(str::trim)
            .filter(|session_id| !session_id.is_empty())
            .map(AgentSessionId::from)?;
        let signal = match event_name {
            "SessionStart" => LifecycleSignal::Registered,
            "BeforeAgent" => LifecycleSignal::TurnStarted,
            "AfterAgent" => LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
            "BeforeTool" => LifecycleSignal::AwaitingInput {
                kind: self
                    .descriptor()
                    .blocking_tool_kind(parsed.tool_name.as_deref())?,
                ask_id: None,
                detail: None,
            },
            "Notification"
                if parsed.notification_type.as_deref() == Some("ToolPermission")
                    && !matches!(
                        notification_ask_kind(parsed.details.as_ref()),
                        AskKind::Question | AskKind::PlanApproval
                    ) =>
            {
                LifecycleSignal::AwaitingInput {
                    kind: AskKind::Permission,
                    ask_id: None,
                    detail: None,
                }
            }
            "AfterTool" => LifecycleSignal::ToolUsed {
                mutates: self.descriptor().tool_mutates(payload),
                edits: self.descriptor().tool_edits_files(payload),
            },
            "PreCompress" => LifecycleSignal::Compacting,
            "SessionEnd" => LifecycleSignal::Ended,
            _ => return None,
        };
        let mut observation = AgentLifecycleObservation::new(Some(agent_id), signal)
            .with_worktree_from_payload(payload);
        let prompt = sanitize_user_prompt(parsed.prompt.as_deref());
        observation.prompt = prompt.clone();
        observation.task = prompt;
        observation.transcript_path = parsed.transcript_path.filter(|path| !path.is_empty());
        if event_name == "SessionStart"
            && matches!(parsed.source.as_deref(), Some("startup" | "clear"))
        {
            observation.origin = Some(SessionOrigin::Fresh);
        }
        if event_name == "AfterAgent"
            && let Some(snapshot) = observation
                .transcript_path
                .as_deref()
                .and_then(|path| transcript_snapshot(Path::new(path)))
        {
            apply_snapshot_to_observation(&mut observation, &snapshot);
        }
        Some(observation)
    }

    fn last_assistant_message(
        &self,
        event_name: &str,
        payload: &Value,
        _observation: &AgentLifecycleObservation,
    ) -> Option<String> {
        (event_name == "AfterAgent")
            .then(|| payloads::parse_hook(payload).prompt_response)
            .flatten()
            .as_deref()
            .and_then(non_empty_trimmed)
    }

    fn ask_question_detail(&self, event_name: &str, payload: &Value) -> Option<Vec<AskQuestion>> {
        if event_name != "BeforeTool" {
            return None;
        }
        let parsed = payloads::parse_hook(payload);
        if parsed.tool_name.as_deref() != Some("ask_user") {
            return None;
        }
        let questions = parsed.tool_input?.questions?;
        let questions: Vec<_> = questions
            .into_iter()
            .filter_map(|question| {
                let text = question.question?.trim().to_owned();
                if text.is_empty() {
                    return None;
                }
                let options = question
                    .options
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|option| {
                        let label = option.label?.trim().to_owned();
                        (!label.is_empty()).then_some(AskOption {
                            label,
                            description: option
                                .description
                                .filter(|value| !value.trim().is_empty()),
                            caution: None,
                        })
                    })
                    .collect();
                Some(AskQuestion {
                    question: text,
                    options,
                    multi_select: question.multi_select.unwrap_or(false),
                    has_option_previews: false,
                })
            })
            .take(4)
            .collect();
        (!questions.is_empty()).then_some(questions)
    }

    fn ask_detail(&self, event_name: &str, payload: &Value) -> Option<String> {
        if event_name != "Notification" {
            let parsed = payloads::parse_hook(payload);
            return self
                .ask_question_detail(event_name, payload)
                .and_then(|questions| questions.into_iter().next())
                .map(|question| question.question)
                .or_else(|| {
                    (parsed.tool_name.as_deref() == Some("exit_plan_mode"))
                        .then_some(parsed.tool_input)
                        .flatten()
                        .and_then(|input| input.plan_path.or(input.plan_filename))
                        .as_deref()
                        .and_then(non_empty_trimmed)
                });
        }
        let parsed = payloads::parse_hook(payload);
        parsed
            .message
            .as_deref()
            .and_then(non_empty_trimmed)
            .or_else(|| {
                parsed
                    .details
                    .as_ref()
                    .and_then(|details| details.title.as_deref())
                    .and_then(non_empty_trimmed)
            })
    }

    fn ends_session(&self, event_name: &str) -> bool {
        event_name == "SessionEnd"
    }

    fn moves_on(&self, event_name: &str) -> bool {
        matches!(event_name, "BeforeAgent" | "AfterAgent")
    }

    fn local_context_refresh(
        &self,
        trigger: RefreshTrigger<'_>,
        ctx: &LocalContextRefreshCtx<'_>,
    ) -> Option<LocalContextRefresh> {
        if let RefreshTrigger::Hook(event_name) = trigger
            && !matches!(
                event_name,
                "SessionStart" | "BeforeAgent" | "AfterTool" | "AfterAgent"
            )
        {
            return None;
        }
        refresh_transcript_context(ctx)
    }

    fn transcript_files(&self) -> Vec<PathBuf> {
        spend::gemini_session_files()
    }

    fn session_transcript(&self, session_id: &str, prior_path: Option<&Path>) -> Option<PathBuf> {
        if let Some(path) = prior_path.filter(|path| path.is_file()) {
            return Some(path.to_owned());
        }
        let prefix: String = session_id.trim().chars().take(8).collect();
        if prefix.len() != 8 {
            return None;
        }
        find_session_transcript(self.transcript_files(), &prefix)
    }

    fn parse_spend(
        &self,
        path: &Path,
        resume: Option<&super::spending::SpendCursor>,
        prices: &super::PriceBook,
    ) -> super::spending::SpendParse {
        spend::parse_gemini_spend(path, resume, prices)
    }

    fn resume_command(&self, session_id: &str, _cwd: &Path) -> Option<Vec<String>> {
        Some(vec![
            "gemini".to_owned(),
            "--resume".to_owned(),
            session_id.to_owned(),
        ])
    }

    fn compact_command(&self) -> Option<&'static str> {
        Some("/compress")
    }

    fn permission_args(&self, mode: PermissionMode) -> Vec<String> {
        let value = match mode {
            PermissionMode::Ask => return Vec::new(),
            PermissionMode::Auto => "auto_edit",
            PermissionMode::Plan => "plan",
            PermissionMode::Yolo => "yolo",
        };
        vec!["--approval-mode".to_owned(), value.to_owned()]
    }

    fn ping_args(&self) -> Option<Vec<String>> {
        Some(Vec::new())
    }

    fn render_preset(
        &self,
        preset: &super::LaunchPreset,
    ) -> std::result::Result<Vec<String>, super::PresetErr> {
        let mut args = Vec::new();
        if let Some(model) = preset.model.as_deref().filter(|model| !model.is_empty()) {
            args.extend(["--model".to_owned(), model.to_owned()]);
        }
        if preset
            .effort
            .as_deref()
            .is_some_and(|effort| !effort.is_empty())
        {
            return Err(super::PresetErr::UnsupportedField {
                agent: "gemini",
                field: "effort",
            });
        }
        if preset.system_prompt_file.is_some() {
            return Err(super::PresetErr::UnsupportedField {
                agent: "gemini",
                field: "system-prompt-file",
            });
        }
        if preset.append_system_prompt_file.is_some() {
            return Err(super::PresetErr::UnsupportedField {
                agent: "gemini",
                field: "append-system-prompt-file",
            });
        }
        Ok(args)
    }

    fn preset_arg_matcher(&self, field: super::PresetField) -> Option<super::PresetArgMatcher> {
        (field == super::PresetField::Model)
            .then(|| super::PresetArgMatcher::Flag(vec!["--model".to_owned(), "-m".to_owned()]))
    }

    fn launch_command(&self, extra_args: &[String], prompt: Option<&str>) -> Option<Vec<String>> {
        Some(super::positional_prompt_argv("gemini", extra_args, prompt))
    }

    fn install_hooks(&self) -> Result<HookInstallReport> {
        install::settings_path().and_then(|path| install::install_into(&path))
    }

    fn preview_hook_install(&self) -> Result<HookInstallPreview> {
        install::settings_path().and_then(|path| install::preview_at(&path))
    }

    fn uninstall_hooks(&self) -> Result<HookUninstallReport> {
        install::settings_path().and_then(|path| install::uninstall_from(&path))
    }

    fn hooks_installed(&self) -> bool {
        install::settings_path().is_ok_and(|path| install::hooks_installed_at(&path))
    }

    fn managed_hook_artifacts_present(&self) -> bool {
        install::settings_path().is_ok_and(|path| install::managed_artifacts_at(&path))
    }

    fn probe_account(&self) -> super::account::AccountProbe {
        account::probe()
    }
}

fn notification_ask_kind(details: Option<&payloads::GeminiNotificationDetails>) -> AskKind {
    match details.and_then(|details| details.kind.as_deref()) {
        Some("ask_user") => AskKind::Question,
        Some("exit_plan_mode") => AskKind::PlanApproval,
        _ => AskKind::Permission,
    }
}

struct TranscriptSnapshot {
    model: Option<String>,
    total: Option<u64>,
    input: Option<u64>,
    cached: Option<u64>,
    output: Option<u64>,
}

fn transcript_snapshot(path: &Path) -> Option<TranscriptSnapshot> {
    let tail = super::read_transcript_tail(path)?;
    let folded = payloads::fold_transcript(&tail);
    let latest = folded.latest_gemini();
    let tokens = latest.and_then(|message| message.tokens.as_ref());
    Some(TranscriptSnapshot {
        model: latest.and_then(|message| message.model.clone()),
        total: tokens.and_then(|tokens| tokens.total),
        input: tokens.and_then(|tokens| tokens.input),
        cached: tokens.and_then(|tokens| tokens.cached),
        output: tokens.map(|tokens| {
            tokens
                .output
                .unwrap_or(0)
                .saturating_add(tokens.thoughts.unwrap_or(0))
        }),
    })
}

fn apply_snapshot_to_observation(
    observation: &mut AgentLifecycleObservation,
    snapshot: &TranscriptSnapshot,
) {
    let window = model_context_window(snapshot.model.as_deref());
    observation.launch.model = snapshot.model.clone();
    observation.context_window = Some(window);
    observation.total_tokens = snapshot.total;
    observation.context_pct = snapshot.total.map(|total| percent(total, window));
    observation.cache_read_input_tokens = snapshot.cached;
    observation.fresh_input_tokens = snapshot
        .input
        .map(|input| input.saturating_sub(snapshot.cached.unwrap_or(0)));
    observation.output_tokens = snapshot.output;
}

fn refresh_transcript_context(ctx: &LocalContextRefreshCtx<'_>) -> Option<LocalContextRefresh> {
    let path =
        GeminiAdapter.session_transcript(ctx.agent_id, ctx.prior_transcript_path.map(Path::new))?;
    let stat = transcript_stat(&path)?;
    if ctx.prior_transcript_stat == Some(&stat) {
        return None;
    }
    let tail = super::read_transcript_tail(&path)?;
    let folded = payloads::fold_transcript(&tail);
    let latest = folded.latest_gemini();
    let model_id = latest
        .and_then(|message| message.model.clone())
        .or_else(|| ctx.model_hint.map(ToOwned::to_owned));
    let window = model_context_window(model_id.as_deref());
    let latest_tokens = latest.and_then(|message| message.tokens.as_ref());
    let total = latest_tokens.and_then(|tokens| tokens.total).unwrap_or(0);
    let current_usage = latest_tokens.map(|tokens| AgentCurrentUsage {
        input_tokens: tokens
            .input
            .map(|input| input.saturating_sub(tokens.cached.unwrap_or(0))),
        output_tokens: Some(
            tokens
                .output
                .unwrap_or(0)
                .saturating_add(tokens.thoughts.unwrap_or(0)),
        ),
        cache_creation_input_tokens: None,
        cache_read_input_tokens: tokens.cached,
    });
    let tokens = Some(AgentTokenUsage {
        context_window_size: Some(window),
        used_percentage: Some(percent(total, window)),
        remaining_percentage: None,
        current_usage,
    });
    let prices = pricing::cached_book(ctx.shared_pricing_cache_path);
    let cost_usd = spend::parse_gemini_spend(&path, None, &prices)
        .entries
        .iter()
        .map(|entry| entry.cost_usd)
        .sum::<f64>();
    Some(LocalContextRefresh {
        model_id,
        effort: None,
        tokens,
        cost: (cost_usd > 0.0).then_some(AgentCost {
            total_cost_usd: Some(cost_usd),
            ..AgentCost::default()
        }),
        turn_error: None,
        turn_complete: None,
        turn_interrupted: None,
        transcript_path: Some(path.to_string_lossy().into_owned()),
        transcript_stat: Some(stat),
    })
}

fn model_context_window(model: Option<&str>) -> u64 {
    if model.is_some_and(|model| model.to_ascii_lowercase().contains("gemma")) {
        GEMMA_CONTEXT_WINDOW
    } else {
        GEMINI_CONTEXT_WINDOW
    }
}

fn find_session_transcript(
    files: impl IntoIterator<Item = PathBuf>,
    session_prefix: &str,
) -> Option<PathBuf> {
    files.into_iter().find(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| {
                name.strip_suffix(".jsonl")
                    .or_else(|| name.strip_suffix(".json"))
            })
            .is_some_and(|stem| stem.ends_with(&format!("-{session_prefix}")))
    })
}

fn percent(used: u64, window: u64) -> u8 {
    used.saturating_mul(100)
        .checked_div(window)
        .unwrap_or(0)
        .min(100) as u8
}

fn transcript_stat(path: &Path) -> Option<TranscriptStat> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some(TranscriptStat {
        mtime_secs: modified.as_secs().try_into().unwrap_or(i64::MAX),
        mtime_nanos: modified.subsec_nanos(),
        len: metadata.len(),
    })
}

#[cfg(test)]
mod tests;
