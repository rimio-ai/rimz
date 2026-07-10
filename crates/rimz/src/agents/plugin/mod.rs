//! Process-backed third-party agent adapters.
//!
//! A machine-tier manifest supplies static descriptor and launch data. The
//! agent's own shim translates native events to [`protocol`] JSON, while
//! optional bounded executables provide pull-only enrichment.

mod check;
mod load;
mod manifest;
mod probes;
mod protocol;

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde_json::{Value, json};
use tracing::{debug, warn};

use self::manifest::{PluginManifest, TranscriptThreadKey};
use self::protocol::{CanonicalEvent, CompactionTrigger, Envelope};
#[cfg(test)]
use super::AskKind;
use super::account::AccountProbe;
use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, ConcernCoverage, HookCoverage, IntegrationConcern,
    PlanLabel, RealtimeUsageChannel, RemoteControlCapability, ThreadKey, ToolClassification,
};
use super::lifecycle::{LifecycleSignal, LifecycleSignalKind};
use super::observation::{payload_context_pct, payload_total_tokens};
use super::spending::{SpendCursor, SpendParse};
use super::{
    AgentAdapter, AgentContext, AgentHookClass, AgentLifecycleObservation, ClassifiedHook,
    LaunchPreset, PresetErr, PriceBook, Result, RootIdentity, SubagentIdentity,
    positional_prompt_argv, resolve_root_identity, resolve_subagent_identity,
};
use crate::harness::run::PermissionMode;
use crate::transcript::AskQuestion;

pub use check::{
    PluginCheckReport, PluginCheckSummary, ProbeCheckReport, ProbeCheckStatus, ReplayCheckReport,
    ReplayFinalState, ReplayRow, check_from_root,
};
pub use load::{
    LoadedPlugins, PluginDiagnostic, PluginLoadError, ProbeDiagnostic, load_from_root, loaded,
    plugins_root,
};
pub use manifest::valid_kind;

const GENERIC_EMBLEM: &str = "[agent]";

pub struct PluginAdapter {
    manifest: &'static PluginManifest,
    plugin_dir: &'static Path,
    descriptor: &'static AgentDescriptor,
}

fn build_adapter(manifest: PluginManifest, plugin_dir: PathBuf) -> &'static PluginAdapter {
    // ponytail: process-lifetime plugin config is leaked once; move registry
    // APIs to owned Arcs if live manifest reload becomes a product feature.
    let manifest = Box::leak(Box::new(manifest));
    let plugin_dir = Box::leak(plugin_dir.into_boxed_path());
    let descriptor = Box::leak(Box::new(build_descriptor(manifest, plugin_dir)));
    Box::leak(Box::new(PluginAdapter {
        manifest,
        plugin_dir,
        descriptor,
    }))
}

