//! Kimi Code command-hook and durable agent-record adapter.

pub(crate) mod account;
mod install;
pub(crate) mod oauth_usage;
pub(crate) mod payloads;
pub(crate) mod spend;
pub mod wire;

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::Deserialize;
use serde_json::Value;

use super::context::{AgentCurrentUsage, AgentTokenUsage};
use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, ConcernCoverage, HookCoverage, IntegrationConcern,
    PlanLabel, RealtimeUsageChannel, RemoteControlCapability, ThreadKey, ToolClassification,
};
use super::lifecycle::{LifecycleSignal, LifecycleSignalKind};
use super::{
    AgentAdapter, AgentLifecycleObservation, AgentTurnError, ClassifiedHook, HookInstallPreview,
    HookInstallReport, HookUninstallReport, LocalContextRefresh, LocalContextRefreshCtx,
    RefreshTrigger, Result, TranscriptStat, TurnErrorClass, classify_agent_hook, non_empty_trimmed,
    sanitize_user_prompt,
};
use crate::harness::run::PermissionMode;
use crate::ids::AgentSessionId;
use crate::transcript::{AskOption, AskQuestion};

pub const HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PermissionRequest",
    "PermissionResult",
    "Stop",
    "StopFailure",
    "Interrupt",
    "SessionEnd",
    "SubagentStart",
    "SubagentStop",
    "PreCompact",
    "PostCompact",
    "Notification",
];

const WIRED_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PermissionRequest",
    "PermissionResult",
    "Stop",
    "StopFailure",
    "Interrupt",
    "SessionEnd",
    "SubagentStart",
    "SubagentStop",
    "PreCompact",
    "PostCompact",
    "Notification",
];

