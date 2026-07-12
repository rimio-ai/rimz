//! Factory Droid native-hook adapter.
//!
//! Droid's stock `settings.json` hooks expose basic session, turn, tool, and
//! compaction lifecycle. The wire carries no structured asks, error outcome,
//! subagent identity, usage, cost, or account surface, so those capabilities
//! remain explicitly absent.

mod install;
mod payloads;

use std::path::Path;

use serde_json::Value;

use self::install::{
    droid_settings_path, hooks_installed_at, install_into, managed_artifacts_at,
    preview_install_at, uninstall_from,
};
use self::payloads::{parse_session_start, parse_user_prompt_submit};
use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, ConcernCoverage, HookCoverage, IntegrationConcern,
    PlanLabel, RealtimeUsageChannel, RemoteControlCapability, ThreadKey, ToolClassification,
};
use super::hook_types::SessionSource;
use super::lifecycle::{LifecycleSignal, LifecycleSignalKind};
use super::{
    AgentAdapter, AgentLifecycleObservation, ClassifiedHook, HookInstallPreview, HookInstallReport,
    HookUninstallReport, Result, SessionOrigin, classify_agent_hook, optional_payload_string,
    sanitize_user_prompt,
};
use crate::harness::run::PermissionMode;
use crate::ids::AgentSessionId;

