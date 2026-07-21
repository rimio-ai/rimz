//! Process-backed third-party agent adapters.
//!
//! A machine-tier manifest supplies static spec and launch data. The
//! agent's own shim translates native events to [`protocol`] JSON, while
//! optional bounded executables provide pull-only enrichment.

mod check;
mod load;
mod manifest;
mod probes;
mod protocol;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use jiff::Timestamp;
use serde_json::{Value, json};
use tracing::{debug, warn};

use self::manifest::{PluginManifest, TranscriptThreadKey};
use self::protocol::Envelope;
#[cfg(test)]
use super::AskKind;
#[cfg(test)]
use super::LaunchPreset;
use super::account::AccountProbe;
use super::definition::{
    AgentSpec, Brand, Capabilities, CapabilityLevel, ConcernCoverage, CoverageAnnotations,
    HookCoverage, LaunchPermissionArgs, LaunchSpec, LifecycleAnnotations, PlanLabel,
    PresetMatchers, PromptStyle, RealtimeUsageChannel, RemoteControlCapability,
    StaticPresetMatcher, ThreadKey, ToolClassification, UserCoverage,
};
use super::observation::{payload_context_pct, payload_total_tokens};
use super::spending::{SpendCursor, SpendParse};
use super::{
    AgentContext, AgentHookClass, AgentLifecycleObservation, ClassifiedHook, ContextObservation,
    HookOutput, HookRouting, PriceBook, Result, RootIdentity, SubagentIdentity,
    resolve_root_identity, resolve_subagent_identity,
};
#[cfg(test)]
use super::{PresetArgMatcher, PresetField};

pub use check::{
    PluginCheckReport, ProbeCheckReport, ProbeCheckStatus, ReplayCheckReport, ReplayFinalState,
    ReplayRow, check_from_root,
};
pub use load::{
    LoadedPlugins, PluginDiagnostic, PluginLoadError, ProbeDiagnostic, load_from_root, loaded,
    plugins_root,
};
pub use manifest::valid_kind;

pub struct PluginAdapter {
    manifest: &'static PluginManifest,
    plugin_dir: &'static Path,
    spec: &'static AgentSpec,
}

fn build_adapter(manifest: PluginManifest, plugin_dir: PathBuf) -> &'static PluginAdapter {
    // ponytail: process-lifetime plugin config is leaked once; move registry
    // APIs to owned Arcs if live manifest reload becomes a product feature.
    let manifest = Box::leak(Box::new(manifest));
    let plugin_dir = Box::leak(plugin_dir.into_boxed_path());
    let spec = Box::leak(Box::new(build_descriptor(manifest, plugin_dir)));
    Box::leak(Box::new(PluginAdapter {
        manifest,
        plugin_dir,
        spec,
    }))
}

fn warn_undeclared_once(kind: &str, event: &str) {
    static WARNED: OnceLock<Mutex<HashSet<(String, String)>>> = OnceLock::new();
    let warned = WARNED.get_or_init(|| Mutex::new(HashSet::new()));
    let Ok(mut warned) = warned.lock() else {
        return;
    };
    if warned.insert((kind.to_owned(), event.to_owned())) {
        warn!(
            kind,
            event, "agent plugin emitted an undeclared canonical event"
        );
    }
}

impl crate::agents::capabilities::CoreCapability for PluginAdapter {
    fn spec(&self) -> &'static AgentSpec {
        self.spec
    }

    #[cfg(test)]
    fn conformance(&self) -> super::AdapterConformance {
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
        super::AdapterConformance {
            classification: samples,
            ..super::AdapterConformance::default()
        }
    }
}