impl AgentAdapter for PluginAdapter {
    fn descriptor(&self) -> &'static AgentDescriptor {
        self.descriptor
    }

    fn classify_hook(&self, event_name: &str, payload: &Value) -> ClassifiedHook {
        let Some(envelope) = Envelope::parse(event_name, payload) else {
            return unknown(event_name);
        };
        if !self.emits(event_name) {
            warn!(
                kind = self.descriptor.kind,
                event = event_name,
                "agent plugin emitted an undeclared canonical event"
            );
        }
        let (class, ask_kind) = match envelope.event {
            CanonicalEvent::Unknown => (AgentHookClass::Unknown, None),
            CanonicalEvent::AwaitingInput { ask, .. } => (AgentHookClass::AwaitingUser, Some(ask)),
            _ => (AgentHookClass::Lifecycle, None),
        };
        ClassifiedHook {
            class,
            ask_kind,
            event_name: event_name.to_owned(),
        }
    }

    #[cfg(test)]
    fn installed_hook_events(&self) -> Vec<&'static str> {
        let manifest: &'static PluginManifest = self.manifest;
        manifest.emits.iter().map(String::as_str).collect()
    }

    #[cfg(test)]
    fn classification_corpus(&self) -> Vec<super::ClassificationSample> {
        use super::ClassificationSample;

        let manifest: &'static PluginManifest = self.manifest;
        let mut samples = Vec::new();
        for event in &manifest.emits {
            let event = event.as_str();
            let mut payload = json!({
                "protocol": 1,
                "hook_event_name": event,
                "session_id": "root"
            });
            if matches!(event, "subagent_start" | "subagent_end") {
                payload["agent_id"] = json!("child");
            }
            if event == "tool_use" {
                payload["tool_name"] = json!(
                    manifest
                        .tools
                        .mutating
                        .first()
                        .map(String::as_str)
                        .unwrap_or("read")
                );
            }
            if event == "awaiting_input" {
                for (ask, kind) in [
                    ("permission", AskKind::Permission),
                    ("plan_approval", AskKind::PlanApproval),
                    ("question", AskKind::Question),
                ] {
                    let mut ask_payload = payload.clone();
                    ask_payload["ask"] = json!(ask);
                    samples.push(ClassificationSample::new(
                        event,
                        ask_payload,
                        AgentHookClass::AwaitingUser,
                        Some(kind),
                    ));
                }
            } else {
                samples.push(ClassificationSample::new(
                    event,
                    payload,
                    AgentHookClass::Lifecycle,
                    None,
                ));
            }
        }
        samples
    }

    fn render_neutral(&self, _event_name: &str) -> Result<Option<Value>> {
        Ok(None)
    }

    fn observe_lifecycle(
        &self,
        event_name: &str,
        payload: &Value,
    ) -> Option<AgentLifecycleObservation> {
        let envelope = Envelope::parse(event_name, payload)?;
        let mut turn_error = None;
        let signal = match &envelope.event {
            CanonicalEvent::SessionStart => LifecycleSignal::Registered,
            CanonicalEvent::TurnStart { .. } => LifecycleSignal::TurnStarted,
            CanonicalEvent::TurnEnd {
                errored,
                error_message,
                ..
            } => {
                if *errored {
                    turn_error = Some(super::AgentTurnError {
                        at: Timestamp::now(),
                        label: error_message.clone(),
                        ..super::AgentTurnError::default()
                    });
                }
                LifecycleSignal::TurnEnded {
                    errored: *errored,
                    parked_on_background: false,
                }
            }
            CanonicalEvent::ToolUse {
                tool_name,
                is_error,
            } => {
                let mutates = !is_error
                    && tool_name
                        .as_deref()
                        .is_some_and(|name| self.descriptor.tools.mutating.contains(&name));
                let edits = mutates
                    && tool_name
                        .as_deref()
                        .is_some_and(|name| self.descriptor.tools.editing.contains(&name));
                LifecycleSignal::ToolUsed { mutates, edits }
            }
            CanonicalEvent::AwaitingInput { ask, .. } => LifecycleSignal::AwaitingInput {
                kind: *ask,
                ask_id: None,
                detail: None,
            },
            CanonicalEvent::CompactionStart => LifecycleSignal::Compacting,
            CanonicalEvent::CompactionEnd { trigger } => LifecycleSignal::CompactionEnded {
                auto: trigger.map(|trigger| matches!(trigger, CompactionTrigger::Auto)),
            },
            CanonicalEvent::SubagentStart => LifecycleSignal::SubagentStarted,
            CanonicalEvent::SubagentEnd { errored } => {
                LifecycleSignal::SubagentStopped { errored: *errored }
            }
            CanonicalEvent::SessionEnd => LifecycleSignal::Ended,
            CanonicalEvent::Context | CanonicalEvent::Unknown => return None,
        };
        let (agent_id, parent_agent_id) = match envelope.event {
            CanonicalEvent::SubagentStart | CanonicalEvent::SubagentEnd { .. } => {
                match resolve_subagent_identity(
                    self.descriptor.kind,
                    event_name,
                    envelope.agent_id.as_deref(),
                    envelope.session_id.as_deref(),
                    payload,
                ) {
                    SubagentIdentity::Resolved {
                        agent_id,
                        parent_agent_id,
                    } => (Some(agent_id), Some(parent_agent_id)),
                    SubagentIdentity::Quarantined => return None,
                }
            }
            _ => match resolve_root_identity(
                self.descriptor.kind,
                event_name,
                envelope.agent_id.as_deref(),
                envelope.session_id.as_deref(),
            ) {
                RootIdentity::Root { agent_id } => (agent_id, None),
                RootIdentity::ForeignChild => return None,
            },
        };
        let mut observation =
            AgentLifecycleObservation::new(agent_id, signal).with_worktree_from_payload(payload);
        observation.turn_error = turn_error;
        observation.parent_agent_id = parent_agent_id;
        observation.launch.model = envelope.model;
        observation.launch.effort = envelope.effort;
        observation.context_pct =
            payload_context_pct(payload, envelope.context_pct.map(|pct| pct.min(100) as u8));
        observation.context_window = envelope.context_window;
        observation.total_tokens = payload_total_tokens(payload, envelope.total_tokens);
        observation.fresh_input_tokens = envelope.input_tokens;
        observation.output_tokens = envelope.output_tokens;
        observation.cache_read_input_tokens = envelope.cache_read_input_tokens;
        observation.cache_write_input_tokens = envelope.cache_write_input_tokens;
        observation.transcript_path = envelope.transcript_path;
        if observation.worktree_path.is_none() {
            observation.worktree_path = envelope.cwd;
        }
        if let CanonicalEvent::TurnStart { prompt } = envelope.event {
            observation.task = prompt.clone();
            observation.prompt = prompt;
        }
        Some(observation)
    }

    fn observe_context(&self, source: &str, payload: &Value) -> Option<AgentContext> {
        let envelope = Envelope::parse("context", payload)?;
        normalize_context(source, payload, &envelope)
    }

    fn last_assistant_message(
        &self,
        event_name: &str,
        payload: &Value,
        _observation: &AgentLifecycleObservation,
    ) -> Option<String> {
        let envelope = Envelope::parse(event_name, payload)?;
        match envelope.event {
            CanonicalEvent::TurnEnd {
                last_assistant_message,
                ..
            } => last_assistant_message,
            _ => None,
        }
    }

    fn ask_question_detail(&self, event_name: &str, payload: &Value) -> Option<Vec<AskQuestion>> {
        let envelope = Envelope::parse(event_name, payload)?;
        let CanonicalEvent::AwaitingInput { question, .. } = envelope.event else {
            return None;
        };
        let question = question?.trim().to_owned();
        (!question.is_empty()).then_some(vec![AskQuestion {
            question,
            options: Vec::new(),
            multi_select: false,
            has_option_previews: false,
        }])
    }

    fn ends_session(&self, event_name: &str) -> bool {
        event_name == "session_end"
    }

    fn moves_on(&self, event_name: &str) -> bool {
        matches!(event_name, "turn_start" | "turn_end")
    }

    fn transcript_files(&self) -> Vec<PathBuf> {
        let Some(transcripts) = &self.manifest.transcripts else {
            return Vec::new();
        };
        let mut files = Vec::new();
        for pattern in &transcripts.globs {
            let pattern = expand_pattern(self.plugin_dir, pattern);
            match glob::glob(&pattern) {
                Ok(paths) => files.extend(paths.filter_map(std::result::Result::ok)),
                Err(err) => {
                    debug!(kind = self.descriptor.kind, %err, "invalid plugin transcript glob")
                }
            }
        }
        files.retain(|path| path.is_file());
        files.sort();
        files.dedup();
        files
    }

    fn parse_spend(
        &self,
        path: &Path,
        resume: Option<&SpendCursor>,
        _prices: &PriceBook,
    ) -> SpendParse {
        let Some(argv) = self.manifest.probes.spend.as_deref() else {
            return SpendParse::default();
        };
        probes::spend(self.descriptor.kind, self.plugin_dir, argv, path, resume)
    }

    fn probe_account(&self) -> AccountProbe {
        let Some(argv) = self.manifest.probes.account.as_deref() else {
            return AccountProbe::LoggedOut;
        };
        probes::account(self.descriptor.kind, self.plugin_dir, argv)
    }

    fn probe_oauth_usage(&self) -> super::OauthUsageProbe {
        let Some(argv) = self.manifest.probes.account.as_deref() else {
            return super::OauthUsageProbe::Unsupported;
        };
        probes::account_usage(self.descriptor.kind, self.plugin_dir, argv)
    }

    fn oauth_account_key(&self) -> Option<String> {
        let argv = self.manifest.probes.account.as_deref()?;
        probes::account_key(self.descriptor.kind, self.plugin_dir, argv)
    }

    fn probe_version(&self) -> Option<String> {
        let argv = self.manifest.probes.version.as_deref()?;
        probes::version(self.descriptor.kind, self.plugin_dir, argv)
    }

    fn permission_args(&self, mode: PermissionMode) -> Vec<String> {
        let Some(launch) = &self.manifest.launch else {
            return Vec::new();
        };
        match mode {
            PermissionMode::Ask => launch.permission_args.ask.clone(),
            PermissionMode::Auto => launch.permission_args.auto.clone(),
            PermissionMode::Yolo => launch.permission_args.yolo.clone(),
            PermissionMode::Plan => launch.permission_args.plan.clone(),
        }
    }

    fn compact_command(&self) -> Option<&'static str> {
        self.manifest.launch.as_ref()?.compact_command.as_deref()
    }

    fn render_preset(&self, preset: &LaunchPreset) -> std::result::Result<Vec<String>, PresetErr> {
        let mut args = Vec::new();
        let launch = self.manifest.launch.as_ref();
        render_flag(
            &mut args,
            self.descriptor.kind,
            "model",
            preset.model.as_deref(),
            launch.and_then(|launch| launch.model_flag.as_deref()),
        )?;
        render_flag(
            &mut args,
            self.descriptor.kind,
            "effort",
            preset.effort.as_deref(),
            launch.and_then(|launch| launch.effort_flag.as_deref()),
        )?;
        if preset.system_prompt_file.is_some() {
            return Err(PresetErr::UnsupportedField {
                agent: self.descriptor.kind,
                field: "system-prompt-file",
            });
        }
        if preset.append_system_prompt_file.is_some() {
            return Err(PresetErr::UnsupportedField {
                agent: self.descriptor.kind,
                field: "append-system-prompt-file",
            });
        }
        Ok(args)
    }

    fn launch_command(&self, extra_args: &[String], prompt: Option<&str>) -> Option<Vec<String>> {
        let launch = self.manifest.launch.as_ref()?;
        let mut args = launch.args.clone();
        args.extend_from_slice(extra_args);
        let bin = probes::resolve_executable(self.plugin_dir, &launch.bin)
            .to_string_lossy()
            .into_owned();
        Some(positional_prompt_argv(&bin, &args, prompt))
    }

    fn resume_command(&self, session_id: &str, _cwd: &Path) -> Option<Vec<String>> {
        let resume = self.manifest.launch.as_ref()?.resume.as_ref()?;
        let mut argv = resume
            .iter()
            .map(|arg| arg.replace("{session_id}", session_id))
            .collect::<Vec<_>>();
        argv[0] = probes::resolve_executable(self.plugin_dir, &argv[0])
            .to_string_lossy()
            .into_owned();
        Some(argv)
    }
}

