//! OpenCode hook adapter.
//!
//! OpenCode loads TypeScript plugins in-process inside each embedded server.
//! Rimz ships one plugin (`plugin.ts`) that shells out to
//! `rimz hooks feed --source opencode`, posts a Rimz-owned snake_case payload
//! on stdin. Current permission and question prompts arrive through
//! `permission.asked` and `question.asked` bus events; the compatibility
//! `permission.ask` hook reads stdout on older releases and leaves OpenCode's
//! `output.status` at `ask` on the neutral path. Native reply bus events record
//! answers and clear waiting after the user responds in OpenCode's own TUI.
//! `session.deleted` and the server-scoped `dispose` sweep normalize to one
//! per-session `session_ended` event, with pane liveness as the crash backstop.

pub(crate) mod account;
pub(crate) mod oauth_usage;
pub(crate) mod payloads;
pub mod server;
pub(crate) mod spend;
mod transcript;

use std::path::{Path, PathBuf};

use serde_json::Value;
#[cfg(test)]
use serde_json::json;

use super::AskKind;
use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, ConcernCoverage, HookCoverage, IntegrationConcern,
    PlanLabel, RealtimeUsageChannel, RemoteControlCapability, ThreadKey, ToolClassification,
};
use super::lifecycle::{LifecycleSignal, LifecycleSignalKind};
use super::managed_source::ManagedSource;
use super::pricing::PriceBook;
use super::{
    AgentAdapter, AgentErr, AgentLifecycleObservation, ClassifiedHook, HookInstallPreview,
    HookInstallReport, HookUninstallReport, LifecycleRefreshCtx, RefreshSpawn, RefreshTrigger,
    Result, SubagentIdentity, classify_agent_hook, resolve_subagent_identity, sanitize_user_prompt,
};
use crate::ids::AgentSessionId;
use crate::transcript::{AskAnswer, AskOption, AskQuestion};