impl crate::agents::capabilities::HookCapability for PluginAdapter {
    fn decode_hook(&self, event_name: &str, payload: &Value) -> Result<HookOutput> {
        let Some(envelope) = Envelope::parse(event_name, payload) else {
            return Ok(HookOutput::new(unknown(event_name)));
        };
        if !self.emits(event_name) {
            warn_undeclared_once(self.spec.kind, event_name);
        }
        let (mutates, edits) = envelope
            .event
            .tool()
            .map_or((false, false), |(name, is_error)| {
                let mutates = !is_error && self.spec.tools.mutating.contains(&name);
                (mutates, mutates && self.spec.tools.editing.contains(&name))
            });
        let event = envelope.event.normalize(mutates, edits);
        let mut decoded = HookOutput::new(super::ClassifiedHook {
            class: event.class,
            ask_kind: event.ask_kind,
            event_name: event_name.to_owned(),
        });
        decoded.set_policy(event.progress, event.session_ended);
        decoded.set_routing(
            HookRouting::split(
                envelope
                    .agent_id
                    .clone()
                    .or_else(|| envelope.session_id.clone())
                    .map(Into::into),
                envelope.session_id.clone().map(Into::into),
            )
            .with_worktree(envelope.cwd.clone()),
        );
        decoded.set_ask(event.questions, event.ask_detail);
        decoded.set_turn_error(event.turn_error.map(|label| super::AgentTurnError {
            at: Timestamp::now(),
            label: Some(label),
            ..super::AgentTurnError::default()
        }));
        decoded.set_final_message(event.final_message);
        if event.context {
            decoded.set_observed_context(normalize_context_observation(
                self.spec.kind,
                payload,
                &envelope,
            ));
        }
        let Some(signal) = event.signal else {
            return Ok(decoded);
        };
        let (agent_id, parent_agent_id) = if event.is_subagent {
            match resolve_subagent_identity(
                self.spec.kind,
                event_name,
                envelope.agent_id.as_deref(),
                envelope.session_id.as_deref(),
                payload,
            ) {
                SubagentIdentity::Resolved {
                    agent_id,
                    parent_agent_id,
                } => (Some(agent_id), Some(parent_agent_id)),
                SubagentIdentity::Quarantined => return Ok(decoded),
            }
        } else {
            match resolve_root_identity(
                self.spec.kind,
                event_name,
                envelope.agent_id.as_deref(),
                envelope.session_id.as_deref(),
            ) {
                RootIdentity::Root { agent_id } => (agent_id, None),
                RootIdentity::ForeignChild => return Ok(decoded),
            }
        };
        let mut observation =
            AgentLifecycleObservation::new(agent_id, signal).with_worktree_from_payload(payload);
        observation.parent_agent_id = parent_agent_id;
        observation.launch.model = envelope.model;
        observation.launch.effort = envelope.effort;
        observation.usage.context_pct =
            payload_context_pct(payload, envelope.context_pct.map(|pct| pct.min(100) as u8));
        observation.usage.context_window = envelope.context_window;
        observation.usage.total_tokens = payload_total_tokens(payload, envelope.total_tokens);
        observation.usage.fresh_input_tokens = envelope.input_tokens;
        observation.usage.output_tokens = envelope.output_tokens;
        observation.usage.cache_read_input_tokens = envelope.cache_read_input_tokens;
        observation.usage.cache_write_input_tokens = envelope.cache_write_input_tokens;
        observation.transcript_path = envelope.transcript_path;
        if observation.worktree_path.is_none() {
            observation.worktree_path = envelope.cwd;
        }
        if event.prompt.is_some() {
            observation.task = event.prompt.clone();
            observation.prompt = event.prompt;
        }
        decoded.attach_lifecycle(observation);
        Ok(decoded)
    }
}

impl crate::agents::capabilities::LaunchCapability for PluginAdapter {
    fn probe_version(&self) -> Option<String> {
        let argv = self.manifest.probes.version.as_deref()?;
        probes::version(self.spec.kind, self.plugin_dir, argv)
    }