impl PluginAdapter {
    fn emits(&self, event: &str) -> bool {
        self.manifest.emits.iter().any(|declared| declared == event)
    }
}

fn build_descriptor(
    manifest: &'static PluginManifest,
    plugin_dir: &'static Path,
) -> AgentDescriptor {
    let event = |name: &str| manifest.emits.iter().any(|event| event == name);
    let setup_doc = manifest::resolve_path(plugin_dir, &manifest.setup_doc);
    let hook_reason = leak_string(format!(
        "hook wiring is self-managed; see {}",
        setup_doc.display()
    ));
    let coverage = derive_coverage(manifest, hook_reason);
    let lifecycle_hooks = derive_lifecycle_hooks(manifest);
    let activity_events = manifest
        .emits
        .iter()
        .filter(|event| {
            matches!(
                event.as_str(),
                "session_start"
                    | "turn_start"
                    | "turn_end"
                    | "tool_use"
                    | "subagent_start"
                    | "subagent_end"
                    | "compaction_start"
                    | "compaction_end"
            )
        })
        .map(|event| leak_string(event.clone()))
        .collect();
    AgentDescriptor {
        kind: leak_string(manifest.kind.clone()),
        display_name: leak_string(manifest.display_name.clone()),
        brand: Brand {
            emblem: manifest
                .brand
                .emblem
                .as_ref()
                .map(|emblem| leak_string(emblem.clone()))
                .unwrap_or(GENERIC_EMBLEM),
            color: manifest.brand.color,
            color_rgb: manifest.brand.color_rgb.into(),
        },
        plan_label: PlanLabel::TitleCaseOnly,
        sub_providers: &[],
        tools: ToolClassification {
            mutating: leak_strings(&manifest.tools.mutating),
            editing: leak_strings(&manifest.tools.editing),
            blocking: &[],
        },
        capabilities: Capabilities {
            blocking_asks: event("awaiting_input"),
            native_ask_ui: manifest.capabilities.native_ask_ui,
            rich_context: event("context"),
            transcript_tail_context: false,
            context_usage: manifest.capabilities.context_usage || event("context"),
            account_spend: manifest.probes.spend.is_some(),
            subagents: manifest.capabilities.subagents,
            background_tasks: manifest.capabilities.background_tasks,
            registers_lazily: manifest.capabilities.registers_lazily,
            daemon_hooked_sessions: false,
            hook_install: false,
            realtime_usage: RealtimeUsageChannel {
                covers_account_while_live: false,
                windows_defer_to_fresh_realtime: false,
            },
            remote_control: RemoteControlCapability {
                pane_sessions: false,
                background_sessions: false,
            },
        },
        coverage,
        lifecycle_hooks,
        default_context_window: None,
        default_model: None,
        process_names: leak_strings(&manifest.process_names),
        bin_names: leak_strings(std::slice::from_ref(&manifest.kind)),
        extra_bin_dirs: &[],
        activity_events: leak_slice(activity_events),
        hook_install_unavailable: Some(hook_reason),
        thread_key: match manifest.transcripts.as_ref().map(|value| value.thread_key) {
            Some(TranscriptThreadKey::SessionDir) => ThreadKey::SessionDir,
            Some(TranscriptThreadKey::PerFile) | None => ThreadKey::PerFile,
        },
    }
}

