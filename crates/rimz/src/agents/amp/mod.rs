//! Amp plugin adapter.
//!
//! Amp has no command-hook or statusline protocol. Rimz installs a small
//! observation-only TypeScript plugin that forwards the active thread's native
//! lifecycle events without entering Amp's tool-decision path.

pub(crate) mod account;
pub(crate) mod payloads;

use std::path::{Path, PathBuf};

use serde_json::Value;
#[cfg(test)]
use serde_json::json;

use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, ConcernCoverage, HookCoverage, IntegrationConcern,
    PlanLabel, RealtimeUsageChannel, RemoteControlCapability, ThreadKey, ToolClassification,
};
use super::lifecycle::{LifecycleSignal, LifecycleSignalKind};
use super::managed_source::ManagedSource;
use super::{
    AgentAdapter, AgentErr, AgentLifecycleObservation, AskKind, ClassifiedHook, HookInstallPreview,
    HookInstallReport, HookUninstallReport, PresetErr, Result, SessionOrigin, classify_agent_hook,
    sanitize_user_prompt,
};
use crate::ids::AgentSessionId;

static AMP_DESCRIPTOR: AgentDescriptor = AgentDescriptor {
    kind: "amp",
    display_name: "Amp",
    brand: Brand {
        emblem: "
 /\\
/__\\",
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
        blocking_asks: true,
        native_ask_ui: true,
        rich_context: false,
        transcript_tail_context: false,
        context_usage: false,
        account_spend: false,
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
    coverage: AMP_COVERAGE,
    lifecycle_hooks: AMP_LIFECYCLE_HOOKS,
    default_context_window: None,
    default_model: None,
    process_names: &["amp", "node"],
    bin_names: &["amp"],
    extra_bin_dirs: &[],
    activity_events: &["session_start", "agent_start", "tool_result", "agent_end"],
    hook_install_unavailable: None,
    thread_key: ThreadKey::PerFile,
};

const AMP_COVERAGE: &[(IntegrationConcern, ConcernCoverage)] = &[
    (
        IntegrationConcern::TurnLifecycle,
        ConcernCoverage::Wired {
            via: "agent_start/agent_end",
        },
    ),
    (
        IntegrationConcern::Permission,
        ConcernCoverage::Wired {
            via: "thread-state awaiting-approval",
        },
    ),
    (
        IntegrationConcern::PlanApproval,
        ConcernCoverage::Unsupported {
            reason: "no native event",
        },
    ),
    (
        IntegrationConcern::UserQuestion,
        ConcernCoverage::Unsupported {
            reason: "no native event",
        },
    ),
    (
        IntegrationConcern::Answer,
        ConcernCoverage::Unsupported {
            reason: "no external resolver",
        },
    ),
    (
        IntegrationConcern::Compaction,
        ConcernCoverage::Unsupported {
            reason: "automatic compaction has no event",
        },
    ),
    (
        IntegrationConcern::Subagents,
        ConcernCoverage::Unsupported {
            reason: "interactive events expose no durable child identity",
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
        ConcernCoverage::Partial {
            via: "pane liveness + rollup reaper",
            gap: "no session-end event",
        },
    ),
    (
        IntegrationConcern::IdleNotification,
        ConcernCoverage::Partial {
            via: "turn-end + awaiting-approval + stall window",
            gap: "no notification event",
        },
    ),
    (
        IntegrationConcern::ContextUsage,
        ConcernCoverage::Unsupported {
            reason: "plugin transcript omits usage",
        },
    ),
    (
        IntegrationConcern::RealtimeCost,
        ConcernCoverage::Unsupported {
            reason: "amp usage is human text",
        },
    ),
    (
        IntegrationConcern::RichContext,
        ConcernCoverage::Unsupported {
            reason: "no out-of-band context transport",
        },
    ),
    (
        IntegrationConcern::HookInstall,
        ConcernCoverage::Wired {
            via: "~/.config/amp/plugins/rimz.ts",
        },
    ),
    (
        IntegrationConcern::AccountSpend,
        ConcernCoverage::Unsupported {
            reason: "no machine-readable spend surface",
        },
    ),
    (
        IntegrationConcern::RemoteControl,
        ConcernCoverage::Unsupported {
            reason: "readiness is not detectable",
        },
    ),
];

const AMP_LIFECYCLE_HOOKS: &[(LifecycleSignalKind, HookCoverage)] = &[
    (
        LifecycleSignalKind::Registered,
        HookCoverage::Native {
            event: "session_start",
        },
    ),
    (
        LifecycleSignalKind::TurnStarted,
        HookCoverage::Native {
            event: "agent_start",
        },
    ),
    (
        LifecycleSignalKind::TurnEnded,
        HookCoverage::Native { event: "agent_end" },
    ),
    (
        LifecycleSignalKind::ToolUsed,
        HookCoverage::Native {
            event: "tool_result",
        },
    ),
    (
        LifecycleSignalKind::AwaitingInput,
        HookCoverage::Native {
            event: "permission_ask",
        },
    ),
    (
        LifecycleSignalKind::SubagentStarted,
        HookCoverage::Absent {
            reason: "no interactive subagent event",
        },
    ),
    (
        LifecycleSignalKind::SubagentStopped,
        HookCoverage::Absent {
            reason: "no interactive subagent event",
        },
    ),
    (
        LifecycleSignalKind::Compacting,
        HookCoverage::Absent {
            reason: "automatic compaction has no event",
        },
    ),
    (
        LifecycleSignalKind::CompactionEnded,
        HookCoverage::Absent {
            reason: "automatic compaction has no event",
        },
    ),
    (
        LifecycleSignalKind::Ended,
        HookCoverage::Derived {
            via: "pane liveness + rollup reaper",
            gap: "no session-end event",
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

const LIFECYCLE_EVENTS: &[&str] = &["session_start", "agent_start", "tool_result", "agent_end"];
const WIRED_EVENTS: &[&str] = &[
    "session_start",
    "agent_start",
    "tool_result",
    "agent_end",
    "permission_ask",
];
const PLUGIN_SOURCE: &str = include_str!("plugin.ts");
const AMP_MANAGED_SOURCE: ManagedSource = ManagedSource {
    agent: "amp",
    source: PLUGIN_SOURCE,
    wired_events: WIRED_EVENTS,
    artifact_noun: "plugin",
};

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
    fn installed_hook_events(&self) -> Vec<&'static str> {
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
        if event_name == "session_start" {
            // Amp's Fresh lineage means fresh pane occupancy, not a fresh
            // conversation: focusing an existing thread must supersede the
            // previously focused thread in the same pane.
            observation.origin = Some(SessionOrigin::Fresh);
        }
        Some(observation)
    }

    fn moves_on(&self, event_name: &str) -> bool {
        matches!(event_name, "agent_start" | "agent_end")
    }

    fn resume_command(&self, session_id: &str, _cwd: &Path) -> Option<Vec<String>> {
        Some(vec![
            "amp".to_owned(),
            "threads".to_owned(),
            "continue".to_owned(),
            session_id.to_owned(),
        ])
    }

    fn fork_command(&self, _session_id: &str, _cwd: &Path) -> Option<Vec<String>> {
        None
    }

    fn compact_command(&self) -> Option<&'static str> {
        None
    }

    fn render_preset(
        &self,
        preset: &super::LaunchPreset,
    ) -> std::result::Result<Vec<String>, PresetErr> {
        let mut argv = Vec::new();
        if let Some(model) = preset.model.as_deref().filter(|value| !value.is_empty()) {
            argv.extend(["--mode".to_owned(), model.to_owned()]);
        }
        if let Some(effort) = preset.effort.as_deref().filter(|value| !value.is_empty()) {
            argv.extend(["--effort".to_owned(), effort.to_owned()]);
        }
        if preset.system_prompt_file.is_some() {
            return Err(PresetErr::UnsupportedField {
                agent: "amp",
                field: "system-prompt-file",
            });
        }
        if preset.append_system_prompt_file.is_some() {
            return Err(PresetErr::UnsupportedField {
                agent: "amp",
                field: "append-system-prompt-file",
            });
        }
        Ok(argv)
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

    fn install_hooks(&self) -> Result<HookInstallReport> {
        AMP_MANAGED_SOURCE.install_into(&amp_plugin_path()?)
    }

    fn preview_hook_install(&self) -> Result<HookInstallPreview> {
        AMP_MANAGED_SOURCE.preview_at(&amp_plugin_path()?)
    }

    fn uninstall_hooks(&self) -> Result<HookUninstallReport> {
        AMP_MANAGED_SOURCE.uninstall_from(&amp_plugin_path()?)
    }

    fn hooks_installed(&self) -> bool {
        amp_plugin_path().is_ok_and(|path| AMP_MANAGED_SOURCE.installed_at(&path))
    }

    fn managed_hook_artifacts_present(&self) -> bool {
        self.hooks_installed()
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

#[cfg(test)]
mod tests;