    fn launch_command(&self, extra_args: &[String], prompt: Option<&str>) -> Option<Vec<String>> {
        let mut argv = self.spec.launch.launch_command(extra_args, prompt)?;
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

impl crate::agents::capabilities::ContextCapability for PluginAdapter {
    fn observe_context(&self, source: &str, payload: &Value) -> Option<ContextObservation> {
        let envelope = Envelope::parse("context", payload)?;
        normalize_context_observation(source, payload, &envelope)
    }
}

impl crate::agents::capabilities::AccountCapability for PluginAdapter {
    fn probe_account(&self) -> AccountProbe {
        let Some(argv) = self.manifest.probes.account.as_deref() else {
            return AccountProbe::LoggedOut;
        };
        probes::account(self.spec.kind, self.plugin_dir, argv)
    }

    fn probe_account_usage(&self) -> super::AccountUsageProbe {
        let Some(argv) = self.manifest.probes.account.as_deref() else {
            return super::AccountUsageProbe::Unsupported;
        };
        probes::account_usage(self.spec.kind, self.plugin_dir, argv)
    }
}

impl crate::agents::capabilities::SpendingCapability for PluginAdapter {
    fn transcript_files(&self) -> Vec<PathBuf> {
        let mut files = self
            .transcript_sources()
            .into_iter()
            .flat_map(|source| source.complete_files())
            .collect::<Vec<_>>();
        files.sort();
        files.dedup();
        files
    }

    fn spending_sources(&self) -> Vec<super::spending::SpendingSource> {
        if self.manifest.transcripts.is_none() || self.manifest.probes.spend.is_none() {
            return Vec::new();
        }
        self.transcript_sources()
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
        probes::spend(self.spec.kind, self.plugin_dir, argv, path, resume)
    }
}

impl PluginAdapter {
    fn transcript_sources(&self) -> Vec<super::spending::SpendingSource> {
        self.manifest
            .transcripts
            .as_ref()
            .into_iter()
            .flat_map(|transcripts| &transcripts.globs)
            .filter_map(|pattern| spending_source_for_pattern(self.plugin_dir, pattern))
            .collect()
    }
}

impl PluginAdapter {
    fn emits(&self, event: &str) -> bool {
        self.manifest.emits.iter().any(|declared| declared == event)
    }
}

fn build_descriptor(manifest: &'static PluginManifest, plugin_dir: &'static Path) -> AgentSpec {
    let setup_doc = manifest::resolve_path(plugin_dir, &manifest.setup_doc);
    let hook_reason = leak_string(format!(
        "hook wiring is self-managed; see {}",
        setup_doc.display()
    ));
    let coverage = derive_coverage(manifest, hook_reason);
    let user_coverage = derive_user_coverage(manifest, &coverage);
    let lifecycle_hooks = derive_lifecycle_hooks(manifest);
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
    AgentSpec {
        kind: leak_string(manifest.kind.clone()),
        aliases: &[],
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
        expected_windows: &[],
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
            direct_account_usage: manifest.probes.account.is_some(),
            same_pane_session: super::SamePaneSessionPolicy::KeepPrimary,
            realtime_usage: RealtimeUsageChannel {
                windows_defer_to_fresh_realtime: false,
            },
            remote_control: RemoteControlCapability {
                pane_sessions: false,
                background_sessions: false,
            },
        },
        coverage,
        user_coverage,
        lifecycle_hooks,
        default_context_window: None,
        default_model: None,
        process_names: leak_strings(&manifest.process_names),
        bin_names: leak_strings(std::slice::from_ref(&manifest.kind)),
        extra_bin_dirs: &[],
        thread_key: match manifest.transcripts.as_ref().map(|value| value.thread_key) {
            Some(TranscriptThreadKey::SessionDir) => ThreadKey::SessionDir,
            Some(TranscriptThreadKey::PerFile) | None => ThreadKey::PerFile,
        },
        launch,
    }
}

fn derive_coverage(manifest: &PluginManifest, hook_reason: &'static str) -> CoverageAnnotations {
    let has = |name: &str| manifest.emits.iter().any(|event| event == name);
    let turn = has("session_start") && has("turn_start") && has("turn_end");
    let asks = has("awaiting_input") && manifest.capabilities.native_ask_ui;
    let subagents = manifest.capabilities.subagents && has("subagent_start") && has("subagent_end");
    CoverageAnnotations {
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

/// The six user-facing marks for a plugin, derived from the same manifest the
/// concern grid reads. A plugin owns its own payload detail, so the capabilities
/// whose completeness depends on what it chooses to send stay partial by
/// construction; the bundle's own docs are the authority on what it publishes.
fn derive_user_coverage(manifest: &PluginManifest, coverage: &CoverageAnnotations) -> UserCoverage {
    let has = |name: &str| manifest.emits.iter().any(|event| event == name);
    UserCoverage {
        state: match coverage.turn_lifecycle {
            ConcernCoverage::Wired { .. } => CapabilityLevel::Full {
                note: "the plugin reports session start and every turn boundary",
            },
            ConcernCoverage::Partial { .. } => CapabilityLevel::Partial {
                shows: "turns land as the plugin reports them",
                limit: "part of the canonical turn protocol stays undeclared",
            },
            ConcernCoverage::Unsupported { .. } => CapabilityLevel::Unsupported {
                reason: "the plugin declares no canonical turn events",
            },
        },
        live: if has("context") {
            CapabilityLevel::Partial {
                shows: "the context, token, and dollar figures the plugin publishes",
                limit: "how much detail lands depends on what the plugin sends",
            }
        } else {
            CapabilityLevel::Unsupported {
                reason: "the plugin declares no context event",
            }
        },
        history: if manifest.probes.spend.is_some() {
            CapabilityLevel::Partial {
                shows: "past sessions with the tokens and dollars its spend probe reports",
                limit: "history reaches back only as far as the plugin's own records",
            }
        } else {
            CapabilityLevel::Unsupported {
                reason: "the plugin declares no spend probe",
            }
        },
        account: if manifest.probes.account.is_some() {
            CapabilityLevel::Partial {
                shows: "the identity and plan its account probe reports",
                limit: "usage windows depend on what the plugin sends",
            }
        } else {
            CapabilityLevel::Unsupported {
                reason: "the plugin declares no account probe",
            }
        },
        ask: if has("awaiting_input") && manifest.capabilities.native_ask_ui {
            CapabilityLevel::Partial {
                shows: "a blocked agent raises Waiting and routes you to its pane",
                limit: "the prompt stays in the agent's own UI, rimz asks stays empty",
            }
        } else {
            CapabilityLevel::Unsupported {
                reason: "the plugin declares no awaiting-input event",
            }
        },
        subagents: if manifest.capabilities.subagents
            && has("subagent_start")
            && has("subagent_end")
        {
            CapabilityLevel::Full {
                note: "children nest under the parent card as the plugin starts and stops them",
            }
        } else {
            CapabilityLevel::Unsupported {
                reason: "the plugin declares no subagent lifecycle",
            }
        },
    }
}

fn derive_lifecycle_hooks(manifest: &PluginManifest) -> LifecycleAnnotations {
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
    LifecycleAnnotations {
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

fn normalize_context_observation(
    source: &str,
    payload: &Value,
    envelope: &Envelope,
) -> Option<ContextObservation> {
    let agent_id = match resolve_root_identity(
        source,
        "context",
        envelope.agent_id.as_deref(),
        envelope.session_id.as_deref(),
    ) {
        RootIdentity::Root {
            agent_id: Some(agent_id),
        } => agent_id,
        RootIdentity::Root { agent_id: None } | RootIdentity::ForeignChild => return None,
    };
    ContextObservation::new(agent_id, normalize_context(source, payload, envelope)?)
}

fn spending_source_for_pattern(
    plugin_dir: &Path,
    pattern: &str,
) -> Option<super::spending::SpendingSource> {
    if let Some(rest) = pattern.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return spending_source_for_relative_pattern(PathBuf::from(home), rest);
    }
    let path = Path::new(pattern);
    if path.is_absolute() {
        spending_source_for_expanded_pattern(path)
    } else {
        spending_source_for_relative_pattern(plugin_dir.to_path_buf(), pattern)
    }
}

fn spending_source_for_expanded_pattern(path: &Path) -> Option<super::spending::SpendingSource> {
    let components = path.components().collect::<Vec<_>>();
    let split = components
        .iter()
        .position(|component| glob_component_has_magic(&component.as_os_str().to_string_lossy()));
    let Some(split) = split else {
        return Some(super::spending::SpendingSource::exact(path));
    };
    let root = components[..split].iter().collect::<PathBuf>();
    let relative = components[split..].iter().collect::<PathBuf>();
    let root = if root.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        root
    };
    let relative = relative.to_str()?.to_owned();
    super::spending::SpendingSource::tree(root, relative)
        .into_iter()
        .next()
}

fn spending_source_for_relative_pattern(
    base: PathBuf,
    pattern: &str,
) -> Option<super::spending::SpendingSource> {
    let path = Path::new(pattern);
    let components = path.components().collect::<Vec<_>>();
    let split = components
        .iter()
        .position(|component| glob_component_has_magic(&component.as_os_str().to_string_lossy()));
    let Some(split) = split else {
        return Some(super::spending::SpendingSource::exact(base.join(path)));
    };
    let root = base.join(components[..split].iter().collect::<PathBuf>());
    let relative = components[split..].iter().collect::<PathBuf>();
    super::spending::SpendingSource::tree(root, relative.to_str()?.to_owned())
        .into_iter()
        .next()
}

fn glob_component_has_magic(component: &str) -> bool {
    let mut escaped = false;
    for ch in component.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
        } else if matches!(ch, '*' | '?' | '[') {
            return true;
        }
    }
    false
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

// Capabilities this agent has no behavior for; every method keeps its
// default from `agents::capabilities`.
impl crate::agents::capabilities::InstallationCapability for PluginAdapter {}
impl crate::agents::capabilities::RuntimeControlCapability for PluginAdapter {}
impl crate::agents::capabilities::SessionCapability for PluginAdapter {}
impl crate::agents::capabilities::TranscriptCapability for PluginAdapter {}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::agents::AskKind;
    use crate::agents::capabilities::*;
    use crate::agents::definition::IntegrationConcern;
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

    fn transcript_adapter(with_spend: bool) -> &'static PluginAdapter {
        let root = TempDir::new().unwrap();
        let root = root.keep();
        let dir = root.join(if with_spend {
            "spendbot"
        } else {
            "transcriptbot"
        });
        fs::create_dir(&dir).unwrap();
        fs::create_dir_all(dir.join("history/archive")).unwrap();
        fs::write(dir.join("README.md"), "setup").unwrap();
        fs::write(dir.join("history/archive/session.jsonl"), "{}\n").unwrap();
        let probe = if with_spend {
            "[probes]\nspend = [\"sh\", \"-c\", \"printf '{}'\"]\n"
        } else {
            ""
        };
        let manifest = PluginManifest::parse(
            &dir.join("agent.toml"),
            &format!(
                r#"protocol = 1
kind = "{}"
display-name = "Transcript Bot"
process-names = ["transcriptbot"]
emits = ["session_start"]
setup-doc = "README.md"
[transcripts]
globs = ["history/**/*.jsonl"]
{probe}"#,
                if with_spend {
                    "spendbot"
                } else {
                    "transcriptbot"
                },
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
                    .decode_hook(event, &value)
                    .expect("test hook decodes")
                    .lifecycle()
                    .unwrap()
                    .signal
                    .kind(),
                signal,
                "{event}"
            );
        }

        let mut child = payload("subagent_start");
        child["agent_id"] = json!("child");
        let observed = adapter
            .decode_hook("subagent_start", &child)
            .expect("test hook decodes")
            .lifecycle()
            .cloned()
            .unwrap();
        assert_eq!(observed.signal.kind(), LifecycleSignalKind::SubagentStarted);
        assert_eq!(observed.agent_id.as_deref(), Some("child"));
        assert_eq!(observed.parent_agent_id.as_deref(), Some("root"));

        let mut child_end = payload("subagent_end");
        child_end["agent_id"] = json!("child");
        assert_eq!(
            adapter
                .decode_hook("subagent_end", &child_end)
                .expect("test hook decodes")
                .lifecycle()
                .unwrap()
                .signal
                .kind(),
            LifecycleSignalKind::SubagentStopped
        );
    }

