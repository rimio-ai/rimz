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
pub(crate) mod payloads;
pub(crate) mod spend;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, ConcernCoverage, IntegrationConcern, PlanLabel,
    RemoteControlCapability, ThreadKey, ToolClassification,
};
use super::lifecycle::LifecycleSignal;
use super::pricing::PriceBook;
use super::{
    AgentAdapter, AgentErr, AgentLifecycleObservation, ClassifiedHook, HookInstallPreview,
    HookInstallReport, HookUninstallReport, Result, SubagentIdentity, choice_is_allow,
    classify_agent_hook, read_optional_file, resolve_subagent_identity, sanitize_user_prompt,
};
use crate::feed::{FeedItem, FeedKind, Resolution};
use crate::ids::AgentSessionId;
use crate::ledger::atomic;

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
        rate_limit_windows: false,
        rich_context: false,
        context_usage: true,
        account_spend: true,
        subagents: true,
        background_tasks: false,
        registers_lazily: true,
        hook_install: true,
        remote_control: RemoteControlCapability {
            pane_sessions: false,
            background_sessions: false,
        },
    },
    coverage: OPENCODE_COVERAGE,
    default_context_window: None,
    default_model: None,
    hook_cap: Duration::from_secs(120),
    process_names: &["opencode", "bun"],
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
        IntegrationConcern::RichContext,
        ConcernCoverage::Unsupported {
            reason: "per-launch random-port server has no discovery surface",
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
            via: "SQLite message store + auth.json",
        },
    ),
    (
        IntegrationConcern::RemoteControl,
        ConcernCoverage::Unsupported {
            reason: "no remote-control surface",
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
const RIMZ_MANAGED_MARKER: &str = "_rimz_managed";

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
        observation.model = parsed.model.clone();
        observation.effort = parsed.effort;
        observation.context_window = parsed
            .context_window
            .or_else(|| context_window_for(parsed.model.as_deref()));
        observation.total_tokens = parsed.total_tokens;
        observation.cache_read_input_tokens = parsed.cache_read_input_tokens;
        observation.fresh_input_tokens =
            match (parsed.input_tokens, parsed.cache_write_input_tokens) {
                (Some(input), Some(cache_write)) => Some(input.saturating_add(cache_write)),
                (Some(input), None) => Some(input),
                (None, Some(cache_write)) => Some(cache_write),
                (None, None) => None,
            };
        observation.output_tokens = parsed.output_tokens;
        Some(observation)
    }

    fn moves_on(&self, event_name: &str) -> bool {
        matches!(
            event_name,
            "chat_message" | "session_idle" | "session_error"
        )
    }

    fn transcript_files(&self) -> Vec<PathBuf> {
        spend::opencode_db_files()
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
        install_into(&path)
    }

    fn preview_hook_install(&self) -> Result<HookInstallPreview> {
        let path = opencode_plugin_path()?;
        preview_install_at(&path)
    }

    fn uninstall_hooks(&self) -> Result<HookUninstallReport> {
        let path = opencode_plugin_path()?;
        uninstall_from(&path)
    }

    fn hooks_installed(&self) -> bool {
        opencode_plugin_path().is_ok_and(|path| hooks_installed_at(&path))
    }

    fn managed_hook_artifacts_present(&self) -> bool {
        self.hooks_installed()
    }

    fn probe_account(&self) -> crate::agents::account::AccountProbe {
        account::probe()
    }
}

fn context_window_for(model: Option<&str>) -> Option<u64> {
    let model = model?.trim().to_ascii_lowercase();
    if model.is_empty() {
        return None;
    }
    if model.contains("[1m]") || model.contains("1m") && model.contains("claude") {
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

fn file_is_rimz_managed(content: &str) -> bool {
    content
        .lines()
        .next()
        .is_some_and(|line| line.contains(RIMZ_MANAGED_MARKER))
}

fn refuse_unmarked(path: &Path, original: Option<&str>) -> Result<()> {
    match original {
        Some(existing) if !file_is_rimz_managed(existing) => Err(AgentErr::Install {
            agent: "opencode",
            reason: format!(
                "refusing to overwrite an unmarked user plugin at {}; move it aside or remove it to let Rimz manage this file",
                path.display()
            ),
        }),
        _ => Ok(()),
    }
}

fn install_into(path: &Path) -> Result<HookInstallReport> {
    let original = read_optional_file("opencode", path)?;
    refuse_unmarked(path, original.as_deref())?;
    atomic::write_bytes_atomically(path, PLUGIN_SOURCE.as_bytes())?;
    Ok(HookInstallReport {
        agent: "opencode",
        config_path: path.to_path_buf(),
        installed_events: installed_event_names(),
        merged: original.is_some(),
    })
}

fn preview_install_at(path: &Path) -> Result<HookInstallPreview> {
    let original = read_optional_file("opencode", path)?;
    refuse_unmarked(path, original.as_deref())?;
    Ok(HookInstallPreview {
        agent: "opencode",
        config_path: path.to_path_buf(),
        planned_events: installed_event_names(),
        merged: original.is_some(),
        original_config: original,
        candidate_config: PLUGIN_SOURCE.to_owned(),
        status_line_change: None,
        subagent_status_line_change: None,
    })
}

fn uninstall_from(path: &Path) -> Result<HookUninstallReport> {
    let original = read_optional_file("opencode", path)?;
    let existed = original.is_some();
    let mut removed_events = Vec::new();
    if original.as_deref().is_some_and(file_is_rimz_managed) {
        std::fs::remove_file(path).map_err(|source| AgentErr::InstallIo {
            agent: "opencode",
            path: path.to_path_buf(),
            source,
        })?;
        removed_events = installed_event_names();
    }
    Ok(HookUninstallReport {
        agent: "opencode",
        config_path: path.to_path_buf(),
        removed_events,
        existed,
    })
}

fn hooks_installed_at(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|content| file_is_rimz_managed(&content))
}

fn installed_event_names() -> Vec<String> {
    WIRED_EVENTS
        .iter()
        .map(|event| (*event).to_owned())
        .collect()
}

#[cfg(test)]
mod tests;