fn derive_coverage(
    manifest: &PluginManifest,
    hook_reason: &'static str,
) -> &'static [(IntegrationConcern, ConcernCoverage)] {
    let has = |name: &str| manifest.emits.iter().any(|event| event == name);
    let turn = has("session_start") && has("turn_start") && has("turn_end");
    let asks = has("awaiting_input") && manifest.capabilities.native_ask_ui;
    let subagents = manifest.capabilities.subagents && has("subagent_start") && has("subagent_end");
    leak_slice(vec![
        (
            IntegrationConcern::TurnLifecycle,
            coverage(
                turn,
                "canonical session_start/turn_start/turn_end",
                "canonical turn events not declared",
            ),
        ),
        (
            IntegrationConcern::Permission,
            coverage(
                asks,
                "canonical awaiting_input",
                "canonical awaiting_input with native-ask-ui not declared",
            ),
        ),
        (
            IntegrationConcern::PlanApproval,
            coverage(
                asks,
                "canonical awaiting_input",
                "canonical awaiting_input with native-ask-ui not declared",
            ),
        ),
        (
            IntegrationConcern::UserQuestion,
            coverage(
                asks,
                "canonical awaiting_input",
                "canonical awaiting_input with native-ask-ui not declared",
            ),
        ),
        (
            IntegrationConcern::Answer,
            ConcernCoverage::Unsupported {
                reason: "plugin prompts are answered in the agent's own UI",
            },
        ),
        (
            IntegrationConcern::Compaction,
            coverage(
                has("compaction_start") && has("compaction_end"),
                "canonical compaction_start/compaction_end",
                "canonical compaction pair not declared",
            ),
        ),
        (
            IntegrationConcern::Subagents,
            coverage(
                subagents,
                "canonical subagent_start/subagent_end",
                "canonical subagent pair and capability not declared",
            ),
        ),
        (
            IntegrationConcern::BackgroundParking,
            ConcernCoverage::Unsupported {
                reason: "canonical protocol has no background-parking signal",
            },
        ),
        (
            IntegrationConcern::SessionEnd,
            coverage(
                has("session_end"),
                "canonical session_end",
                "canonical session_end not declared",
            ),
        ),
        (
            IntegrationConcern::IdleNotification,
            ConcernCoverage::Partial {
                via: "turn_end + stall window",
                gap: "canonical protocol has no idle notification",
            },
        ),
        (
            IntegrationConcern::ContextUsage,
            coverage(
                manifest.capabilities.context_usage || has("context"),
                "canonical context/gauge fields",
                "context usage not declared",
            ),
        ),
        (
            IntegrationConcern::RealtimeCost,
            coverage(
                has("context"),
                "canonical context total_cost_usd",
                "context event not declared",
            ),
        ),
        (
            IntegrationConcern::RichContext,
            coverage(
                has("context"),
                "canonical context event",
                "context event not declared",
            ),
        ),
        (
            IntegrationConcern::HookInstall,
            ConcernCoverage::Unsupported {
                reason: hook_reason,
            },
        ),
        (
            IntegrationConcern::AccountSpend,
            coverage(
                manifest.probes.spend.is_some(),
                "plugin spend probe",
                "spend probe not declared",
            ),
        ),
        (
            IntegrationConcern::RemoteControl,
            ConcernCoverage::Unsupported {
                reason: "plugin remote control is not supported",
            },
        ),
    ])
}

