//! OpenCode hook adapter.
//!
//! OpenCode loads TypeScript plugins in-process inside each embedded server.
//! Rimz ships one plugin (`plugin.ts`) that shells out to
//! `rimz hooks feed --source opencode`, posts a Rimz-owned snake_case payload
//! on stdin, and reads stdout only for the blocking `permission.ask` hook. The
//! neutral path leaves OpenCode's `output.status` at `ask`, so the native TUI
//! dialog remains the human fallback. Session end is reconstructed from pane
//! liveness and the rollup reaper because OpenCode's `dispose` hook is
//! server-scoped and carries no session id.

pub(crate) mod account;
pub(crate) mod oauth_usage;
pub(crate) mod payloads;
pub mod server;
pub(crate) mod spend;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

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
    Result, SubagentIdentity, choice_is_allow, classify_agent_hook, resolve_subagent_identity,
    sanitize_user_prompt,
};
use crate::feed::{FeedItem, FeedKind, Resolution};
use crate::ids::AgentSessionId;

static OPENCODE_DESCRIPTOR: AgentDescriptor = AgentDescriptor {
    kind: "opencode",
    display_name: "Open Code",
    brand: Brand {
        emblem: "
 ▗▛▀▀▀▜▖
▝▜▌ █ ▐▛▘
 ▝▀▀▀▀▀▘",
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
        blocking_feed: true,
        native_ask_ui: true,
        rich_context: true,
        transcript_tail_context: false,
        context_usage: true,
        account_spend: true,
        subagents: true,
        background_tasks: false,
        registers_lazily: true,
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
    coverage: OPENCODE_COVERAGE,
    lifecycle_hooks: OPENCODE_LIFECYCLE_HOOKS,
    default_context_window: None,
    default_model: None,
    hook_cap: Duration::from_secs(120),
    process_names: &["opencode", "bun"],
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
        ConcernCoverage::Unsupported {
            reason: "question tool has no contracted bus event in 1.15.13",
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
        ConcernCoverage::Partial {
            via: "pane liveness + rollup reaper",
            gap: "dispose is server-scoped and carries no session id",
        },
    ),
    (
        IntegrationConcern::IdleNotification,
        ConcernCoverage::Partial {
            via: "turn-end + permission.ask + stall window",
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
        ConcernCoverage::Partial {
            via: "SQLite message spend sum",
            gap: "reconstructed on turn-end, not a provider-pushed realtime figure",
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
        HookCoverage::Derived {
            via: "pane liveness + rollup reaper",
            gap: "dispose is server-scoped and carries no session id",
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
        let feed_kind = (event_name == "permission_ask").then_some(FeedKind::Permission);
        classify_agent_hook(event_name, feed_kind, LIFECYCLE_EVENTS)
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
                AgentHookClass::BlockingFeed,
                Some(FeedKind::Permission),
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

    fn render_decision(&self, item: &FeedItem, resolution: &Resolution) -> Result<Value> {
        match item.kind {
            FeedKind::Permission => {
                if choice_is_allow(resolution) {
                    Ok(json!({ "status": "allow" }))
                } else {
                    let reason = resolution
                        .reason
                        .clone()
                        .or_else(|| {
                            resolution
                                .decision
                                .get("reason")
                                .and_then(Value::as_str)
                                .map(ToOwned::to_owned)
                        })
                        .unwrap_or_else(|| "denied by resolver".to_owned());
                    Ok(json!({ "status": "deny", "reason": reason }))
                }
            }
            other => Err(AgentErr::Render {
                agent: "opencode",
                reason: format!("unsupported feed kind {other:?}"),
            }),
        }
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
            "chat_message" => LifecycleSignal::TurnStarted,
            "session_idle" => LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
            "session_error" => LifecycleSignal::TurnEnded {
                errored: true,
                parked_on_background: false,
            },
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

    fn moves_on(&self, event_name: &str) -> bool {
        matches!(
            event_name,
            "chat_message" | "session_idle" | "session_error"
        )
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

    fn launch_command(&self, extra_args: &[String], prompt: Option<&str>) -> Option<Vec<String>> {
        let mut argv = vec!["opencode".to_owned()];
        argv.extend(extra_args.iter().cloned());
        if let Some(prompt) = prompt.filter(|value| !value.is_empty()) {
            argv.push(prompt.to_owned());
        }
        Some(argv)
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
        crate::agents::credits::map_probe_snapshot(oauth_usage::fetch(), "opencode.oauth_usage")
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
