//! Kiro CLI v3 launch and process-presence adapter.
//!
//! Kiro CLI 2.12.1 does not execute the documented standalone hook configs in
//! a stock v3 session. Keep launch, resume, and process identity available;
//! lifecycle and hook installation stay unsupported until a pinned release
//! provides a reproducible native signal.

mod install;
#[cfg(test)]
mod tests;

use std::path::Path;

use serde_json::Value;

use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, ConcernCoverage, HookCoverage, IntegrationConcern,
    PlanLabel, RealtimeUsageChannel, RemoteControlCapability, ThreadKey, ToolClassification,
};
use super::lifecycle::LifecycleSignalKind;
use super::{
    AgentAdapter, AgentErr, AgentHookClass, ClassifiedHook, HookInstallPreview, HookInstallReport,
    HookUninstallReport, Result,
};

const HOOK_INSTALL_UNAVAILABLE: &str = "Kiro CLI 2.12.1 v3 does not execute standalone hook configs; re-enable after a pinned v3 release provides a reproducible native hook contract";

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
        mutating: &[],
        editing: &[],
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
        hook_install: false,
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
    activity_events: &[],
    hook_install_unavailable: Some(HOOK_INSTALL_UNAVAILABLE),
    thread_key: ThreadKey::PerFile,
};

const KIRO_COVERAGE: &[(IntegrationConcern, ConcernCoverage)] = &[
    (
        IntegrationConcern::TurnLifecycle,
        ConcernCoverage::Unsupported {
            reason: "Kiro CLI 2.12.1 v3 exposes no verified executable turn-lifecycle signal",
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
        ConcernCoverage::Unsupported {
            reason: "no verified lifecycle hook or native idle notification",
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
        ConcernCoverage::Unsupported {
            reason: HOOK_INSTALL_UNAVAILABLE,
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
        HookCoverage::Absent {
            reason: "Kiro CLI 2.12.1 v3 did not execute the documented SessionStart hook config",
        },
    ),
    (
        LifecycleSignalKind::TurnStarted,
        HookCoverage::Absent {
            reason: "Kiro CLI 2.12.1 v3 did not execute the documented UserPromptSubmit hook config",
        },
    ),
    (
        LifecycleSignalKind::TurnEnded,
        HookCoverage::Absent {
            reason: "Kiro CLI 2.12.1 v3 did not execute the documented Stop hook config",
        },
    ),
    (
        LifecycleSignalKind::ToolUsed,
        HookCoverage::Absent {
            reason: "Kiro CLI 2.12.1 v3 did not execute the documented PostToolUse hook config",
        },
    ),
    (
        LifecycleSignalKind::AwaitingInput,
        HookCoverage::Absent {
            reason: "no verified v3 hook announces native prompts",
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

#[derive(Clone, Debug, Default)]
pub struct KiroAdapter;

impl AgentAdapter for KiroAdapter {
    fn descriptor(&self) -> &'static AgentDescriptor {
        &KIRO_DESCRIPTOR
    }

    fn classify_hook(&self, event_name: &str, _payload: &Value) -> ClassifiedHook {
        ClassifiedHook {
            class: AgentHookClass::Unknown,
            ask_kind: None,
            event_name: event_name.to_owned(),
        }
    }

    fn render_neutral(&self, _event_name: &str) -> Result<Option<Value>> {
        // Keep defensive manual feeds silent; if a future pinned release makes
        // documented hooks executable, stdout may become an agent input.
        Ok(None)
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
        Err(AgentErr::Install {
            agent: self.descriptor().kind,
            reason: HOOK_INSTALL_UNAVAILABLE.to_owned(),
        })
    }

    fn preview_hook_install(&self) -> Result<HookInstallPreview> {
        Err(AgentErr::Install {
            agent: self.descriptor().kind,
            reason: HOOK_INSTALL_UNAVAILABLE.to_owned(),
        })
    }

    fn uninstall_hooks(&self) -> Result<HookUninstallReport> {
        install::uninstall_from(&install::hooks_path()?)
    }

    fn hooks_installed(&self) -> bool {
        false
    }

    fn managed_hook_artifacts_present(&self) -> bool {
        install::hooks_path().is_ok_and(|path| install::managed_at(&path))
    }
}