    #[test]
    fn historical_discovery_requires_transcripts_and_spend_probe() {
        let transcript_only = transcript_adapter(false);
        assert_eq!(transcript_only.transcript_files().len(), 1);
        assert!(transcript_only.spending_sources().is_empty());
        assert_eq!(transcript_adapter(true).spending_sources().len(), 1);
    }

    #[test]
    fn classifies_asks_and_rejects_malformed_or_unknown_events() {
        let adapter = adapter();
        let mut ask = payload("awaiting_input");
        ask["ask"] = json!("permission");
        let decoded = adapter
            .decode_hook("awaiting_input", &ask)
            .expect("test hook decodes");
        assert_eq!(decoded.class(), AgentHookClass::AwaitingUser);
        assert_eq!(decoded.ask_kind(), Some(AskKind::Permission));
        assert_eq!(decoded.event_name(), "awaiting_input");
        assert_eq!(
            adapter
                .decode_hook("future", &payload("future"))
                .expect("test hook decodes")
                .class(),
            AgentHookClass::Unknown
        );
        assert_eq!(
            adapter
                .decode_hook(
                    "turn_end",
                    &json!({ "protocol": 2, "hook_event_name": "turn_end" })
                )
                .expect("test hook decodes")
                .class(),
            AgentHookClass::Unknown
        );
        assert!(
            adapter
                .decode_hook("turn_end", &json!({}))
                .expect("test hook decodes")
                .lifecycle()
                .is_none()
        );
        assert_eq!(
            adapter
                .decode_hook("awaiting_input", &Value::Null)
                .expect("test hook decodes")
                .json_reply(),
            None
        );
    }