static KIMI_DESCRIPTOR: AgentDescriptor = AgentDescriptor {
    kind: "kimi",
    display_name: "Kimi",
    brand: Brand {
        emblem: None,
        color: 33,
        color_rgb: (0x17, 0x83, 0xff),
    },
    plan_label: PlanLabel::Prefixed { prefix: "Kimi" },
    sub_providers: &[],
    tools: ToolClassification {
        mutating: &["Bash", "Write", "Edit"],
        editing: &["Write", "Edit"],
        blocking: &[
            ("AskUserQuestion", super::AskKind::Question),
            ("ExitPlanMode", super::AskKind::PlanApproval),
        ],
    },
    capabilities: Capabilities {
        blocking_asks: true,
        native_ask_ui: true,
        rich_context: false,
        transcript_tail_context: true,
        context_usage: false,
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
    coverage: KIMI_COVERAGE,
    lifecycle_hooks: KIMI_LIFECYCLE_HOOKS,
    default_context_window: Some(262_144),
    default_model: None,
    process_names: &["kimi", "kimi-code"],
    bin_names: &["kimi"],
    extra_bin_dirs: &[".kimi-code/bin"],
    activity_events: &[
        "SessionStart",
        "UserPromptSubmit",
        "PostToolUse",
        "PostToolUseFailure",
        "PermissionResult",
        "Stop",
        "Interrupt",
        "SubagentStart",
        "SubagentStop",
        "Notification",
    ],
    hook_install_unavailable: None,
    thread_key: ThreadKey::SessionDir,
};

const KIMI_COVERAGE: &[(IntegrationConcern, ConcernCoverage)] = &[
    (
        IntegrationConcern::TurnLifecycle,
        ConcernCoverage::Wired {
            via: "SessionStart/UserPromptSubmit/Stop",
        },
    ),
    (
        IntegrationConcern::Permission,
        ConcernCoverage::Wired {
            via: "PermissionRequest hook",
        },
    ),
    (
        IntegrationConcern::PlanApproval,
        ConcernCoverage::Wired {
            via: "PermissionRequest ExitPlanMode",
        },
    ),
    (
        IntegrationConcern::UserQuestion,
        ConcernCoverage::Wired {
            via: "PreToolUse AskUserQuestion",
        },
    ),
    (
        IntegrationConcern::Answer,
        ConcernCoverage::Unsupported {
            reason: "native Kimi UI owns answers",
        },
    ),
    (
        IntegrationConcern::Compaction,
        ConcernCoverage::Wired {
            via: "PreCompact/PostCompact",
        },
    ),
    (
        IntegrationConcern::Subagents,
        ConcernCoverage::Partial {
            via: "parent activity hooks",
            gap: "hooks carry no child identity; Wire child rows are deferred",
        },
    ),
    (
        IntegrationConcern::BackgroundParking,
        ConcernCoverage::Unsupported {
            reason: "background parking is not mapped",
        },
    ),
    (
        IntegrationConcern::SessionEnd,
        ConcernCoverage::Wired { via: "SessionEnd" },
    ),
    (
        IntegrationConcern::IdleNotification,
        ConcernCoverage::Partial {
            via: "turn boundaries + ask path + stall window",
            gap: "no idle notification hook",
        },
    ),
    (
        IntegrationConcern::ContextUsage,
        ConcernCoverage::Partial {
            via: "derived from Wire usage.record token totals",
            gap: "kimi-code omits the exact context ratio from the durable log",
        },
    ),
    (
        IntegrationConcern::RealtimeCost,
        ConcernCoverage::Wired {
            via: "priced Wire usage.record (model present)",
        },
    ),
    (
        IntegrationConcern::RichContext,
        ConcernCoverage::Unsupported {
            reason: "no push transport; bounded Wire tail only",
        },
    ),
    (
        IntegrationConcern::HookInstall,
        ConcernCoverage::Wired {
            via: "~/.kimi-code/config.toml [[hooks]]",
        },
    ),
    (
        IntegrationConcern::AccountSpend,
        ConcernCoverage::Partial {
            via: "managed OAuth quota plus priced agent-record tokens",
            gap: "effective provider attribution and non-USD balance currency need shared model support",
        },
    ),
    (
        IntegrationConcern::RemoteControl,
        ConcernCoverage::Unsupported {
            reason: "no remote-control surface",
        },
    ),
];

const KIMI_LIFECYCLE_HOOKS: &[(LifecycleSignalKind, HookCoverage)] = &[
    (
        LifecycleSignalKind::Registered,
        HookCoverage::Native {
            event: "SessionStart",
        },
    ),
    (
        LifecycleSignalKind::TurnStarted,
        HookCoverage::Native {
            event: "UserPromptSubmit",
        },
    ),
    (
        LifecycleSignalKind::TurnEnded,
        HookCoverage::Native { event: "Stop" },
    ),
    (
        LifecycleSignalKind::ToolUsed,
        HookCoverage::Native {
            event: "PostToolUse",
        },
    ),
    (
        LifecycleSignalKind::AwaitingInput,
        HookCoverage::Native {
            event: "PermissionRequest",
        },
    ),
    (
        LifecycleSignalKind::SubagentStarted,
        HookCoverage::Derived {
            via: "parent activity only",
            gap: "child identity is absent from hooks",
        },
    ),
    (
        LifecycleSignalKind::SubagentStopped,
        HookCoverage::Derived {
            via: "parent activity only",
            gap: "child identity is absent from hooks",
        },
    ),
    (
        LifecycleSignalKind::Compacting,
        HookCoverage::Native {
            event: "PreCompact",
        },
    ),
    (
        LifecycleSignalKind::CompactionEnded,
        HookCoverage::Native {
            event: "PostCompact",
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
            via: "rimz exec wrapper",
            gap: "native hooks do not report mux-session death",
        },
    ),
];

#[derive(Clone, Debug, Default)]
pub struct KimiAdapter;

impl AgentAdapter for KimiAdapter {
    fn descriptor(&self) -> &'static AgentDescriptor {
        &KIMI_DESCRIPTOR
    }

    fn configured_identity(&self) -> (Option<String>, Option<String>) {
        (spend::configured_model(), None)
    }

    fn classify_hook(&self, event_name: &str, payload: &Value) -> ClassifiedHook {
        let parsed = payloads::parse(event_name, payload);
        let ask = match event_name {
            "PermissionRequest" => Some(
                self.descriptor()
                    .blocking_tool_kind(parsed.tool_name.as_deref())
                    .unwrap_or(super::AskKind::Permission),
            ),
            "PreToolUse"
                if parsed.tool_name.as_deref() == Some("AskUserQuestion")
                    && !parsed.question_background =>
            {
                Some(super::AskKind::Question)
            }
            _ => None,
        };
        classify_agent_hook(event_name, ask, WIRED_EVENTS)
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
                "SessionStart",
                serde_json::json!({"session_id":"s"}),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "UserPromptSubmit",
                serde_json::json!({"session_id":"s","prompt":[{"type":"text","text":"fix"}]}),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "PreToolUse",
                serde_json::json!({"session_id":"s","tool_name":"AskUserQuestion","tool_input":{"questions":[{"question":"Continue?"}]}}),
                AgentHookClass::AwaitingUser,
                Some(super::AskKind::Question),
            ),
            ClassificationSample::new(
                "PreToolUse",
                serde_json::json!({"session_id":"s","tool_name":"ExitPlanMode"}),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "PostToolUse",
                serde_json::json!({"session_id":"s","tool_name":"Write"}),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "PostToolUseFailure",
                serde_json::json!({"session_id":"s","tool_name":"Bash"}),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "Stop",
                serde_json::json!({"session_id":"s"}),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "StopFailure",
                serde_json::json!({"session_id":"s","error_type":"RuntimeError"}),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "SessionEnd",
                serde_json::json!({"session_id":"s"}),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "SubagentStart",
                serde_json::json!({"session_id":"s","agent_name":"coder"}),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "SubagentStop",
                serde_json::json!({"session_id":"s","agent_name":"coder"}),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "PreCompact",
                serde_json::json!({"session_id":"s","trigger":"manual"}),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "PostCompact",
                serde_json::json!({"session_id":"s","trigger":"auto"}),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "Notification",
                serde_json::json!({"session_id":"s","notification_type":"task.completed"}),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "PermissionRequest",
                serde_json::json!({"session_id":"s","tool_call_id":"r","tool_name":"Bash","action":"Run shell"}),
                AgentHookClass::AwaitingUser,
                Some(super::AskKind::Permission),
            ),
            ClassificationSample::new(
                "PermissionRequest",
                serde_json::json!({"session_id":"s","tool_call_id":"r","tool_name":"ExitPlanMode","action":"Exit plan mode"}),
                AgentHookClass::AwaitingUser,
                Some(super::AskKind::PlanApproval),
            ),
            ClassificationSample::new(
                "PermissionResult",
                serde_json::json!({"session_id":"s","tool_call_id":"r","tool_name":"Bash","decision":"approved"}),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "Interrupt",
                serde_json::json!({"session_id":"s","turn_id":"t","reason":"cancelled"}),
                AgentHookClass::Lifecycle,
                None,
            ),
        ]
    }

    #[cfg(test)]
    fn spend_fixture(&self) -> Option<super::SpendFixture> {
        Some(super::SpendFixture {
            session_id: "s",
            file_name: "sessions/wd/s/agents/main/wire.jsonl",
            body: super::SpendFixtureBody::Jsonl(concat!(
                "{\"type\":\"metadata\",\"protocol_version\":\"1.4\"}\n",
                "{\"type\":\"usage.record\",\"time\":1770000000000,\"model\":\"moonshot/kimi-k2.5\",\"usage\":{\"inputOther\":100,\"output\":50,\"inputCacheRead\":10,\"inputCacheCreation\":5},\"usageScope\":\"turn\"}"
            )),
        })
    }

    fn render_neutral(&self, _event_name: &str) -> Result<Option<Value>> {
        Ok(None)
    }

    fn ask_question_detail(&self, event_name: &str, payload: &Value) -> Option<Vec<AskQuestion>> {
        if event_name != "PreToolUse" {
            return None;
        }
        let parsed = payloads::parse(event_name, payload);
        if parsed.tool_name.as_deref() != Some("AskUserQuestion") {
            return None;
        }
        parse_questions(parsed.tool_input.as_ref()?)
    }

    fn ask_detail(&self, event_name: &str, payload: &Value) -> Option<String> {
        if event_name == "PermissionRequest" {
            let parsed = payloads::parse(event_name, payload);
            return parsed.action.and_then(|action| non_empty_trimmed(&action));
        }
        self.ask_question_detail(event_name, payload)
            .and_then(|questions| questions.into_iter().next())
            .map(|question| question.question)
    }

    fn observe_lifecycle(
        &self,
        event_name: &str,
        payload: &Value,
    ) -> Option<AgentLifecycleObservation> {
        let parsed = payloads::parse(event_name, payload);
        let signal = match event_name {
            "SessionStart" => LifecycleSignal::Registered,
            "UserPromptSubmit" => LifecycleSignal::TurnStarted,
            "PreToolUse"
                if parsed.tool_name.as_deref() == Some("AskUserQuestion")
                    && !parsed.question_background =>
            {
                LifecycleSignal::AwaitingInput {
                    kind: super::AskKind::Question,
                    ask_id: None,
                    detail: self.ask_detail(event_name, payload),
                }
            }
            "PermissionRequest" => LifecycleSignal::AwaitingInput {
                kind: self
                    .descriptor()
                    .blocking_tool_kind(parsed.tool_name.as_deref())
                    .unwrap_or(super::AskKind::Permission),
                ask_id: None,
                detail: self.ask_detail(event_name, payload),
            },
            "PermissionResult" => LifecycleSignal::ToolUsed {
                mutates: false,
                edits: false,
            },
            "PostToolUse" => LifecycleSignal::ToolUsed {
                mutates: self.descriptor().tool_mutates(payload),
                edits: self.descriptor().tool_edits_files(payload),
            },
            "PostToolUseFailure" => LifecycleSignal::ToolUsed {
                mutates: false,
                edits: false,
            },
            "Stop" => LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
            "StopFailure" => LifecycleSignal::TurnEnded {
                errored: true,
                parked_on_background: false,
            },
            "Interrupt" => LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
            "SessionEnd" => LifecycleSignal::Ended,
            "PreCompact" => LifecycleSignal::Compacting,
            "PostCompact" => LifecycleSignal::CompactionEnded {
                auto: parsed.trigger.as_deref().map(|trigger| trigger == "auto"),
            },
            _ => return None,
        };
        let agent_id = parsed
            .common
            .session_id
            .clone()
            .map(AgentSessionId::from)
            .or_else(|| {
                payload
                    .get("session_id")
                    .and_then(Value::as_str)
                    .map(AgentSessionId::from)
            });
        let mut observation =
            AgentLifecycleObservation::new(agent_id, signal).with_worktree_from_payload(payload);
        observation.task = sanitize_user_prompt(parsed.prompt.as_deref());
        observation.prompt = sanitize_user_prompt(parsed.prompt.as_deref());
        if event_name == "SessionStart"
            && let Some(session_id) = parsed.common.session_id.as_deref()
        {
            observation.transcript_path =
                wire::wire_path(session_id, parsed.common.cwd.as_deref().map(Path::new))
                    .map(|path| path.to_string_lossy().into_owned());
        }
        Some(observation)
    }

    fn observe_turn_error_from_hook(
        &self,
        event_name: &str,
        payload: &Value,
    ) -> Option<AgentTurnError> {
        if event_name != "StopFailure" {
            return None;
        }
        let parsed = payloads::parse(event_name, payload);
        let label = parsed
            .error_message
            .or(parsed.error_type)
            .and_then(|value| non_empty_trimmed(&value))
            .map(|value| value.chars().take(80).collect::<String>());
        Some(AgentTurnError {
            class: TurnErrorClass::classify_label(label.as_deref()),
            at: Timestamp::now(),
            label,
        })
    }

    fn last_assistant_message(
        &self,
        event_name: &str,
        payload: &Value,
        _observation: &AgentLifecycleObservation,
    ) -> Option<String> {
        if !matches!(event_name, "Stop" | "StopFailure") {
            return None;
        }
        let parsed = payloads::parse(event_name, payload);
        let path = wire::wire_path(
            parsed.common.session_id.as_deref()?,
            parsed.common.cwd.as_deref().map(Path::new),
        )?;
        let tail = super::read_transcript_tail(&path)?;
        self.parse_transcript_messages(&tail)
            .into_iter()
            .rev()
            .find(|message| message.role == super::TranscriptRole::Assistant)
            .map(|message| message.text)
    }

    fn parse_transcript_messages(&self, lines: &str) -> Vec<super::TranscriptMessage> {
        lines.lines().filter_map(parse_context_message).collect()
    }

    fn ends_session(&self, event_name: &str) -> bool {
        event_name == "SessionEnd"
    }

    fn moves_on(&self, event_name: &str) -> bool {
        matches!(event_name, "UserPromptSubmit" | "Stop" | "Interrupt")
    }

    fn permission_args(&self, mode: PermissionMode) -> Vec<String> {
        match mode {
            PermissionMode::Ask => Vec::new(),
            PermissionMode::Auto => vec!["--auto".to_owned()],
            PermissionMode::Yolo => vec!["--yolo".to_owned()],
            PermissionMode::Plan => vec!["--plan".to_owned()],
        }
    }

    fn ping_args(&self) -> Option<Vec<String>> {
        Some(Vec::new())
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
        for (present, field) in [
            (
                preset
                    .effort
                    .as_deref()
                    .is_some_and(|value| !value.is_empty()),
                "effort",
            ),
            (preset.system_prompt_file.is_some(), "system-prompt-file"),
            (
                preset.append_system_prompt_file.is_some(),
                "append-system-prompt-file",
            ),
        ] {
            if present {
                return Err(super::PresetErr::UnsupportedField {
                    agent: "kimi",
                    field,
                });
            }
        }
        Ok(argv)
    }

    fn preset_arg_matcher(&self, field: super::PresetField) -> Option<super::PresetArgMatcher> {
        (field == super::PresetField::Model)
            .then(|| super::PresetArgMatcher::Flag(vec!["--model".to_owned(), "-m".to_owned()]))
    }

    fn launch_command(&self, extra_args: &[String], prompt: Option<&str>) -> Option<Vec<String>> {
        let mut argv = vec!["kimi".to_owned()];
        argv.extend(
            extra_args
                .iter()
                .filter(|arg| {
                    prompt.is_none() || !matches!(arg.as_str(), "--auto" | "--yolo" | "--plan")
                })
                .cloned(),
        );
        if let Some(prompt) = prompt.filter(|prompt| !prompt.is_empty()) {
            argv.extend([
                "--prompt".to_owned(),
                prompt.to_owned(),
                "--output-format".to_owned(),
                "stream-json".to_owned(),
            ]);
        }
        Some(argv)
    }

    fn resume_command(&self, session_id: &str, _cwd: &Path) -> Option<Vec<String>> {
        Some(vec![
            "kimi".to_owned(),
            "--session".to_owned(),
            session_id.to_owned(),
        ])
    }

    fn local_context_refresh(
        &self,
        trigger: RefreshTrigger<'_>,
        ctx: &LocalContextRefreshCtx<'_>,
    ) -> Option<LocalContextRefresh> {
        if matches!(trigger, RefreshTrigger::Hook(event) if !matches!(event, "Stop" | "PostToolUse" | "PostToolUseFailure" | "PostCompact" | "Notification"))
        {
            return None;
        }
        refresh_wire_context(ctx.agent_id, ctx.prior_transcript_stat)
    }

    fn install_hooks(&self) -> Result<HookInstallReport> {
        install::install(&install::config_path()?)
    }
    fn preview_hook_install(&self) -> Result<HookInstallPreview> {
        install::preview(&install::config_path()?)
    }
    fn uninstall_hooks(&self) -> Result<HookUninstallReport> {
        install::uninstall(&install::config_path()?)
    }
    fn hooks_installed(&self) -> bool {
        install::config_path().is_ok_and(|path| install::installed(&path))
    }
    fn managed_hook_artifacts_present(&self) -> bool {
        install::config_path().is_ok_and(|path| install::managed(&path))
    }

    fn transcript_files(&self) -> Vec<PathBuf> {
        spend::files()
    }

    fn session_transcript(&self, session_id: &str, prior_path: Option<&Path>) -> Option<PathBuf> {
        if let Some(path) = prior_path.filter(|path| {
            path.file_name().is_some_and(|name| name == "wire.jsonl") && path.is_file()
        }) {
            return Some(path.to_path_buf());
        }
        wire::wire_path(session_id, None)
    }

    fn parse_spend(
        &self,
        path: &Path,
        resume: Option<&super::spending::SpendCursor>,
        prices: &super::PriceBook,
    ) -> super::spending::SpendParse {
        spend::parse(path, resume, prices)
    }

    fn probe_account(&self) -> super::account::AccountProbe {
        account::probe()
    }

    fn probe_oauth_usage(&self) -> super::OauthUsageProbe {
        super::credits::map_probe_snapshot(oauth_usage::fetch(), "kimi")
    }

    fn oauth_credentials_stamp(&self) -> Option<u64> {
        oauth_usage::credentials_stamp()
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct QuestionInput {
    questions: Vec<QuestionWire>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct QuestionWire {
    question: Option<String>,
    options: Vec<QuestionOption>,
    multi_select: bool,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct QuestionOption {
    label: Option<String>,
    description: Option<String>,
}

fn parse_questions(input: &Value) -> Option<Vec<AskQuestion>> {
    let input: QuestionInput = serde_json::from_value(input.clone()).ok()?;
    let questions = input
        .questions
        .into_iter()
        .filter_map(|question| {
            let text = question.question.as_deref().and_then(non_empty_trimmed)?;
            let options = question
                .options
                .into_iter()
                .filter_map(|option| {
                    Some(AskOption {
                        label: option.label.as_deref().and_then(non_empty_trimmed)?,
                        description: option
                            .description
                            .and_then(|value| non_empty_trimmed(&value)),
                        caution: None,
                    })
                })
                .collect();
            Some(AskQuestion {
                question: text,
                options,
                multi_select: question.multi_select,
                has_option_previews: false,
            })
        })
        .collect::<Vec<_>>();
    (!questions.is_empty()).then_some(questions)
}

fn refresh_wire_context(
    session_id: &str,
    prior_stat: Option<&TranscriptStat>,
) -> Option<LocalContextRefresh> {
    let path = wire::wire_path(session_id, None)?;
    let stat = transcript_stat(&path)?;
    refresh_wire_path(&path, stat, prior_stat)
}

fn refresh_wire_path(
    path: &Path,
    stat: TranscriptStat,
    prior_stat: Option<&TranscriptStat>,
) -> Option<LocalContextRefresh> {
    if prior_stat == Some(&stat) {
        return None;
    }
    let tail = super::read_transcript_tail(path)?;
    let records = wire::records_from_bytes(tail.as_bytes());
    let latest_usage = wire::usage_records(&records).into_iter().last();
    let model_id = latest_model(&records).or_else(spend::configured_model);
    let context_window_size = configured_context_window(model_id.as_deref()).or(Some(262_144));
    let tokens = if let Some(input) = wire::latest_context_tokens(&records) {
        Some(AgentTokenUsage {
            context_window_size,
            used_percentage: context_window_size.map(|window| percentage(input, window)),
            remaining_percentage: None,
            current_usage: Some(AgentCurrentUsage {
                input_tokens: latest_usage
                    .as_ref()
                    .and_then(|(_, record)| record.usage.input_other),
                output_tokens: latest_usage
                    .as_ref()
                    .and_then(|(_, record)| record.usage.output),
                cache_creation_input_tokens: latest_usage
                    .as_ref()
                    .and_then(|(_, record)| record.usage.input_cache_creation),
                cache_read_input_tokens: latest_usage
                    .as_ref()
                    .and_then(|(_, record)| record.usage.input_cache_read),
            }),
        })
    } else {
        // No usage record in the bounded tail. Emit the shared fresh sentinel —
        // unknown fill over zeroed step usage — so the merge layer preserves an
        // established session's context meter instead of flashing it to 0% when a
        // heavy tool record pushes the last usage record out of the tail window.
        // A brand-new session still resolves to the empty baseline.
        Some(AgentTokenUsage {
            context_window_size,
            used_percentage: None,
            remaining_percentage: None,
            current_usage: Some(AgentCurrentUsage::default()),
        })
    };
    Some(LocalContextRefresh {
        model_id,
        effort: latest_effort(&records),
        tokens,
        cost: None,
        turn_error: None,
        turn_complete: None,
        turn_interrupted: None,
        transcript_path: None,
        transcript_stat: Some(stat),
    })
}

fn latest_model(records: &[wire::WireRecord]) -> Option<String> {
    records.iter().rev().find_map(|record| {
        let model = match record.kind.as_str() {
            "usage.record" => record.fields.get("model"),
            "config.update" => record.fields.get("modelAlias"),
            _ => None,
        }?
        .as_str()?
        .trim();
        (!model.is_empty()).then(|| model.to_owned())
    })
}

fn latest_effort(records: &[wire::WireRecord]) -> Option<String> {
    records.iter().rev().find_map(|record| {
        let effort = (record.kind == "config.update")
            .then(|| record.fields.get("thinkingEffort"))??
            .as_str()?
            .trim();
        (!effort.is_empty()).then(|| effort.to_owned())
    })
}

fn percentage(used: u64, capacity: u64) -> u8 {
    if capacity == 0 {
        return 0;
    }
    ((used as f64 / capacity as f64) * 100.0)
        .round()
        .clamp(0.0, 100.0) as u8
}

fn configured_context_window(model_hint: Option<&str>) -> Option<u64> {
    let path = install::config_path().ok()?;
    configured_context_window_at(&path, model_hint)
}

fn configured_context_window_at(path: &Path, model_hint: Option<&str>) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    let root: toml::Table = toml::from_str(&text).ok()?;
    let alias = model_hint
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .or_else(|| root.get("default_model").and_then(toml::Value::as_str))?;
    let model = root
        .get("models")
        .and_then(toml::Value::as_table)?
        .get(alias)?
        .as_table()?;
    model
        .get("overrides")
        .and_then(toml::Value::as_table)
        .and_then(|overrides| overrides.get("max_context_size"))
        .or_else(|| model.get("max_context_size"))
        .and_then(toml::Value::as_integer)
        .and_then(|value| u64::try_from(value).ok())
}

fn transcript_stat(path: &Path) -> Option<TranscriptStat> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    Some(TranscriptStat {
        mtime_secs: i64::try_from(modified.as_secs()).ok()?,
        mtime_nanos: modified.subsec_nanos(),
        len: metadata.len(),
    })
}

fn parse_context_message(line: &str) -> Option<super::TranscriptMessage> {
    let record = serde_json::from_str::<wire::WireRecord>(line).ok()?;
    let value = wire::record_message(&record)?;
    let role = match value.get("role").and_then(Value::as_str)? {
        "user" => super::TranscriptRole::User,
        "assistant" => super::TranscriptRole::Assistant,
        _ => return None,
    };
    let content = value.get("content")?;
    let text = if let Some(text) = content.as_str() {
        text.to_owned()
    } else {
        content
            .as_array()?
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let text = text.trim();
    (!text.is_empty()).then(|| super::TranscriptMessage {
        role,
        at: record_time(record.time),
        text: text.to_owned(),
    })
}

fn record_time(time: f64) -> Option<Timestamp> {
    let millis = if time > 100_000_000_000.0 {
        time as i64
    } else {
        (time * 1_000.0) as i64
    };
    Timestamp::from_millisecond(millis).ok()
}

#[cfg(test)]
mod tests;
