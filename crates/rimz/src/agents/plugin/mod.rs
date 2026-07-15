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
#[cfg(test)]
use super::LaunchPreset;
use super::account::AccountProbe;
use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, ConcernCoverage, HookCoverage, IntegrationCoverage,
    LaunchPermissionArgs, LaunchSpec, LifecycleCoverage, PlanLabel, PresetMatchers, PromptStyle,
    RealtimeUsageChannel, RemoteControlCapability, StaticPresetMatcher, ThreadKey,
    ToolClassification,
};
use super::lifecycle::LifecycleSignal;
use super::observation::{payload_context_pct, payload_total_tokens};
use super::spending::{SpendCursor, SpendParse};
use super::{
    AgentAdapter, AgentContext, AgentHookClass, AgentLifecycleObservation, ClassifiedHook,
    PriceBook, Result, RootIdentity, SubagentIdentity, resolve_root_identity,
    resolve_subagent_identity,
};
#[cfg(test)]
use super::{PresetArgMatcher, PresetField};
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
    fn native_hook_events(&self) -> Vec<&'static str> {
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

    fn probe_account_usage(&self) -> super::AccountUsageProbe {
        let Some(argv) = self.manifest.probes.account.as_deref() else {
            return super::AccountUsageProbe::Unsupported;
        };
        probes::account_usage(self.descriptor.kind, self.plugin_dir, argv)
    }

    fn account_usage_identity(&self) -> Option<super::AccountUsageIdentity> {
        self.manifest
            .probes
            .account
            .as_ref()
            .map(|_| super::AccountUsageIdentity::default())
    }

    fn probe_version(&self) -> Option<String> {
        let argv = self.manifest.probes.version.as_deref()?;
        probes::version(self.descriptor.kind, self.plugin_dir, argv)
    }

    fn launch_command(&self, extra_args: &[String], prompt: Option<&str>) -> Option<Vec<String>> {
        let mut argv = self.descriptor.launch.launch_command(extra_args, prompt)?;
        argv[0] = probes::resolve_executable(self.plugin_dir, &argv[0])
            .to_string_lossy()
            .into_owned();
        Some(argv)
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
    let launch = manifest
        .launch
        .as_ref()
        .map_or(LaunchSpec::EMPTY, |launch| {
            let flag = |value: &Option<String>| {
                value.as_ref().map(|flag| {
                    StaticPresetMatcher::Flag(leak_slice(vec![leak_string(flag.clone())]))
                })
            };
            LaunchSpec {
                program: Some(leak_string(launch.bin.clone())),
                fixed_args: leak_strings(&launch.args),
                prompt: PromptStyle::PositionalAfterDoubleDash,
                resume: None,
                fork: None,
                permission: LaunchPermissionArgs {
                    ask: leak_strings(&launch.permission_args.ask),
                    auto: leak_strings(&launch.permission_args.auto),
                    yolo: leak_strings(&launch.permission_args.yolo),
                    plan: leak_strings(&launch.permission_args.plan),
                },
                ping_args: None,
                max_turn_flag: None,
                compact_command: launch
                    .compact_command
                    .as_ref()
                    .map(|command| leak_string(command.clone())),
                presets: PresetMatchers {
                    model: flag(&launch.model_flag),
                    effort: flag(&launch.effort_flag),
                    system_prompt_file: None,
                    append_system_prompt_file: None,
                },
            }
        });
    AgentDescriptor {
        kind: leak_string(manifest.kind.clone()),
        display_name: leak_string(manifest.display_name.clone()),
        brand: Brand {
            emblem: manifest
                .brand
                .emblem
                .as_ref()
                .map(|emblem| leak_string(emblem.clone())),
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
            native_ask_ui: manifest.capabilities.native_ask_ui,
            transcript_tail_context: false,
            registers_lazily: manifest.capabilities.registers_lazily,
            local_session_discovery: false,
            daemon_hooked_sessions: false,
            realtime_usage: RealtimeUsageChannel {
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
        thread_key: match manifest.transcripts.as_ref().map(|value| value.thread_key) {
            Some(TranscriptThreadKey::SessionDir) => ThreadKey::SessionDir,
            Some(TranscriptThreadKey::PerFile) | None => ThreadKey::PerFile,
        },
        launch,
    }
}

fn derive_coverage(manifest: &PluginManifest, hook_reason: &'static str) -> IntegrationCoverage {
    let has = |name: &str| manifest.emits.iter().any(|event| event == name);
    let turn = has("session_start") && has("turn_start") && has("turn_end");
    let asks = has("awaiting_input") && manifest.capabilities.native_ask_ui;
    let subagents = manifest.capabilities.subagents && has("subagent_start") && has("subagent_end");
    IntegrationCoverage {
        turn_lifecycle: coverage(
            turn,
            "canonical session_start/turn_start/turn_end",
            "canonical turn events not declared",
        ),
        permission: coverage(
            asks,
            "canonical awaiting_input",
            "canonical awaiting_input with native-ask-ui not declared",
        ),
        plan_approval: coverage(
            asks,
            "canonical awaiting_input",
            "canonical awaiting_input with native-ask-ui not declared",
        ),
        user_question: coverage(
            asks,
            "canonical awaiting_input",
            "canonical awaiting_input with native-ask-ui not declared",
        ),
        answer: ConcernCoverage::Unsupported {
            reason: "plugin prompts are answered in the agent's own UI",
        },
        compaction: coverage(
            has("compaction_start") && has("compaction_end"),
            "canonical compaction_start/compaction_end",
            "canonical compaction pair not declared",
        ),
        subagents: coverage(
            subagents,
            "canonical subagent_start/subagent_end",
            "canonical subagent pair and capability not declared",
        ),
        background_parking: ConcernCoverage::Unsupported {
            reason: "canonical protocol has no background-parking signal",
        },
        session_end: coverage(
            has("session_end"),
            "canonical session_end",
            "canonical session_end not declared",
        ),
        idle_notification: ConcernCoverage::Partial {
            via: "turn_end + stall window",
            gap: "canonical protocol has no idle notification",
        },
        context_usage: coverage(
            manifest.capabilities.context_usage || has("context"),
            "canonical context/gauge fields",
            "context usage not declared",
        ),
        realtime_cost: coverage(
            has("context"),
            "canonical context total_cost_usd",
            "context event not declared",
        ),
        rich_context: coverage(
            has("context"),
            "canonical context event",
            "context event not declared",
        ),
        hook_install: ConcernCoverage::Unsupported {
            reason: hook_reason,
        },
        account_spend: coverage(
            manifest.probes.spend.is_some(),
            "plugin spend probe",
            "spend probe not declared",
        ),
        remote_control: ConcernCoverage::Unsupported {
            reason: "plugin remote control is not supported",
        },
    }
}

fn derive_lifecycle_hooks(manifest: &PluginManifest) -> LifecycleCoverage {
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
    LifecycleCoverage {
        registered: native("session_start"),
        turn_started: native("turn_start"),
        turn_ended: native("turn_end"),
        tool_used: native("tool_use"),
        awaiting_input: native("awaiting_input"),
        subagent_started: native("subagent_start"),
        subagent_stopped: native("subagent_end"),
        compacting: native("compaction_start"),
        compaction_ended: native("compaction_end"),
        ended: native("session_end"),
        lost: HookCoverage::Derived {
            via: "rimz exec wrapper",
            gap: "canonical hooks do not report mux-session death",
        },
    }
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
    use crate::agents::descriptor::IntegrationConcern;
    use crate::agents::lifecycle::LifecycleSignalKind;

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

    fn account_adapter(marker: &Path) -> &'static PluginAdapter {
        let root = TempDir::new().unwrap();
        let root = root.keep();
        let dir = root.join("accountbot");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("README.md"), "setup").unwrap();
        let manifest = PluginManifest::parse(
            &dir.join("agent.toml"),
            &format!(
                r#"protocol = 1
kind = "accountbot"
display-name = "Account Bot"
process-names = ["accountbot"]
emits = ["session_start"]
setup-doc = "README.md"
[probes]
account = ["sh", "-c", "touch {}; printf '{{}}'"]
"#,
                marker.display()
            ),
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
        assert!(matches!(
            adapter
                .descriptor()
                .concern_coverage(IntegrationConcern::RichContext),
            ConcernCoverage::Unsupported { .. }
        ));
    }

    #[test]
    fn account_usage_support_discovery_does_not_execute_probe() {
        let root = TempDir::new().unwrap();
        let marker = root.path().join("probe-ran");
        let adapter = account_adapter(&marker);
        assert_eq!(
            adapter.account_usage_identity(),
            Some(super::super::AccountUsageIdentity::default())
        );
        assert!(!marker.exists());
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
        assert_eq!(
            adapter.preset_arg_matcher(PresetField::Model),
            Some(PresetArgMatcher::Flag(vec!["--model".into()]))
        );
        assert_eq!(
            adapter.preset_arg_matcher(PresetField::Effort),
            Some(PresetArgMatcher::Flag(vec!["--effort".into()]))
        );
    }

    #[test]
    fn derives_complete_coverage_tables() {
        let descriptor = adapter().descriptor();
        let coverage = descriptor
            .coverage
            .iter()
            .fold([0; 3], |mut totals, (_, row)| {
                totals[match row {
                    ConcernCoverage::Wired { .. } => 0,
                    ConcernCoverage::Partial { .. } => 1,
                    ConcernCoverage::Unsupported { .. } => 2,
                }] += 1;
                totals
            });
        assert_eq!(coverage, [10, 1, 5]);
        let lifecycle = descriptor
            .lifecycle_hooks
            .iter()
            .fold([0; 3], |mut totals, (_, row)| {
                totals[match row {
                    HookCoverage::Native { .. } => 0,
                    HookCoverage::Derived { .. } => 1,
                    HookCoverage::Absent { .. } => 2,
                }] += 1;
                totals
            });
        assert_eq!(lifecycle, [10, 1, 0]);
        assert!(
            descriptor
                .concern_coverage(IntegrationConcern::RichContext)
                .is_wired()
        );
        assert!(!descriptor.has_wired_hook_install());
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
