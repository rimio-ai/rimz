//! Kiro CLI v3 lifecycle adapter.
//!
//! Kiro's four stock-TUI command hooks provide root-session presence, prompt,
//! mutating-tool, and turn-end signals. The upstream hook payload schema is
//! unpublished, so the installer stamps the trigger on argv and the payload
//! parser stays tolerant until live fixtures can pin the wire.

mod install;
mod payloads;
#[cfg(test)]
mod tests;

use std::path::Path;

use serde_json::Value;
#[cfg(test)]
use serde_json::json;

use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, ConcernCoverage, HookCoverage, IntegrationConcern,
    PlanLabel, RealtimeUsageChannel, RemoteControlCapability, ThreadKey, ToolClassification,
};
use super::lifecycle::{LifecycleSignal, LifecycleSignalKind};
use super::{
    AgentAdapter, AgentLifecycleObservation, ClassifiedHook, HookInstallPreview, HookInstallReport,
    HookUninstallReport, Result, classify_agent_hook, non_empty_trimmed, sanitize_user_prompt,
    stop_payload_errored,
};
use crate::ids::AgentSessionId;

static KIRO_DESCRIPTOR: AgentDescriptor = AgentDescriptor {
    kind: "kiro",
    bin_names: &["kiro-cli"],
    display_name: "Kiro",
    brand: Brand {
        emblem: None,
        color: 92,
        color_rgb: (0x79, 0x0e, 0xcb),
    },
    plan_label: PlanLabel::Prefixed { prefix: "Kiro" },
    sub_providers: &[],
    tools: ToolClassification {
        mutating: &["fs_write", "str_replace"],
        editing: &["fs_write", "str_replace"],
        blocking: &[],
    },
    capabilities: Capabilities {
        blocking_asks: false,
        // Kiro can draw native prompts, but v3 exposes no hook that records
        // them for Rimz routing.
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
    coverage: KIRO_COVERAGE,
    lifecycle_hooks: KIRO_LIFECYCLE_HOOKS,
    default_context_window: None,
    default_model: None,
    // `kiro-cli` launches the session; the v3 chat engine lives in the separate
    // `kiro-cli-chat` binary the launcher execs into, so a live pane can read as
    // either. `kiro-cli-term` is the figterm shell-integration daemon (it runs
    // for every integrated shell, not just agent panes), so it is deliberately
    // excluded to avoid false-positive presence.
    process_names: &["kiro-cli", "kiro-cli-chat"],
    extra_bin_dirs: &[],
    activity_events: WIRED_EVENTS,
    hook_install_unavailable: None,
    thread_key: ThreadKey::PerFile,
};

const KIRO_COVERAGE: &[(IntegrationConcern, ConcernCoverage)] = &[
    (
        IntegrationConcern::TurnLifecycle,
        ConcernCoverage::Wired {
            via: "SessionStart/UserPromptSubmit/Stop",
        },
    ),
    (
        IntegrationConcern::Permission,
        ConcernCoverage::Unsupported {
            reason: "no v3 hook announces the native ask; PreToolUse fires before policy decides",
        },
    ),
    (
        IntegrationConcern::PlanApproval,
        ConcernCoverage::Unsupported {
            reason: "no v3 plan-approval hook",
        },
    ),
    (
        IntegrationConcern::UserQuestion,
        ConcernCoverage::Unsupported {
            reason: "no v3 native-question hook",
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
        ConcernCoverage::Unsupported {
            reason: "no stock-TUI compaction hook; compaction rotates the session id",
        },
    ),
    (
        IntegrationConcern::Subagents,
        ConcernCoverage::Unsupported {
            reason: "v3 hooks do not fire in subagents and publish no child lifecycle",
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
            gap: "no SessionEnd hook; v3 Stop is turn end, not session end",
        },
    ),
    (
        IntegrationConcern::IdleNotification,
        ConcernCoverage::Partial {
            via: "turn boundaries + stall window",
            gap: "no idle Notification event",
        },
    ),
    (
        IntegrationConcern::ContextUsage,
        ConcernCoverage::Unsupported {
            reason: "no machine-readable context usage surface",
        },
    ),
    (
        IntegrationConcern::RealtimeCost,
        ConcernCoverage::Unsupported {
            reason: "credit-metered; no machine-readable usage surface",
        },
    ),
    (
        IntegrationConcern::RichContext,
        ConcernCoverage::Unsupported {
            reason: "hooks publish no model, effort, context, or transcript contract",
        },
    ),
    (
        IntegrationConcern::HookInstall,
        ConcernCoverage::Wired {
            via: "~/.kiro/hooks/rimz.json",
        },
    ),
    (
        IntegrationConcern::AccountSpend,
        ConcernCoverage::Unsupported {
            reason: "whoami schema and credit ledger are unpublished",
        },
    ),
    (
        IntegrationConcern::RemoteControl,
        ConcernCoverage::Unsupported {
            reason: "no stock-TUI remote-control surface",
        },
    ),
];

const KIRO_LIFECYCLE_HOOKS: &[(LifecycleSignalKind, HookCoverage)] = &[
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
        HookCoverage::Absent {
            reason: "no v3 hook announces native prompts",
        },
    ),
    (
        LifecycleSignalKind::SubagentStarted,
        HookCoverage::Absent {
            reason: "v3 hooks do not fire in subagents",
        },
    ),
    (
        LifecycleSignalKind::SubagentStopped,
        HookCoverage::Absent {
            reason: "v3 hooks do not fire in subagents",
        },
    ),
    (
        LifecycleSignalKind::Compacting,
        HookCoverage::Absent {
            reason: "no stock-TUI compaction hook",
        },
    ),
    (
        LifecycleSignalKind::CompactionEnded,
        HookCoverage::Absent {
            reason: "no stock-TUI compaction hook",
        },
    ),
    (
        LifecycleSignalKind::Ended,
        HookCoverage::Derived {
            via: "pane liveness + rollup reaper",
            gap: "Stop ends a turn, not the session",
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

const LIFECYCLE_EVENTS: &[&str] = &["SessionStart", "UserPromptSubmit", "PostToolUse", "Stop"];
const WIRED_EVENTS: &[&str] = LIFECYCLE_EVENTS;

#[derive(Clone, Debug, Default)]
pub struct KiroAdapter;

impl AgentAdapter for KiroAdapter {
    fn descriptor(&self) -> &'static AgentDescriptor {
        &KIRO_DESCRIPTOR
    }

    fn classify_hook(&self, event_name: &str, _payload: &Value) -> ClassifiedHook {
        classify_agent_hook(event_name, None, LIFECYCLE_EVENTS)
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
                json!({ "session_id": "s", "cwd": "/tmp/work" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "UserPromptSubmit",
                json!({ "session_id": "s", "prompt": "fix auth" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "PostToolUse",
                json!({ "session_id": "s", "tool_name": "fs_write" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "Stop",
                json!({ "session_id": "s" }),
                AgentHookClass::Lifecycle,
                None,
            ),
        ]
    }

    fn render_neutral(&self, _event_name: &str) -> Result<Option<Value>> {
        // Kiro adds stdout to model context on SessionStart and
        // UserPromptSubmit; keep every event silent for one safe contract.
        Ok(None)
    }

    fn observe_lifecycle(
        &self,
        event_name: &str,
        payload: &Value,
    ) -> Option<AgentLifecycleObservation> {
        let parsed = payloads::parse_payload(payload);
        let signal = match event_name {
            "SessionStart" => LifecycleSignal::Registered,
            "UserPromptSubmit" => LifecycleSignal::TurnStarted,
            "PostToolUse"
                if parsed
                    .tool_name
                    .as_deref()
                    .is_some_and(|name| self.descriptor().tools.mutating.contains(&name)) =>
            {
                LifecycleSignal::ToolUsed {
                    mutates: true,
                    edits: parsed
                        .tool_name
                        .as_deref()
                        .is_some_and(|name| self.descriptor().tools.editing.contains(&name)),
                }
            }
            "Stop" => LifecycleSignal::TurnEnded {
                errored: stop_payload_errored(payload),
                parked_on_background: false,
            },
            _ => return None,
        };
        let agent_id = parsed
            .session_id
            .as_deref()
            .and_then(non_empty_trimmed)
            .map(AgentSessionId::from);
        let mut observation =
            AgentLifecycleObservation::new(agent_id, signal).with_worktree_from_payload(payload);
        observation.task = sanitize_user_prompt(parsed.prompt.as_deref());
        observation.prompt = sanitize_user_prompt(parsed.prompt.as_deref());
        Some(observation)
    }

    fn moves_on(&self, event_name: &str) -> bool {
        matches!(event_name, "UserPromptSubmit" | "Stop")
    }

    fn resume_command(&self, session_id: &str, _cwd: &Path) -> Option<Vec<String>> {
        Some(vec![
            "kiro-cli".to_owned(),
            "chat".to_owned(),
            "--v3".to_owned(),
            "--resume-id".to_owned(),
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
        if preset.system_prompt_file.is_some() {
            return Err(super::PresetErr::UnsupportedField {
                agent: "kiro",
                field: "system-prompt-file",
            });
        }
        if preset.append_system_prompt_file.is_some() {
            return Err(super::PresetErr::UnsupportedField {
                agent: "kiro",
                field: "append-system-prompt-file",
            });
        }
        let mut argv = Vec::new();
        if let Some(model) = preset.model.as_deref().filter(|value| !value.is_empty()) {
            argv.extend(["--model".to_owned(), model.to_owned()]);
        }
        if let Some(effort) = preset.effort.as_deref().filter(|value| !value.is_empty()) {
            argv.extend(["--effort".to_owned(), effort.to_owned()]);
        }
        Ok(argv)
    }

    fn preset_arg_matcher(&self, field: super::PresetField) -> Option<super::PresetArgMatcher> {
        let flag = match field {
            super::PresetField::Model => "--model",
            super::PresetField::Effort => "--effort",
            super::PresetField::SystemPromptFile | super::PresetField::AppendSystemPromptFile => {
                return None;
            }
        };
        Some(super::PresetArgMatcher::Flag(vec![flag.to_owned()]))
    }

    fn launch_command(&self, extra_args: &[String], prompt: Option<&str>) -> Option<Vec<String>> {
        // Profile flags belong to `chat`; putting them after the root-level
        // `--v3` shortcut makes clap reject them before chat starts.
        let mut args = vec!["chat".to_owned(), "--v3".to_owned()];
        args.extend(extra_args.iter().cloned());
        Some(super::positional_prompt_argv("kiro-cli", &args, prompt))
    }

    fn install_hooks(&self) -> Result<HookInstallReport> {
        install::install_into(&install::hooks_path()?)
    }

    fn preview_hook_install(&self) -> Result<HookInstallPreview> {
        install::preview_at(&install::hooks_path()?)
    }

    fn uninstall_hooks(&self) -> Result<HookUninstallReport> {
        install::uninstall_from(&install::hooks_path()?)
    }

    fn hooks_installed(&self) -> bool {
        install::hooks_path().is_ok_and(|path| install::installed_at(&path))
    }

    fn managed_hook_artifacts_present(&self) -> bool {
        install::hooks_path().is_ok_and(|path| install::managed_at(&path))
    }
}