fn derive_lifecycle_hooks(
    manifest: &PluginManifest,
) -> &'static [(LifecycleSignalKind, HookCoverage)] {
    let has = |name: &str| manifest.emits.iter().any(|event| event == name);
    let native = |event: &'static str| {
        if has(event) {
            HookCoverage::Native { event }
        } else {
            HookCoverage::Absent {
                reason: "canonical event not declared",
            }
        }
    };
    leak_slice(vec![
        (LifecycleSignalKind::Registered, native("session_start")),
        (LifecycleSignalKind::TurnStarted, native("turn_start")),
        (LifecycleSignalKind::TurnEnded, native("turn_end")),
        (LifecycleSignalKind::ToolUsed, native("tool_use")),
        (LifecycleSignalKind::AwaitingInput, native("awaiting_input")),
        (
            LifecycleSignalKind::SubagentStarted,
            native("subagent_start"),
        ),
        (LifecycleSignalKind::SubagentStopped, native("subagent_end")),
        (LifecycleSignalKind::Compacting, native("compaction_start")),
        (
            LifecycleSignalKind::CompactionEnded,
            native("compaction_end"),
        ),
        (LifecycleSignalKind::Ended, native("session_end")),
        (
            LifecycleSignalKind::Lost,
            HookCoverage::Derived {
                via: "rimz exec wrapper",
                gap: "canonical hooks do not report mux-session death",
            },
        ),
    ])
}

