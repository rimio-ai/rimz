//! OpenCode hook adapter.
//!
//! OpenCode loads TypeScript plugins in-process inside each embedded server.
//! RimZ ships one plugin (`plugin.ts`) that shells out to
//! `rimz hooks feed --source opencode`, posts a RimZ-owned snake_case payload
//! on stdin. Current permission and question prompts arrive through
//! `permission.asked` and `question.asked` bus events; the compatibility
//! `permission.ask` hook reads stdout on older releases and leaves OpenCode's
//! `output.status` at `ask` on the neutral path. Native reply bus events record
//! answers and clear waiting after the user responds in OpenCode's own TUI.
//! A root `session_idle` after a plan-agent turn derives a native plan-approval
//! wait that the next prompt clears after the user switches modes in the TUI.
//! `/new` registers a fresh root inside the same live process; follow-latest
//! succession hands pane and card ownership to the new conversation.
//! `session.deleted` and the server-scoped `dispose` sweep normalize to one
//! per-session `session_ended` event, with pane liveness as the crash backstop.

pub(crate) mod account;
mod database;
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
    AgentDescriptor, Brand, Capabilities, ConcernCoverage, HookCoverage, IntegrationCoverage,
    LifecycleCoverage, PlanLabel, RealtimeUsageChannel, RemoteControlCapability, ThreadKey,
    ToolClassification,
};
use super::hook_types::{HookRecord, decode_catalog_hook, hook_record};
use super::lifecycle::LifecycleSignal;
use super::managed_source::ManagedSource;
use super::pricing::PriceBook;
use super::{
    AgentAdapter, AgentErr, AgentLifecycleObservation, DecodedHook, HookRouting,
    LifecycleRefreshCtx, RefreshSpawn, RefreshTrigger, Result, SubagentIdentity,
    optional_payload_string, resolve_subagent_identity, sanitize_user_prompt,
};
#[cfg(test)]
use crate::harness::run::PermissionMode;
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
    expected_windows: &[],
    tools: ToolClassification {
        mutating: &["bash", "edit", "write", "apply_patch", "patch"],
        editing: &["edit", "write", "apply_patch", "patch"],
        blocking: &[],
    },
    capabilities: Capabilities {
        native_ask_ui: true,
        transcript_tail_context: false,
        registers_lazily: true,
        local_session_discovery: false,
        daemon_hooked_sessions: false,
        direct_account_usage: true,
        same_pane_session: super::SamePaneSessionPolicy::FollowLatest,
        realtime_usage: RealtimeUsageChannel {
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
    thread_key: ThreadKey::PerFile,
    launch: super::LaunchSpec {
        program: Some("opencode"),
        fixed_args: &[],
        prompt: super::PromptStyle::PositionalAfterDoubleDash,
        resume: Some(super::SessionCommand {
            before_id: &["opencode", "--session"],
            after_id: &[],
        }),
        fork: Some(super::SessionCommand {
            before_id: &["opencode", "--session"],
            after_id: &["--fork"],
        }),
        permission: super::LaunchPermissionArgs {
            ask: &[],
            auto: &[],
            yolo: &["--auto"],
            plan: &["--agent", "plan"],
        },
        ping_args: None,
        max_turn_flag: None,
        compact_command: Some("/compact"),
        presets: super::PresetMatchers {
            model: Some(super::StaticPresetMatcher::Flag(&["--model", "-m"])),
            ..super::PresetMatchers::EMPTY
        },
    },
};

const OPENCODE_COVERAGE: IntegrationCoverage = IntegrationCoverage {
    turn_lifecycle: ConcernCoverage::Wired {
        via: "session_created/chat_message/session_idle",
    },
    permission: ConcernCoverage::Wired {
        via: "permission_ask",
    },
    plan_approval: ConcernCoverage::Wired {
        via: "session_idle + resting plan-agent turn",
    },
    user_question: ConcernCoverage::Wired {
        via: "question.asked",
    },
    answer: ConcernCoverage::Unsupported {
        reason: "native answers are observed; RimZ-to-OpenCode answer transport is not mapped",
    },
    compaction: ConcernCoverage::Wired {
        via: "session_compacting/session_compacted",
    },
    subagents: ConcernCoverage::Wired {
        via: "SubagentStart/SubagentStop",
    },
    background_parking: ConcernCoverage::Unsupported {
        reason: "no background-task parking",
    },
    session_end: ConcernCoverage::Wired {
        via: "session_ended (session.deleted + dispose sweep)",
    },
    idle_notification: ConcernCoverage::Partial {
        via: "turn-end + permission.ask/question.asked + stall window",
        gap: "no idle Notification hook; no idle-timeout nudge",
    },
    context_usage: ConcernCoverage::Wired {
        via: "message.updated token split",
    },
    realtime_cost: ConcernCoverage::Wired {
        via: "authoritative per-session SQLite message spend sum, reconciled at each turn boundary",
    },
    rich_context: ConcernCoverage::Wired {
        via: "embedded server /config/providers + /session over plugin serverUrl",
    },
    hook_install: ConcernCoverage::Wired {
        via: "~/.config/opencode/plugin/rimz.ts",
    },
    account_spend: ConcernCoverage::Wired {
        via: "SQLite message store + auth.json OAuth usage probe",
    },
    remote_control: ConcernCoverage::Unsupported {
        reason: "no remote-control surface",
    },
};

const OPENCODE_LIFECYCLE_HOOKS: LifecycleCoverage = LifecycleCoverage {
    registered: HookCoverage::Native {
        event: "session_created",
    },
    turn_started: HookCoverage::Native {
        event: "chat_message",
    },
    turn_ended: HookCoverage::Native {
        event: "session_idle",
    },
    tool_used: HookCoverage::Native {
        event: "tool_after",
    },
    awaiting_input: HookCoverage::Native {
        // One representative installed event names the signal, the Codex
        // precedent; `question_ask` is the other awaiting-user event and
        // rides the separately-wired `UserQuestion` concern.
        event: "permission_ask",
    },
    subagent_started: HookCoverage::Native {
        event: "SubagentStart",
    },
    subagent_stopped: HookCoverage::Native {
        event: "SubagentStop",
    },
    compacting: HookCoverage::Native {
        event: "session_compacting",
    },
    compaction_ended: HookCoverage::Native {
        event: "session_compacted",
    },
    ended: HookCoverage::Native {
        event: "session_ended",
    },
    lost: HookCoverage::Derived {
        via: "rimz exec wrapper",
        gap: "native hooks do not report mux-session death",
    },
};

const OPENCODE_HOOKS: &[HookRecord] = &[
    hook_record!(
        lifecycle,
        "session_created",
        r#"{"session_id":"ses_1","cwd":"/tmp/repo"}"#
    )
    .progress(),
    hook_record!(
        lifecycle,
        "chat_message",
        r#"{"session_id":"ses_1","prompt":"fix auth"}"#
    )
    .progress(),
    hook_record!(lifecycle, "session_idle", r#"{"session_id":"ses_1"}"#).progress(),
    hook_record!(
        lifecycle,
        "session_error",
        r#"{"session_id":"ses_1","error_message":"boom"}"#
    )
    .progress(),
    hook_record!(
        lifecycle,
        "tool_after",
        r#"{"session_id":"ses_1","tool_name":"bash"}"#
    )
    .progress(),
    hook_record!(lifecycle, "session_compacting", r#"{"session_id":"ses_1"}"#),
    hook_record!(lifecycle, "session_compacted", r#"{"session_id":"ses_1"}"#),
    hook_record!(
        lifecycle,
        "SubagentStart",
        r#"{"session_id":"ses_child","parent_session_id":"ses_parent","prompt":"review auth"}"#
    )
    .progress(),
    hook_record!(
        lifecycle,
        "SubagentStop",
        r#"{"session_id":"ses_child","parent_session_id":"ses_parent"}"#
    )
    .progress(),
    hook_record!(
        blocking,
        "permission_ask",
        r#"{"session_id":"ses_1","tool_name":"bash"}"#,
        AskKind::Permission
    )
    .synchronous(),
    hook_record!(
        blocking,
        "question_ask",
        r#"{"session_id":"ses_1","title":"Which database?"}"#,
        AskKind::Question
    )
    .synchronous(),
    hook_record!(
        lifecycle,
        "permission_replied",
        r#"{"session_id":"ses_1","reply":"once"}"#
    ),
    hook_record!(
        lifecycle,
        "question_replied",
        r#"{"session_id":"ses_1","answers":[["Postgres"]]}"#
    ),
    hook_record!(lifecycle, "question_rejected", r#"{"session_id":"ses_1"}"#),
    hook_record!(
        lifecycle,
        "session_ended",
        r#"{"session_id":"ses_1","reason":"deleted"}"#
    )
    .session_ended(),
];

const PLUGIN_SOURCE: &str = include_str!("plugin.ts");
const OPENCODE_MANAGED_SOURCE: ManagedSource = ManagedSource::new(
    "opencode",
    PLUGIN_SOURCE,
    OPENCODE_HOOKS,
    "plugin",
    opencode_plugin_path,
    true,
);

#[derive(Clone, Debug, Default)]
pub struct OpencodeAdapter;

impl AgentAdapter for OpencodeAdapter {
    fn descriptor(&self) -> &'static AgentDescriptor {
        &OPENCODE_DESCRIPTOR
    }

    fn decode_hook(&self, event_name: &str, payload: &Value) -> Result<DecodedHook> {
        let parsed = payloads::parse_payload(payload);
        let ask_kind = match event_name {
            "permission_ask" => Some(AskKind::Permission),
            "question_ask" => Some(AskKind::Question),
            "session_idle" if parsed.plan_proposed == Some(true) => Some(AskKind::PlanApproval),
            _ => None,
        };
        let mut decoded = decode_catalog_hook(OPENCODE_HOOKS, event_name, ask_kind);
        decoded.set_routing(
            HookRouting::session(parsed.session_id.clone().map(Into::into))
                .with_worktree(optional_payload_string(payload, &["worktree_path", "cwd"])),
        );
        let questions = if event_name == "question_ask" {
            parsed
                .questions
                .clone()
                .unwrap_or_default()
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
                .collect()
        } else {
            Vec::new()
        };
        let ask_detail = questions
            .first()
            .and_then(|question| question.question.lines().next())
            .map(ToOwned::to_owned)
            .filter(|detail| !detail.is_empty());
        decoded.set_ask(questions, ask_detail);
        decoded.set_native_answers(match event_name {
            "permission_replied" => parsed.reply.as_deref().and_then(|reply| {
                let reply = reply.trim().to_owned();
                (!reply.is_empty()).then_some(vec![AskAnswer {
                    question: None,
                    chosen: vec![reply],
                    note: None,
                }])
            }),
            "question_replied" => {
                let answers: Vec<_> = parsed
                    .answers
                    .as_ref()
                    .into_iter()
                    .flatten()
                    .filter_map(|choices| {
                        let chosen: Vec<_> = choices
                            .iter()
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
        });
        let signal = match event_name {
            "session_created" => Some(LifecycleSignal::Registered),
            "permission_ask" => Some(LifecycleSignal::AwaitingInput {
                kind: AskKind::Permission,
                ask_id: None,
                detail: parsed.title.clone(),
                native_key: None,
            }),
            "question_ask" => Some(LifecycleSignal::AwaitingInput {
                kind: AskKind::Question,
                ask_id: None,
                detail: parsed.title.clone(),
                native_key: None,
            }),
            "chat_message" => Some(LifecycleSignal::TurnStarted),
            "session_idle" if parsed.plan_proposed == Some(true) => {
                Some(LifecycleSignal::AwaitingInput {
                    kind: AskKind::PlanApproval,
                    ask_id: None,
                    detail: None,
                    native_key: None,
                })
            }
            "session_idle" => Some(LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            }),
            "session_error" => Some(LifecycleSignal::TurnEnded {
                errored: true,
                parked_on_background: false,
            }),
            "permission_replied" | "question_replied" | "question_rejected" => {
                Some(LifecycleSignal::ToolUsed {
                    mutates: false,
                    edits: false,
                    native_key: None,
                })
            }
            "tool_after" if self.descriptor().tool_mutates(payload) => {
                Some(LifecycleSignal::ToolUsed {
                    mutates: true,
                    edits: self.descriptor().tool_edits_files(payload),
                    native_key: None,
                })
            }
            "session_compacting" => Some(LifecycleSignal::Compacting),
            "session_compacted" => Some(LifecycleSignal::CompactionEnded { auto: None }),
            "SubagentStart" => Some(LifecycleSignal::SubagentStarted),
            "SubagentStop" => Some(LifecycleSignal::SubagentStopped {
                errored: payloads::errored(&parsed),
            }),
            "session_ended" => Some(LifecycleSignal::Ended),
            _ => None,
        };
        let Some(signal) = signal else {
            return Ok(decoded);
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
                SubagentIdentity::Quarantined => return Ok(decoded),
            }
        } else {
            (parsed.session_id.as_deref().map(AgentSessionId::from), None)
        };
        let mut observation =
            AgentLifecycleObservation::new(agent_id, signal).with_worktree_from_payload(payload);
        observation.transcript_path = optional_payload_string(payload, &["transcript_path"]);
        observation.parent_agent_id = parent_agent_id;
        observation.task = sanitize_user_prompt(parsed.prompt.as_deref());
        observation.prompt = sanitize_user_prompt(parsed.prompt.as_deref());
        observation.launch.model = parsed.model.clone();
        observation.launch.effort = parsed.effort;
        observation.usage.context_window = parsed
            .context_window
            .or_else(|| context_window_for(parsed.model.as_deref()));
        observation.usage.total_tokens = parsed.total_tokens;
        observation.usage.cache_read_input_tokens = parsed.cache_read_input_tokens;
        observation.usage.cache_write_input_tokens = parsed.cache_write_input_tokens;
        observation.usage.fresh_input_tokens = parsed.input_tokens;
        observation.usage.output_tokens = parsed.output_tokens;
        decoded.set_final_message(if matches!(event_name, "session_idle" | "session_error") {
            observation.agent_id.as_ref().and_then(|session_id| {
                let path = observation
                    .transcript_path
                    .as_deref()
                    .map(Path::new)
                    .filter(|path| path.is_file())
                    .map(Path::to_path_buf)
                    .or_else(|| database::files().into_iter().next())?;
                transcript::last_assistant_message(&path, session_id)
            })
        } else {
            None
        });
        decoded.attach_lifecycle(observation);
        Ok(decoded)
    }

    #[cfg(test)]
    fn native_hook_events(&self) -> Vec<&'static str> {
        super::hook_types::catalog_event_names(OPENCODE_HOOKS)
    }

    #[cfg(test)]
    fn classification_corpus(&self) -> Vec<super::ClassificationSample> {
        use super::{AgentHookClass, ClassificationSample};

        let mut samples = super::hook_types::catalog_classification_corpus(OPENCODE_HOOKS);
        samples.push(ClassificationSample::new(
            "session_idle",
            json!({ "session_id": "ses_1", "plan_proposed": true }),
            AgentHookClass::AwaitingUser,
            Some(AskKind::PlanApproval),
        ));
        samples
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

    fn transcript_stat(&self, path: &Path) -> Option<crate::agents::TranscriptStat> {
        database::logical_stat(path)
    }

    fn spending_sources(&self) -> Vec<crate::agents::spending::SpendingSource> {
        database::data_dirs()
            .into_iter()
            .filter_map(|root| {
                let primary =
                    crate::agents::spending::SpendingSourceTree::new(root.clone(), "opencode.db")?;
                let channels =
                    crate::agents::spending::SpendingSourceTree::new(root, "opencode-*.db")?
                        .filtered("opencode-channel", database::is_channel_relative);
                Some(crate::agents::spending::SpendingSource::first(vec![
                    primary, channels,
                ]))
            })
            .collect()
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

    fn managed_integration(&self) -> Option<&'static dyn super::ManagedIntegration> {
        Some(&OPENCODE_MANAGED_SOURCE)
    }

    fn probe_account(&self) -> crate::agents::account::AccountProbe {
        account::probe()
    }

    fn probe_account_usage(&self) -> crate::agents::AccountUsageProbe {
        account::probe_usage()
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