static DROID_DESCRIPTOR: AgentDescriptor = AgentDescriptor {
    kind: "droid",
    display_name: "Droid",
    brand: Brand {
        emblem: "
 ▄▄▄▄▄
▐ ● ● ▌
 ▀▀▀▀▀",
        color: 252,
        color_rgb: (0xd8, 0xd8, 0xd8),
    },
    plan_label: PlanLabel::TitleCaseOnly,
    sub_providers: &[],
    tools: ToolClassification {
        mutating: &["Create", "Edit", "ApplyPatch", "Execute"],
        editing: &["Create", "Edit", "ApplyPatch"],
        blocking: &[],
    },
    capabilities: Capabilities {
        blocking_asks: false,
        // Droid renders native permission prompts, but its hooks expose no
        // structured prompt event RimZ can route or answer.
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
    coverage: DROID_COVERAGE,
    lifecycle_hooks: DROID_LIFECYCLE_HOOKS,
    default_context_window: None,
    default_model: None,
    process_names: &["droid"],
    bin_names: &["droid"],
    extra_bin_dirs: &[],
    activity_events: &["SessionStart", "UserPromptSubmit", "PostToolUse", "Stop"],
    hook_install_unavailable: None,
    thread_key: ThreadKey::PerFile,
};

const DROID_COVERAGE: &[(IntegrationConcern, ConcernCoverage)] = &[
    (
        IntegrationConcern::TurnLifecycle,
        ConcernCoverage::Wired {
            via: "SessionStart/UserPromptSubmit/Stop",
        },
    ),
    (
        IntegrationConcern::Permission,
        ConcernCoverage::Unsupported {
            reason: "no PermissionRequest hook or structured Notification discriminator",
        },
    ),
    (
        IntegrationConcern::PlanApproval,
        ConcernCoverage::Unsupported {
            reason: "no plan-approval hook; spec-mode exit is invisible",
        },
    ),
    (
        IntegrationConcern::UserQuestion,
        ConcernCoverage::Unsupported {
            reason: "no question hook",
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
            via: "PreCompact/SessionStart:compact",
        },
    ),
    (
        IntegrationConcern::Subagents,
        ConcernCoverage::Unsupported {
            reason: "SubagentStop carries no child identity",
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
        ConcernCoverage::Wired { via: "SessionEnd" },
    ),
    (
        IntegrationConcern::IdleNotification,
        ConcernCoverage::Wired {
            via: "Notification",
        },
    ),
    (
        IntegrationConcern::ContextUsage,
        ConcernCoverage::Unsupported {
            reason: "no token/context hook fields; transcript schema unpublished",
        },
    ),
    (
        IntegrationConcern::RealtimeCost,
        ConcernCoverage::Unsupported {
            reason: "no cost surface",
        },
    ),
    (
        IntegrationConcern::RichContext,
        ConcernCoverage::Unsupported {
            reason: "statusLine is user presentation, not an observation API",
        },
    ),
    (
        IntegrationConcern::HookInstall,
        ConcernCoverage::Wired {
            via: "~/.factory/settings.json",
        },
    ),
    (
        IntegrationConcern::AccountSpend,
        ConcernCoverage::Unsupported {
            reason: "no machine-readable auth or usage surface",
        },
    ),
    (
        IntegrationConcern::RemoteControl,
        ConcernCoverage::Unsupported {
            reason: "no remote-control surface",
        },
    ),
];

const DROID_LIFECYCLE_HOOKS: &[(LifecycleSignalKind, HookCoverage)] = &[
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
            reason: "no permission or question hook",
        },
    ),
    (
        LifecycleSignalKind::SubagentStarted,
        HookCoverage::Absent {
            reason: "no child identity",
        },
    ),
    (
        LifecycleSignalKind::SubagentStopped,
        HookCoverage::Absent {
            reason: "no child identity",
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
            event: "SessionStart",
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

const DROID_HOOK_TIMEOUT_SECS: u64 = 10;
const INSTALLED_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PostToolUse",
    "Notification",
    "Stop",
    "PreCompact",
    "SessionEnd",
];
const LIFECYCLE_EVENTS: &[&str] = INSTALLED_EVENTS;
const HOOKS_KEY: &str = "hooks";
const RIMZ_MANAGED_KEY: &str = "_rimz_managed";
const RIMZ_HOOK_COMMAND: &str = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source droid";
const RIMZ_HOOK_MARKER: &str = "rimz hooks feed --source droid";

#[derive(Clone, Debug, Default)]
pub struct DroidAdapter;

impl AgentAdapter for DroidAdapter {
    fn descriptor(&self) -> &'static AgentDescriptor {
        &DROID_DESCRIPTOR
    }

    fn classify_hook(&self, event_name: &str, _payload: &Value) -> ClassifiedHook {
        classify_agent_hook(event_name, None, LIFECYCLE_EVENTS)
    }

    #[cfg(test)]
    fn installed_hook_events(&self) -> Vec<&'static str> {
        INSTALLED_EVENTS.to_vec()
    }

    #[cfg(test)]
    fn classification_corpus(&self) -> Vec<super::ClassificationSample> {
        use super::{AgentHookClass, ClassificationSample};

        let lifecycle = |event_name, payload| {
            ClassificationSample::new(event_name, payload, AgentHookClass::Lifecycle, None)
        };
        vec![
            lifecycle(
                "SessionStart",
                serde_json::json!({"session_id": "sess-1", "source": "startup"}),
            ),
            lifecycle(
                "SessionStart",
                serde_json::json!({"session_id": "sess-1", "source": "compact"}),
            ),
            lifecycle(
                "UserPromptSubmit",
                serde_json::json!({"session_id": "sess-1", "prompt": "fix auth"}),
            ),
            lifecycle(
                "PostToolUse",
                serde_json::json!({"session_id": "sess-1", "tool_name": "Edit"}),
            ),
            lifecycle("Notification", serde_json::json!({"session_id": "sess-1"})),
            lifecycle("Stop", serde_json::json!({"session_id": "sess-1"})),
            lifecycle(
                "PreCompact",
                serde_json::json!({"session_id": "sess-1", "trigger": "manual"}),
            ),
            lifecycle(
                "SessionEnd",
                serde_json::json!({"session_id": "sess-1", "reason": "logout"}),
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
        if matches!(observation.signal, LifecycleSignal::Registered)
            && session_start.as_ref().is_some_and(|start| {
                matches!(start.source, SessionSource::Startup | SessionSource::Clear)
            })
        {
            observation.origin = Some(SessionOrigin::Fresh);
        }
        Some(observation)
    }

    fn ends_session(&self, event_name: &str) -> bool {
        event_name == "SessionEnd"
    }

    fn moves_on(&self, event_name: &str) -> bool {
        matches!(event_name, "Stop" | "UserPromptSubmit")
    }

    fn resume_command(&self, session_id: &str, _cwd: &Path) -> Option<Vec<String>> {
        Some(vec![
            "droid".to_owned(),
            "--resume".to_owned(),
            session_id.to_owned(),
        ])
    }

    fn fork_command(&self, session_id: &str, _cwd: &Path) -> Option<Vec<String>> {
        Some(vec![
            "droid".to_owned(),
            "--fork".to_owned(),
            session_id.to_owned(),
        ])
    }

    fn permission_args(&self, mode: PermissionMode) -> Vec<String> {
        match mode {
            PermissionMode::Auto => vec!["--auto".to_owned(), "medium".to_owned()],
            PermissionMode::Plan => vec!["--use-spec".to_owned()],
            // Stock interactive mode keeps Droid's configured autonomy and
            // native permission UI. The CLI exposes no interactive equivalent
            // of exec's unsafe bypass, so an empty yolo posture remains
            // unsupported in the shared layout parser.
            PermissionMode::Ask | PermissionMode::Yolo => Vec::new(),
        }
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
                agent: "droid",
                field: "effort",
            });
        }
        if preset.system_prompt_file.is_some() {
            return Err(super::PresetErr::UnsupportedField {
                agent: "droid",
                field: "system-prompt-file",
            });
        }
        if let Some(path) = preset.append_system_prompt_file.as_deref() {
            argv.extend([
                "--append-system-prompt-file".to_owned(),
                path.to_string_lossy().into_owned(),
            ]);
        }
        Ok(argv)
    }

    fn launch_command(&self, extra_args: &[String], prompt: Option<&str>) -> Option<Vec<String>> {
        Some(super::positional_prompt_argv("droid", extra_args, prompt))
    }

    fn install_hooks(&self) -> Result<HookInstallReport> {
        install_into(&droid_settings_path()?)
    }

    fn preview_hook_install(&self) -> Result<HookInstallPreview> {
        preview_install_at(&droid_settings_path()?)
    }

    fn uninstall_hooks(&self) -> Result<HookUninstallReport> {
        uninstall_from(&droid_settings_path()?)
    }

    fn hooks_installed(&self) -> bool {
        droid_settings_path().is_ok_and(|path| hooks_installed_at(&path))
    }

    fn managed_hook_artifacts_present(&self) -> bool {
        droid_settings_path().is_ok_and(|path| managed_artifacts_at(&path))
    }
}

#[cfg(test)]
mod tests;