fn coverage(wired: bool, via: &'static str, reason: &'static str) -> ConcernCoverage {
    if wired {
        ConcernCoverage::Wired { via }
    } else {
        ConcernCoverage::Unsupported { reason }
    }
}

fn normalize_context(source: &str, payload: &Value, envelope: &Envelope) -> Option<AgentContext> {
    let mut value = payload.clone();
    let object = value.as_object_mut()?;
    object.insert("source".into(), Value::String(source.to_owned()));
    object.insert(
        "observed_at".into(),
        serde_json::to_value(Timestamp::now()).ok()?,
    );
    if !object.contains_key("model_id")
        && let Some(model) = object.get("model").cloned()
    {
        object.insert("model_id".into(), model);
    }
    if !object.contains_key("cost")
        && let Some(total) = envelope.total_cost_usd
    {
        object.insert("cost".into(), json!({ "total_cost_usd": total }));
    }
    if !object.contains_key("rate_limits")
        && let Some(rate_limits) = envelope.rate_limits.as_ref()
    {
        object.insert(
            "rate_limits".into(),
            serde_json::to_value(rate_limits).ok()?,
        );
    }
    let carries_tokens = envelope.context_window.is_some()
        || envelope.context_pct.is_some()
        || envelope.input_tokens.is_some()
        || envelope.output_tokens.is_some()
        || envelope.cache_write_input_tokens.is_some()
        || envelope.cache_read_input_tokens.is_some();
    if !object.contains_key("tokens") && carries_tokens {
        let used_percentage = envelope.context_pct.map(|pct| pct.min(100) as u8);
        let carries_current_usage = envelope.input_tokens.is_some()
            || envelope.output_tokens.is_some()
            || envelope.cache_write_input_tokens.is_some()
            || envelope.cache_read_input_tokens.is_some();
        let current_usage = carries_current_usage.then(|| {
            json!({
                "input_tokens": envelope.input_tokens,
                "output_tokens": envelope.output_tokens,
                "cache_creation_input_tokens": envelope.cache_write_input_tokens,
                "cache_read_input_tokens": envelope.cache_read_input_tokens,
            })
        });
        let tokens = json!({
            "context_window_size": envelope.context_window,
            "used_percentage": used_percentage,
            "current_usage": current_usage,
        });
        object.insert("tokens".into(), tokens);
    }
    let mut context: AgentContext = serde_json::from_value(value).ok()?;
    if let Some(rate_limits) = context.rate_limits.take() {
        context.rate_limits = Some(rate_limits.stamped_at(context.observed_at));
    }
    Some(context)
}