    #[test]
    fn undeclared_canonical_events_still_ingest() {
        let adapter = minimal_adapter();
        let turn = payload("turn_start");
        assert_eq!(
            adapter
                .decode_hook("turn_start", &turn)
                .expect("test hook decodes")
                .class(),
            AgentHookClass::Lifecycle
        );
        assert_eq!(
            adapter
                .decode_hook("turn_start", &turn)
                .expect("test hook decodes")
                .lifecycle()
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
                .spec()
                .concern_coverage(IntegrationConcern::RichContext),
            ConcernCoverage::Unsupported { .. }
        ));
    }

    #[test]
    fn account_usage_support_discovery_does_not_execute_probe() {
        let root = TempDir::new().unwrap();
        let marker = root.path().join("probe-ran");
        let adapter = account_adapter(&marker);
        assert!(adapter.spec().capabilities.direct_account_usage);
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
                .spec()
                .render_preset(&LaunchPreset {
                    model: Some("m".into()),
                    effort: Some("high".into()),
                    ..LaunchPreset::default()
                })
                .unwrap(),
            vec!["--model", "m", "--effort", "high"]
        );
        assert_eq!(
            adapter.spec().launch.preset_arg_matcher(PresetField::Model),
            Some(PresetArgMatcher::Flag(vec!["--model".into()]))
        );
        assert_eq!(
            adapter
                .spec()
                .launch
                .preset_arg_matcher(PresetField::Effort),
            Some(PresetArgMatcher::Flag(vec!["--effort".into()]))
        );
    }

    #[test]
    fn derives_complete_coverage_tables() {
        let spec = adapter().spec();
        let coverage = spec.coverage.iter().fold([0; 3], |mut totals, (_, row)| {
            totals[match row {
                ConcernCoverage::Wired { .. } => 0,
                ConcernCoverage::Partial { .. } => 1,
                ConcernCoverage::Unsupported { .. } => 2,
            }] += 1;
            totals
        });
        assert_eq!(coverage, [10, 1, 5]);
        let lifecycle = spec
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
            spec.concern_coverage(IntegrationConcern::RichContext)
                .is_wired()
        );
        assert!(!spec.has_wired_hook_install());
    }

    #[test]
    fn context_event_stamps_source_and_observation_time() {
        let adapter = adapter();
        let mut value = payload("context");
        value["total_cost_usd"] = json!(1.25);
        let observation = adapter.observe_context("testbot", &value).unwrap();
        assert_eq!(observation.agent_id.as_str(), "root");
        let context = observation.context;
        assert_eq!(context.source, "testbot");
        assert_eq!(context.model_id.as_deref(), Some("model-1"));
        assert_eq!(
            context.cost.and_then(|cost| cost.total_cost_usd),
            Some(1.25)
        );
    }
}