static OPENCODE_DESCRIPTOR: AgentDescriptor = AgentDescriptor {
    kind: "opencode",
    display_name: "Open Code",
    brand: Brand {
        emblem: None,
        color: 208,
        color_rgb: (0xff, 0x87, 0x00),
    },
    plan_label: PlanLabel::TitleCaseOnly,
    sub_providers: &[],
    tools: ToolClassification {
        mutating: &["bash", "edit", "write", "apply_patch", "patch"],
        editing: &["edit", "write", "apply_patch", "patch"],
        blocking: &[],
    },
    capabilities: Capabilities {
        blocking_asks: true,
        native_ask_ui: true,
        rich_context: true,
        transcript_tail_context: false,
        context_usage: true,
        account_spend: true,
        subagents: true,
        background_tasks: false,
        registers_lazily: true,
        local_session_discovery: false,
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
    coverage: OPENCODE_COVERAGE,
    lifecycle_hooks: OPENCODE_LIFECYCLE_HOOKS,
    default_context_window: None,
    default_model: None,
    process_names: &["opencode", "bun"],
    bin_names: &["opencode"],
    extra_bin_dirs: &[".opencode/bin"],
    activity_events: &[
        "session_created",
        "chat_message",
        "session_idle",
        "session_error",
        "tool_after",
        "SubagentStart",
        "SubagentStop",
    ],
    hook_install_unavailable: None,
    thread_key: ThreadKey::PerFile,
};

const OPENCODE_COVERAGE: &[(IntegrationConcern, ConcernCoverage)] = &[
    (
        IntegrationConcern::TurnLifecycle,
        ConcernCoverage::Wired {
            via: "session_created/chat_message/session_idle",
        },
    ),
    (
        IntegrationConcern::Permission,
        ConcernCoverage::Wired {
            via: "permission_ask",
        },
    ),
    (
        IntegrationConcern::PlanApproval,
        ConcernCoverage::Unsupported {
            reason: "no plan-approval gate",
        },
    ),
    (
        IntegrationConcern::UserQuestion,
        ConcernCoverage::Wired {
            via: "question.asked",
        },
    ),
    (
        IntegrationConcern::Answer,
        ConcernCoverage::Unsupported {
            reason: "native answers are observed; Rimz-to-OpenCode answer transport is not mapped",
        },
    ),
    (
        IntegrationConcern::Compaction,
        ConcernCoverage::Wired {
            via: "session_compacting/session_compacted",
        },
    ),
    (
        IntegrationConcern::Subagents,
        ConcernCoverage::Wired {
            via: "SubagentStart/SubagentStop",
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
            via: "session_ended (session.deleted + dispose sweep)",
        },
    ),
    (
        IntegrationConcern::IdleNotification,
        ConcernCoverage::Partial {
            via: "turn-end + permission.ask/question.asked + stall window",
            gap: "no idle Notification hook; no idle-timeout nudge",
        },
    ),
    (
        IntegrationConcern::ContextUsage,
        ConcernCoverage::Wired {
            via: "message.updated token split",
        },
    ),
    (
        IntegrationConcern::RealtimeCost,
        ConcernCoverage::Wired {
            via: "authoritative per-session SQLite message spend sum, reconciled at each turn boundary",
        },
    ),
    (
        IntegrationConcern::RichContext,
        ConcernCoverage::Wired {
            via: "embedded server /config/providers + /session over plugin serverUrl",
        },
    ),
    (
        IntegrationConcern::HookInstall,
        ConcernCoverage::Wired {
            via: "~/.config/opencode/plugin/rimz.ts",
        },
    ),
    (
        IntegrationConcern::AccountSpend,
        ConcernCoverage::Wired {
            via: "SQLite message store + auth.json OAuth usage probe",
        },
    ),
    (
        IntegrationConcern::RemoteControl,
        ConcernCoverage::Unsupported {
            reason: "no remote-control surface",
        },
    ),
];

const OPENCODE_LIFECYCLE_HOOKS: &[(LifecycleSignalKind, HookCoverage)] = &[
    (
        LifecycleSignalKind::Registered,
        HookCoverage::Native {
            event: "session_created",
        },
    ),
    (
        LifecycleSignalKind::TurnStarted,
        HookCoverage::Native {
            event: "chat_message",
        },
    ),
    (
        LifecycleSignalKind::TurnEnded,
        HookCoverage::Native {
            event: "session_idle",
        },
    ),
    (
        LifecycleSignalKind::ToolUsed,
        HookCoverage::Native {
            event: "tool_after",
        },
    ),
    (
        LifecycleSignalKind::AwaitingInput,
        HookCoverage::Native {
            // One representative installed event names the signal, the Codex
            // precedent; `question_ask` is the other awaiting-user event and
            // rides the separately-wired `UserQuestion` concern.
            event: "permission_ask",
        },
    ),
    (
        LifecycleSignalKind::SubagentStarted,
        HookCoverage::Native {
            event: "SubagentStart",
        },
    ),
    (
        LifecycleSignalKind::SubagentStopped,
        HookCoverage::Native {
            event: "SubagentStop",
        },
    ),
    (
        LifecycleSignalKind::Compacting,
        HookCoverage::Native {
            event: "session_compacting",
        },
    ),
    (
        LifecycleSignalKind::CompactionEnded,
        HookCoverage::Native {
            event: "session_compacted",
        },
    ),
    (
        LifecycleSignalKind::Ended,
        HookCoverage::Native {
            event: "session_ended",
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

// The awaiting-user events (`permission_ask`, `question_ask`) are absent by
// design: `classify_hook` hands their `AskKind` to `classify_agent_hook`, which
// short-circuits to `AwaitingUser` before ever consulting this list. Native
// replies and normalized session end remain lifecycle observations.
const LIFECYCLE_EVENTS: &[&str] = &[
    "session_created",
    "chat_message",
    "session_idle",
    "session_error",
    "tool_after",
    "session_compacting",
    "session_compacted",
    "SubagentStart",
    "SubagentStop",
    "permission_replied",
    "question_replied",
    "question_rejected",
    "session_ended",
];

const WIRED_EVENTS: &[&str] = &[
    "session_created",
    "chat_message",
    "session_idle",
    "session_error",
    "tool_after",
    "session_compacting",
    "session_compacted",
    "SubagentStart",
    "SubagentStop",
    "permission_ask",
    "question_ask",
    "permission_replied",
    "question_replied",
    "question_rejected",
    "session_ended",
];

const PLUGIN_SOURCE: &str = include_str!("plugin.ts");
const OPENCODE_MANAGED_SOURCE: ManagedSource = ManagedSource {
    agent: "opencode",
    source: PLUGIN_SOURCE,
    wired_events: WIRED_EVENTS,
    artifact_noun: "plugin",
};

#[derive(Clone, Debug, Default)]
pub struct OpencodeAdapter;

impl AgentAdapter for OpencodeAdapter {
    fn descriptor(&self) -> &'static AgentDescriptor {
        &OPENCODE_DESCRIPTOR
    }

    fn classify_hook(&self, event_name: &str, _payload: &Value) -> ClassifiedHook {
        let ask_kind = match event_name {
            "permission_ask" => Some(AskKind::Permission),
            "question_ask" => Some(AskKind::Question),
            _ => None,
        };
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
                "permission_ask",
                json!({ "session_id": "ses_1", "tool_name": "bash" }),
                AgentHookClass::AwaitingUser,
                Some(AskKind::Permission),
            ),
            ClassificationSample::new(
                "question_ask",
                json!({ "session_id": "ses_1", "title": "Which database?" }),
                AgentHookClass::AwaitingUser,
                Some(AskKind::Question),
            ),
            ClassificationSample::new(
                "session_created",
                json!({ "session_id": "ses_1", "cwd": "/tmp/repo" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "chat_message",
                json!({ "session_id": "ses_1", "prompt": "fix auth" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "session_idle",
                json!({ "session_id": "ses_1" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "session_error",
                json!({ "session_id": "ses_1", "error_message": "boom" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "tool_after",
                json!({ "session_id": "ses_1", "tool_name": "bash" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "session_compacting",
                json!({ "session_id": "ses_1" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "session_compacted",
                json!({ "session_id": "ses_1" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "SubagentStart",
                json!({
                    "session_id": "ses_child",
                    "parent_session_id": "ses_parent",
                    "prompt": "review auth"
                }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "SubagentStop",
                json!({
                    "session_id": "ses_child",
                    "parent_session_id": "ses_parent"
                }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "permission_replied",
                json!({ "session_id": "ses_1", "reply": "once" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "question_replied",
                json!({ "session_id": "ses_1", "answers": [["Postgres"]] }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "question_rejected",
                json!({ "session_id": "ses_1" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "session_ended",
                json!({ "session_id": "ses_1", "reason": "deleted" }),
                AgentHookClass::Lifecycle,
                None,
            ),
        ]
    }

    #[cfg(test)]
    fn spend_fixture(&self) -> Option<super::SpendFixture> {
        Some(super::SpendFixture {
            session_id: "ses_1",
            file_name: "opencode.db",
            body: super::SpendFixtureBody::OpencodeSqlite {
                data: r#"{"cost":0.42,"modelID":"gpt-5","providerID":"openai","time":{"created":1780394400000},"tokens":{"input":100,"output":50}}"#,
            },
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
        let signal = match event_name {
            "session_created" => LifecycleSignal::Registered,
            "permission_ask" => LifecycleSignal::AwaitingInput {
                kind: AskKind::Permission,
                ask_id: None,
                detail: parsed.title.clone(),
            },
            "question_ask" => LifecycleSignal::AwaitingInput {
                kind: AskKind::Question,
                ask_id: None,
                detail: parsed.title.clone(),
            },
            "chat_message" => LifecycleSignal::TurnStarted,
            "session_idle" => LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
            "session_error" => LifecycleSignal::TurnEnded {
                errored: true,
                parked_on_background: false,
            },
            "permission_replied" | "question_replied" | "question_rejected" => {
                LifecycleSignal::ToolUsed {
                    mutates: false,
                    edits: false,
                }
            }
            "tool_after" if self.descriptor().tool_mutates(payload) => LifecycleSignal::ToolUsed {
                mutates: true,
                edits: self.descriptor().tool_edits_files(payload),
            },
            "session_compacting" => LifecycleSignal::Compacting,
            "session_compacted" => LifecycleSignal::CompactionEnded { auto: None },
            "SubagentStart" => LifecycleSignal::SubagentStarted,
            "SubagentStop" => LifecycleSignal::SubagentStopped {
                errored: payloads::errored(&parsed),
            },
            "session_ended" => LifecycleSignal::Ended,
            _ => return None,
        };

        let (agent_id, parent_agent_id) = if matches!(event_name, "SubagentStart" | "SubagentStop")
        {
            match resolve_subagent_identity(
                self.descriptor().kind,
                event_name,
                parsed.session_id.as_deref(),
                parsed.parent_session_id.as_deref(),
                payload,
            ) {
                SubagentIdentity::Resolved {
                    agent_id,
                    parent_agent_id,
                } => (Some(agent_id), Some(parent_agent_id)),
                SubagentIdentity::Quarantined => return None,
            }
        } else {
            (parsed.session_id.as_deref().map(AgentSessionId::from), None)
        };

        let mut observation =
            AgentLifecycleObservation::new(agent_id, signal).with_worktree_from_payload(payload);
        observation.parent_agent_id = parent_agent_id;
        observation.task = sanitize_user_prompt(parsed.prompt.as_deref());
        observation.prompt = sanitize_user_prompt(parsed.prompt.as_deref());
        observation.launch.model = parsed.model.clone();
        observation.launch.effort = parsed.effort;
        observation.context_window = parsed
            .context_window
            .or_else(|| context_window_for(parsed.model.as_deref()));
        observation.total_tokens = parsed.total_tokens;
        observation.cache_read_input_tokens = parsed.cache_read_input_tokens;
        observation.cache_write_input_tokens = parsed.cache_write_input_tokens;
        observation.fresh_input_tokens = parsed.input_tokens;
        observation.output_tokens = parsed.output_tokens;
        Some(observation)
    }

    fn ends_session(&self, event_name: &str) -> bool {
        event_name == "session_ended"
    }

    fn ask_question_detail(&self, event_name: &str, payload: &Value) -> Option<Vec<AskQuestion>> {
        if event_name != "question_ask" {
            return None;
        }
        let questions = payloads::parse_payload(payload).questions?;
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
                    multi_select: question.multiple.unwrap_or(false),
                    has_option_previews: false,
                })
            })
            .take(4)
            .collect();
        (!questions.is_empty()).then_some(questions)
    }

    fn native_ask_answer(&self, event_name: &str, payload: &Value) -> Option<Vec<AskAnswer>> {
        let parsed = payloads::parse_payload(payload);
        match event_name {
            "permission_replied" => {
                let reply = parsed.reply?.trim().to_owned();
                (!reply.is_empty()).then_some(vec![AskAnswer {
                    question: None,
                    chosen: vec![reply],
                    note: None,
                }])
            }
            "question_replied" => {
                let answers: Vec<_> = parsed
                    .answers?
                    .into_iter()
                    .filter_map(|choices| {
                        let chosen: Vec<_> = choices
                            .into_iter()
                            .map(|choice| choice.trim().to_owned())
                            .filter(|choice| !choice.is_empty())
                            .collect();
                        (!chosen.is_empty()).then_some(AskAnswer {
                            question: None,
                            chosen,
                            note: None,
                        })
                    })
                    .collect();
                (!answers.is_empty()).then_some(answers)
            }
            "question_rejected" => Some(vec![AskAnswer {
                question: None,
                chosen: vec!["(rejected)".to_owned()],
                note: None,
            }]),
            _ => None,
        }
    }

    fn moves_on(&self, event_name: &str) -> bool {
        matches!(
            event_name,
            "chat_message" | "session_idle" | "session_error"
        )
    }

    fn last_assistant_message(
        &self,
        event_name: &str,
        _payload: &Value,
        observation: &AgentLifecycleObservation,
    ) -> Option<String> {
        if !matches!(event_name, "session_idle" | "session_error") {
            return None;
        }
        let session_id = observation.agent_id.as_ref()?;
        let path = observation
            .transcript_path
            .as_deref()
            .map(Path::new)
            .filter(|path| path.is_file())
            .map(Path::to_path_buf)
            .or_else(|| spend::opencode_db_files().into_iter().next())?;
        transcript::last_assistant_message(&path, session_id)
    }

    fn context_refresh_spawn(
        &self,
        trigger: RefreshTrigger<'_>,
        ctx: &LifecycleRefreshCtx<'_>,
    ) -> Option<RefreshSpawn> {
        let RefreshTrigger::Hook(event_name) = trigger else {
            return None;
        };
        if !matches!(
            event_name,
            "session_created" | "chat_message" | "session_idle" | "session_error"
        ) {
            return None;
        }
        let server_url = ctx.server_url.filter(|url| !url.is_empty())?;
        let mut args = vec![
            "opencode".to_owned(),
            "refresh-context".to_owned(),
            "--session-id".to_owned(),
            ctx.agent_id.to_owned(),
            "--workspace-id".to_owned(),
            ctx.workspace_id.to_owned(),
            "--server-url".to_owned(),
            server_url.to_owned(),
        ];
        if let Some(model) = ctx.model_hint {
            args.extend(["--model".to_owned(), model.to_owned()]);
        }
        Some(RefreshSpawn { args })
    }

    fn transcript_files(&self) -> Vec<PathBuf> {
        spend::opencode_db_files()
    }

    fn session_transcript(&self, _session_id: &str, prior_path: Option<&Path>) -> Option<PathBuf> {
        if let Some(path) = prior_path.filter(|path| path.is_file()) {
            return Some(path.to_path_buf());
        }
        self.transcript_files().into_iter().next()
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
        resume: Option<&crate::agents::spending::SpendCursor>,
        prices: &PriceBook,
    ) -> crate::agents::spending::SpendParse {
        spend::parse_opencode_spend(path, resume, prices)
    }

    fn resume_command(&self, session_id: &str, _cwd: &Path) -> Option<Vec<String>> {
        Some(vec![
            "opencode".to_owned(),
            "--session".to_owned(),
            session_id.to_owned(),
        ])
    }

    fn fork_command(&self, session_id: &str, _cwd: &Path) -> Option<Vec<String>> {
        Some(vec![
            "opencode".to_owned(),
            "--session".to_owned(),
            session_id.to_owned(),
            "--fork".to_owned(),
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
        if preset
            .effort
            .as_deref()
            .is_some_and(|effort| !effort.is_empty())
        {
            return Err(super::PresetErr::UnsupportedField {
                agent: self.descriptor().kind,
                field: "effort",
            });
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

    fn preset_arg_matcher(&self, field: super::PresetField) -> Option<super::PresetArgMatcher> {
        (field == super::PresetField::Model)
            .then(|| super::PresetArgMatcher::Flag(vec!["--model".to_owned(), "-m".to_owned()]))
    }

    fn launch_command(&self, extra_args: &[String], prompt: Option<&str>) -> Option<Vec<String>> {
        Some(super::positional_prompt_argv(
            "opencode", extra_args, prompt,
        ))
    }

    fn install_hooks(&self) -> Result<HookInstallReport> {
        let path = opencode_plugin_path()?;
        OPENCODE_MANAGED_SOURCE.install_into(&path)
    }

    fn preview_hook_install(&self) -> Result<HookInstallPreview> {
        let path = opencode_plugin_path()?;
        OPENCODE_MANAGED_SOURCE.preview_at(&path)
    }

    fn uninstall_hooks(&self) -> Result<HookUninstallReport> {
        let path = opencode_plugin_path()?;
        OPENCODE_MANAGED_SOURCE.uninstall_from(&path)
    }

    fn hooks_installed(&self) -> bool {
        opencode_plugin_path().is_ok_and(|path| OPENCODE_MANAGED_SOURCE.installed_at(&path))
    }

    fn managed_hook_artifacts_present(&self) -> bool {
        self.hooks_installed()
    }

    fn probe_account(&self) -> crate::agents::account::AccountProbe {
        account::probe()
    }

    fn probe_oauth_usage(&self) -> crate::agents::OauthUsageProbe {
        crate::agents::credits::map_probe_snapshot(oauth_usage::fetch(), "opencode")
    }

    fn oauth_credentials_stamp(&self) -> Option<u64> {
        oauth_usage::credentials_stamp()
    }
}

/// Offline fallback window when the plugin's catalog-resolved
/// [`context_window`](payloads::OpencodeHookPayload::context_window) is absent —
/// Claude-family only. Every other model resolves through the plugin's model
/// catalog ([`plugin.ts`](./plugin.ts)); a bare id here stays unknown.
fn context_window_for(model: Option<&str>) -> Option<u64> {
    let model = model?.trim().to_ascii_lowercase();
    if model.is_empty() {
        return None;
    }
    if model.contains("[1m]") || (model.contains("1m") && model.contains("claude")) {
        return Some(1_000_000);
    }
    if model.starts_with("claude-") || model.contains("/claude-") {
        return Some(200_000);
    }
    None
}

fn opencode_plugin_path() -> Result<PathBuf> {
    if let Some(raw) = std::env::var_os("RIMZ_OPENCODE_PLUGIN").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(raw));
    }
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .map(|home| home.join(".config"))
        })
        .ok_or_else(|| AgentErr::Install {
            agent: "opencode",
            reason: "$HOME is not set; cannot resolve ~/.config/opencode/plugin/rimz.ts".to_owned(),
        })?;
    Ok(config_home.join("opencode/plugin/rimz.ts"))
}

#[cfg(test)]
mod tests;