fn render_flag(
    args: &mut Vec<String>,
    agent: &'static str,
    field: &'static str,
    value: Option<&str>,
    flag: Option<&str>,
) -> std::result::Result<(), PresetErr> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    let Some(flag) = flag else {
        return Err(PresetErr::UnsupportedField { agent, field });
    };
    args.push(flag.to_owned());
    args.push(value.to_owned());
    Ok(())
}

fn expand_pattern(plugin_dir: &Path, pattern: &str) -> String {
    if let Some(rest) = pattern.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home)
            .join(rest)
            .to_string_lossy()
            .into_owned();
    }
    let path = Path::new(pattern);
    if path.is_absolute() {
        pattern.to_owned()
    } else {
        plugin_dir.join(path).to_string_lossy().into_owned()
    }
}

fn unknown(event_name: &str) -> ClassifiedHook {
    debug!(
        event = event_name,
        "unknown or invalid canonical plugin event dropped"
    );
    ClassifiedHook {
        class: AgentHookClass::Unknown,
        ask_kind: None,
        event_name: event_name.to_owned(),
    }
}

fn leak_string(value: String) -> &'static str {
    Box::leak(value.into_boxed_str())
}

fn leak_strings(values: &[String]) -> &'static [&'static str] {
    leak_slice(values.iter().cloned().map(leak_string).collect())
}

fn leak_slice<T>(values: Vec<T>) -> &'static [T] {
    Box::leak(values.into_boxed_slice())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::agents::AskKind;

    fn adapter() -> &'static PluginAdapter {
        let root = TempDir::new().unwrap();
        let root = root.keep();
        let dir = root.join("testbot");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("README.md"), "setup").unwrap();
        let manifest = PluginManifest::parse(
            &dir.join("agent.toml"),
            r#"protocol = 1
kind = "testbot"
display-name = "Test Bot"
process-names = ["testbot"]
emits = ["session_start", "turn_start", "turn_end", "tool_use", "awaiting_input", "compaction_start", "compaction_end", "subagent_start", "subagent_end", "session_end", "context"]
setup-doc = "README.md"
[capabilities]
native-ask-ui = true
subagents = true
[tools]
mutating = ["write", "shell"]
editing = ["write"]
[launch]
bin = "testbot"
args = ["--interactive"]
model-flag = "--model"
effort-flag = "--effort"
resume = ["testbot", "--resume", "{session_id}"]
compact-command = "/compact"
"#,
        )
        .unwrap();
        build_adapter(manifest, dir)
    }

    fn minimal_adapter() -> &'static PluginAdapter {
        let root = TempDir::new().unwrap();
        let root = root.keep();
        let dir = root.join("minimalbot");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("README.md"), "setup").unwrap();
        let manifest = PluginManifest::parse(
            &dir.join("agent.toml"),
            r#"protocol = 1
kind = "minimalbot"
display-name = "Minimal Bot"
process-names = ["minimalbot"]
emits = ["session_start"]
setup-doc = "README.md"
"#,
        )
        .unwrap();
        build_adapter(manifest, dir)
    }

    fn payload(event: &str) -> Value {
        json!({
            "protocol": 1,
            "hook_event_name": event,
            "session_id": "root",
            "model": "model-1",
            "context_pct": 42
        })
    }

    #[test]
    fn maps_every_canonical_lifecycle_event() {
        let adapter = adapter();
        let cases = [
            ("session_start", LifecycleSignalKind::Registered),
            ("turn_start", LifecycleSignalKind::TurnStarted),
            ("turn_end", LifecycleSignalKind::TurnEnded),
            ("tool_use", LifecycleSignalKind::ToolUsed),
            ("awaiting_input", LifecycleSignalKind::AwaitingInput),
            ("compaction_start", LifecycleSignalKind::Compacting),
            ("compaction_end", LifecycleSignalKind::CompactionEnded),
            ("session_end", LifecycleSignalKind::Ended),
        ];
        for (event, signal) in cases {
            let mut value = payload(event);
            if event == "awaiting_input" {
                value["ask"] = json!("question");
            }
            assert_eq!(
                adapter
                    .observe_lifecycle(event, &value)
                    .unwrap()
                    .signal
                    .kind(),
                signal,
                "{event}"
            );
        }

        let mut child = payload("subagent_start");
        child["agent_id"] = json!("child");
        let observed = adapter.observe_lifecycle("subagent_start", &child).unwrap();
        assert_eq!(observed.signal.kind(), LifecycleSignalKind::SubagentStarted);
        assert_eq!(observed.agent_id.as_deref(), Some("child"));
        assert_eq!(observed.parent_agent_id.as_deref(), Some("root"));

        let mut child_end = payload("subagent_end");
        child_end["agent_id"] = json!("child");
        assert_eq!(
            adapter
                .observe_lifecycle("subagent_end", &child_end)
                .unwrap()
                .signal
                .kind(),
            LifecycleSignalKind::SubagentStopped
        );
    }

    #[test]
    fn classifies_asks_and_rejects_malformed_or_unknown_events() {
        let adapter = adapter();
        let mut ask = payload("awaiting_input");
        ask["ask"] = json!("permission");
        assert_eq!(
            adapter.classify_hook("awaiting_input", &ask),
            ClassifiedHook {
                class: AgentHookClass::AwaitingUser,
                ask_kind: Some(AskKind::Permission),
                event_name: "awaiting_input".into(),
            }
        );
        assert_eq!(
            adapter.classify_hook("future", &payload("future")).class,
            AgentHookClass::Unknown
        );
        assert_eq!(
            adapter
                .classify_hook(
                    "turn_end",
                    &json!({ "protocol": 2, "hook_event_name": "turn_end" })
                )
                .class,
            AgentHookClass::Unknown
        );
        assert!(adapter.observe_lifecycle("turn_end", &json!({})).is_none());
        assert_eq!(adapter.render_neutral("awaiting_input").unwrap(), None);
    }

    #[test]
    fn undeclared_canonical_events_still_ingest() {
        let adapter = minimal_adapter();
        let turn = payload("turn_start");
        assert_eq!(
            adapter.classify_hook("turn_start", &turn).class,
            AgentHookClass::Lifecycle
        );
        assert_eq!(
            adapter
                .observe_lifecycle("turn_start", &turn)
                .unwrap()
                .signal
                .kind(),
            LifecycleSignalKind::TurnStarted
        );
        assert!(
            adapter
                .observe_context("minimalbot", &payload("context"))
                .is_some()
        );
        assert!(!adapter.descriptor().capabilities.rich_context);
    }

    #[test]
    fn renders_launch_resume_and_presets() {
        let adapter = adapter();
        assert_eq!(
            adapter.launch_command(&["--extra".into()], Some("hello")),
            Some(vec![
                "testbot".into(),
                "--interactive".into(),
                "--extra".into(),
                "--".into(),
                "hello".into()
            ])
        );
        assert_eq!(
            adapter.resume_command("sess-1", Path::new(".")),
            Some(vec!["testbot".into(), "--resume".into(), "sess-1".into()])
        );
        assert_eq!(
            adapter
                .render_preset(&LaunchPreset {
                    model: Some("m".into()),
                    effort: Some("high".into()),
                    ..LaunchPreset::default()
                })
                .unwrap(),
            vec!["--model", "m", "--effort", "high"]
        );
    }

    #[test]
    fn derives_complete_coverage_tables() {
        let descriptor = adapter().descriptor();
        assert_eq!(descriptor.coverage.len(), IntegrationConcern::ALL.len());
        assert_eq!(
            descriptor.lifecycle_hooks.len(),
            LifecycleSignalKind::ALL.len()
        );
        assert!(descriptor.capabilities.rich_context);
        assert!(!descriptor.capabilities.hook_install);
    }

    #[test]
    fn context_event_stamps_source_and_observation_time() {
        let adapter = adapter();
        let mut value = payload("context");
        value["total_cost_usd"] = json!(1.25);
        let context = adapter.observe_context("testbot", &value).unwrap();
        assert_eq!(context.source, "testbot");
        assert_eq!(context.model_id.as_deref(), Some("model-1"));
        assert_eq!(
            context.cost.and_then(|cost| cost.total_cost_usd),
            Some(1.25)
        );
    }
}
