//! OMP's passive extension wire, launch contract, and session accounting.

mod payloads;
mod spend;
mod transcript;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::definition::{
    CapabilityLevel as Level, ConcernCoverage as Concern, HookCoverage as Hook,
    LocalSessionDiscovery,
};
use super::hook_types::{HookEventSpec, decode_catalog_hook};
use super::lifecycle::LifecycleSignal;
use super::managed_source::ManagedSource;
use super::*;
use crate::agents::capabilities::*;

#[derive(Clone, Debug, Default)]
pub(in crate::agents) struct OmpAdapter;

impl CoreCapability for OmpAdapter {
    fn spec(&self) -> &'static AgentSpec {
        &OMP_DESCRIPTOR
    }

    #[cfg(test)]
    fn conformance(&self) -> AdapterConformance {
        AdapterConformance {
            classification: hook_types::catalog_classification_corpus(OMP_HOOKS),
            spend: Some(SpendFixture {
                session_id: "019e9161-0000-7000-8000-000000000001",
                file_name: "2026-06-02T10-00-00-000Z_019e9161-0000-7000-8000-000000000001.jsonl",
                body: SpendFixtureBody::Jsonl(concat!(
                    r#"{"type":"session","id":"019e9161-0000-7000-8000-000000000001","cwd":"/work"}"#,
                    "\n",
                    r#"{"type":"message","timestamp":"2026-06-02T10:00:00Z","message":{"role":"assistant","model":"gpt-5","usage":{"input":100,"output":50,"cost":{"total":0.42}}}}"#,
                    "\n",
                )),
            }),
            ..AdapterConformance::default()
        }
    }
}

impl HookCapability for OmpAdapter {
    fn decode_hook(&self, event: &str, payload: &Value) -> Result<HookOutput> {
        let parsed = payloads::parse(payload);
        let has_ui = payload.get("has_ui").and_then(Value::as_bool) == Some(true);
        let tool_name = payload.get("tool_name").and_then(Value::as_str);
        let ask_kind = match event {
            "tool_call" if has_ui => self.spec().blocking_tool_kind(tool_name),
            "tool_approval_requested" if has_ui => Some(AskKind::Permission),
            _ => None,
        };
        let mut decoded = decode_catalog_hook(OMP_HOOKS, event, ask_kind);
        decoded.set_routing(
            HookRouting::session(parsed.session_id.clone().map(Into::into))
                .with_worktree(optional_payload_string(payload, &["cwd"])),
        );
        let detail = optional_payload_string(payload, &["reason", "tool_name"]);
        let questions = if event == "tool_call" && ask_kind.is_some() {
            payload
                .get("tool_input")
                .map(payloads::questions)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        decoded.set_ask(questions, detail.clone());
        decoded.set_observed_context(self.observe_context("omp", payload));
        let native_key = optional_payload_string(payload, &["tool_call_id"]);
        let signal = match event {
            "session_start" => Some(LifecycleSignal::Registered),
            "before_agent_start" => Some(LifecycleSignal::TurnStarted { turn_id: None }),
            "agent_end" if parsed.will_continue != Some(true) => {
                decoded.set_final_message(
                    parsed
                        .last_assistant_message
                        .as_deref()
                        .and_then(non_empty_trimmed),
                );
                Some(if parsed.stop_reason.as_deref() == Some("aborted") {
                    LifecycleSignal::TurnInterrupted { turn_id: None }
                } else {
                    LifecycleSignal::TurnEnded {
                        errored: parsed.stop_reason.as_deref() == Some("error")
                            || parsed
                                .error_message
                                .as_deref()
                                .is_some_and(|s| !s.is_empty()),
                        parked_on_background: false,
                        turn_id: None,
                    }
                })
            }
            "tool_call" | "tool_approval_requested" => {
                ask_kind.map(|kind| LifecycleSignal::AwaitingInput {
                    kind,
                    ask_id: None,
                    detail,
                    native_key,
                })
            }
            "tool_execution_end" | "tool_approval_resolved" => Some(LifecycleSignal::ToolUsed {
                mutates: event == "tool_execution_end" && self.spec().tool_mutates(payload),
                edits: event == "tool_execution_end" && self.spec().tool_edits_files(payload),
                name: (event == "tool_execution_end")
                    .then(|| tool_name.map(ToOwned::to_owned))
                    .flatten(),
                native_key,
                turn_id: None,
            }),
            "session_before_compact" => Some(LifecycleSignal::Compacting),
            "session_compact" => Some(LifecycleSignal::CompactionEnded {
                auto: None,
                failed: false,
            }),
            "auto_compaction_end"
                if parsed.aborted == Some(true)
                    || parsed.skipped == Some(true)
                    || parsed
                        .error_message
                        .as_deref()
                        .is_some_and(|s| !s.is_empty()) =>
            {
                Some(LifecycleSignal::CompactionEnded {
                    auto: Some(true),
                    failed: true,
                })
            }
            "session_shutdown" => Some(LifecycleSignal::Ended),
            _ => None,
        };
        if let Some(signal) = signal {
            let mut observation =
                AgentLifecycleObservation::new(parsed.session_id.map(Into::into), signal)
                    .with_worktree_from_payload(payload);
            observation.task = sanitize_user_prompt(parsed.prompt.as_deref());
            observation.prompt = SanitizedPrompt::new(parsed.prompt.as_deref());
            observation.launch.model = parsed.model;
            observation.launch.effort = parsed.effort;
            observation.usage.context_pct = observation::payload_context_pct(payload, None);
            observation.usage.context_window = parsed.context_window;
            observation.usage.total_tokens = parsed.total_tokens;
            observation.usage.fresh_input_tokens = parsed.input_tokens;
            observation.usage.output_tokens = parsed.output_tokens;
            observation.usage.cache_read_input_tokens = parsed.cache_read_input_tokens;
            observation.usage.cache_write_input_tokens = parsed.cache_write_input_tokens;
            decoded.attach_lifecycle(observation);
        }
        Ok(decoded)
    }
}

impl LaunchCapability for OmpAdapter {
    fn append_system_text_channel(&self) -> Option<SystemTextChannel> {
        Some(SystemTextChannel::ExtensionEnv)
    }

    fn config_home(&self, env: &BTreeMap<String, String>) -> Option<PathBuf> {
        env.get("HOME")
            .filter(|home| !home.is_empty())
            .map(|home| Path::new(home).join(".omp/agent"))
    }

    fn skills_home(&self, env: &BTreeMap<String, String>) -> Option<PathBuf> {
        self.config_home(env).map(|home| home.join("skills"))
    }

    fn manual_skill(&self) -> ManualSkill {
        ManualSkill::Frontmatter
    }
}

impl InstallationCapability for OmpAdapter {
    fn managed_integration(&self) -> Option<&'static dyn ManagedIntegration> {
        Some(&OmpAdapter)
    }
}

impl ManagedIntegration for OmpAdapter {
    fn install(
        &self,
        env: &BTreeMap<String, String>,
        _options: crate::agents::HookInstallOptions,
    ) -> Result<HookInstallReport> {
        OMP_SOURCE.install(env)
    }
    fn preview(
        &self,
        env: &BTreeMap<String, String>,
        _options: crate::agents::HookInstallOptions,
    ) -> Result<HookInstallPreview> {
        OMP_SOURCE.preview(env)
    }
    fn uninstall(&self, env: &BTreeMap<String, String>) -> Result<HookUninstallReport> {
        OMP_SOURCE.uninstall(env)
    }
    fn installed(&self, env: &BTreeMap<String, String>) -> bool {
        OMP_SOURCE.installed(env)
    }
    fn managed_artifacts_present(&self, env: &BTreeMap<String, String>) -> bool {
        OMP_SOURCE.managed_artifacts_present(env)
    }
    fn upgrade_available(
        &self,
        env: &BTreeMap<String, String>,
        _options: crate::agents::HookInstallOptions,
    ) -> bool {
        OMP_SOURCE.upgrade_available(env)
    }
    fn wiring_input_paths(
        &self,
        _spec: &AgentSpec,
        env: &BTreeMap<String, String>,
    ) -> Vec<PathBuf> {
        OMP_SOURCE.resolved_path(env).into_iter().collect()
    }
    fn hooks_disabled(&self, env: &BTreeMap<String, String>) -> Option<HookPreflightErr> {
        let profile = env
            .get_key_value("OMP_PROFILE")
            .or_else(|| env.get_key_value("PI_PROFILE"));
        let relocating = profile
            .filter(|(_, value)| !matches!(value.trim(), "" | "default"))
            .or_else(|| {
                ["PI_CONFIG_DIR", "PI_CODING_AGENT_DIR"]
                    .into_iter()
                    .filter_map(|key| env.get_key_value(key))
                    .find(|(_, value)| !value.is_empty())
            });
        let (key, value) = relocating?;
        Some(HookPreflightErr::HooksDisabled {
            reason: format!("{key}={value:?} relocates omp; RimZ wires omp only at ~/.omp/agent"),
            fix: format!("unset {key} for omp panes"),
        })
    }
}

impl TranscriptCapability for OmpAdapter {
    fn parse_transcript_messages(&self, lines: &str) -> Vec<TranscriptMessage> {
        transcript::parse_messages(lines)
    }
}

impl SpendingCapability for OmpAdapter {
    fn spending_session_id<'p>(&self, path: &'p Path) -> Option<&'p str> {
        let (_, id) = path.file_stem()?.to_str()?.rsplit_once('_')?;
        uuid::Uuid::parse_str(id).ok().map(|_| id)
    }
    fn spending_sources(&self, env: &BTreeMap<String, String>) -> Vec<spending::SpendingSource> {
        let mut roots = self
            .config_home(env)
            .map(|home| home.join("sessions"))
            .into_iter()
            .collect::<Vec<_>>();
        if let Some(data) = env
            .get("XDG_DATA_HOME")
            .filter(|data| !data.is_empty())
            .map(|data| Path::new(data).join("omp"))
            && data.is_dir()
        {
            let root = data.join("sessions");
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
        roots
            .into_iter()
            .flat_map(|root| spending::SpendingSource::tree(root, "**/*.jsonl"))
            .collect()
    }
    fn parse_spend(
        &self,
        path: &Path,
        resume: Option<&spending::SpendCursor>,
        prices: &pricing::PriceBook,
    ) -> spending::SpendParse {
        if self.spending_session_id(path).is_none() {
            return spending::SpendParse::stalled(resume);
        }
        spend::parse(path, resume, prices)
    }
}

impl ContextCapability for OmpAdapter {
    fn observe_context(&self, source: &str, payload: &Value) -> Option<ContextObservation> {
        payloads::context(source, payload)
    }
}
impl AccountCapability for OmpAdapter {}
impl RuntimeControlCapability for OmpAdapter {}
impl SessionCapability for OmpAdapter {}

static OMP_DESCRIPTOR: AgentSpec = AgentSpec {
    kind: "omp",
    display_name: "OMP",
    aliases: &[],
    brand: definition::Brand {
        emblem: None,
        color: 29,
        color_rgb: (0x27, 0xa0, 0x77),
    },
    plan_label: definition::PlanLabel::TitleCaseOnly,
    sub_providers: &[],
    expected_windows: &[],
    tool_rules: skills::ToolRules::Unsupported,
    host_skills: skills::HostSkills::Unsupported,
    tools: definition::ToolClassification {
        input_key: None,
        mutating: &[
            "bash",
            "eval",
            "edit",
            "write",
            "ast_edit",
            "debug",
            "ida",
            "github",
            "lsp",
            "rewind",
            "learn",
            "manage_skill",
            "security_scan",
            "task",
        ],
        editing: &["edit", "write", "ast_edit"],
        blocking: &[("ask", AskKind::Question)],
    },
    capabilities: definition::Capabilities {
        hook_context: None,
        prompt_context: false,
        native_ask_ui: true,
        transcript_tail_context: false,
        registers_lazily: false,
        local_session_discovery: LocalSessionDiscovery::Unsupported,
        daemon_hooked_sessions: false,
        direct_account_usage: false,
        same_pane_session: SamePaneSessionPolicy::FollowLatest,
        remote_control: definition::RemoteControlCapability {
            pane_sessions: false,
            background_sessions: false,
        },
    },
    coverage: definition::CoverageAnnotations {
        turn_lifecycle: Concern::Wired { via: "agent_end" },
        permission: Concern::Wired {
            via: "tool_approval_requested/tool_approval_resolved",
        },
        plan_approval: Concern::Unsupported {
            reason: "idle plan reviews cannot restore the resting card",
        },
        user_question: Concern::Wired {
            via: "tool_call (ask)/tool_execution_end",
        },
        answer: Concern::Unsupported {
            reason: "answer in the omp pane",
        },
        compaction: Concern::Wired {
            via: "session_before_compact/session_compact/auto_compaction_end",
        },
        subagents: Concern::Unsupported {
            reason: "omp task children are not tracked",
        },
        launch_reminders: Concern::Wired {
            via: "before_agent_start systemPrompt",
        },
        background_parking: Concern::Unsupported {
            reason: "background work is not tracked",
        },
        background_shells: Concern::Unsupported {
            reason: "background shells are not tracked",
        },
        session_end: Concern::Wired {
            via: "session_shutdown",
        },
        idle_notification: Concern::Partial {
            via: "agent_end + stall window",
            gap: "no idle-timeout notification",
        },
        context_usage: Concern::Wired {
            via: "extension context usage",
        },
        realtime_cost: Concern::Partial {
            via: "session spend reconciliation",
            gap: "dollars arrive when the turn ends",
        },
        rich_context: Concern::Wired {
            via: "extension model, effort, tokens, and provider windows",
        },
        hook_install: Concern::Wired {
            via: "~/.omp/agent/extensions/rimz.ts",
        },
        account_spend: Concern::Partial {
            via: "provider windows and session spend",
            gap: "login and plan are not read",
        },
        tool_stats: Concern::Wired {
            via: "tool_execution_end names and session toolCall blocks",
        },
        remote_control: Concern::Unsupported {
            reason: "no remote-control surface",
        },
    },
    user_coverage: definition::UserCoverage {
        state: Level::Partial {
            shows: "the card opens, follows each turn, and clears at shutdown",
            limit: "a turn may finish before omp's background work settles",
        },
        live: Level::Partial {
            shows: "context fill, model, and thinking level update as omp works",
            limit: "dollars arrive when the turn ends",
        },
        history: Level::Partial {
            shows: "past sessions carry their tokens and dollars",
            limit: "task children's spend is not counted",
        },
        account: Level::Partial {
            shows: "the backing provider's rate-limit windows appear when reported",
            limit: "login and plan are not read",
        },
        ask: Level::Partial {
            shows: "tool approvals and ask questions raise Waiting",
            limit: "answer them in the omp pane; plan reviews are not tracked",
        },
        subagents: Level::Unsupported {
            reason: "omp task children are not tracked",
        },
    },
    lifecycle_hooks: definition::LifecycleAnnotations {
        registered: Hook::Native {
            event: "session_start",
        },
        turn_started: Hook::Native {
            event: "before_agent_start",
        },
        turn_ended: Hook::Native { event: "agent_end" },
        tool_used: Hook::Native {
            event: "tool_execution_end",
        },
        awaiting_input: Hook::Native { event: "tool_call" },
        subagent_started: Hook::Absent {
            reason: "task children are not tracked",
        },
        subagent_stopped: Hook::Absent {
            reason: "task children are not tracked",
        },
        compacting: Hook::Native {
            event: "session_before_compact",
        },
        compaction_ended: Hook::Native {
            event: "session_compact",
        },
        ended: Hook::Native {
            event: "session_shutdown",
        },
        lost: Hook::Derived {
            via: "rimz exec wrapper",
            gap: "native hooks do not report mux-session death",
        },
    },
    default_context_window: None,
    default_model: None,
    process_names: &["omp"],
    bin_names: &["omp"],
    bin_identity: None,
    extra_bin_dirs: &[],
    thread_key: definition::ThreadKey::PerFile,
    launch: LaunchSpec {
        program: Some("omp"),
        picker: Some(&["omp", "--resume"]),
        prompt: PromptStyle::PositionalAfterDoubleDash,
        resume: Some(SessionCommand {
            before_id: &["omp", "--resume"],
            after_id: &[],
        }),
        fork: Some(SessionCommand {
            before_id: &["omp", "--fork"],
            after_id: &[],
        }),
        permission: LaunchPermissionArgs {
            ask: &["--approval-mode", "always-ask"],
            auto: &["--approval-mode", "write"],
            yolo: &["--approval-mode", "yolo"],
            plan: &["--approval-mode", "always-ask"],
        },
        compact_command: Some(CompactCommand {
            command: "/compact",
            instruction: CompactInstruction::Trailing,
        }),
        presets: definition::PresetMatchers {
            model: Some(definition::StaticPresetMatcher::Flag(&["--model"])),
            effort: Some(definition::StaticPresetMatcher::Flag(&["--thinking"])),
            system_prompt_file: Some(definition::StaticPresetMatcher::TextFlag(&[
                "--system-prompt",
            ])),
            ..definition::PresetMatchers::EMPTY
        },
        definitions: definition::DefinitionSpec {
            tools: definition::DefinitionTools::Ignored,
            ..definition::DefinitionSpec::EMPTY
        },
        ..LaunchSpec::EMPTY
    },
};

const EXTENSION_SOURCE: &str = include_str!("extension.ts");

const OMP_HOOKS: &[HookEventSpec] = &[
    HookEventSpec::lifecycle("session_start", r#"{"session_id":"s"}"#).progress(),
    HookEventSpec::lifecycle(
        "before_agent_start",
        r#"{"session_id":"s","prompt":"fix it"}"#,
    )
    .progress(),
    HookEventSpec::lifecycle("agent_end", r#"{"session_id":"s","stop_reason":"stop"}"#).progress(),
    HookEventSpec::lifecycle("turn_end", r#"{"session_id":"s"}"#).progress(),
    HookEventSpec::lifecycle("message_update", r#"{"session_id":"s"}"#).progress(),
    HookEventSpec::lifecycle("after_provider_response", r#"{"session_id":"s"}"#),
    HookEventSpec::blocking(
        "tool_call",
        r#"{"session_id":"s","tool_name":"ask","tool_call_id":"q","has_ui":true,"tool_input":{"questions":[{"id":"route","question":"Which?","options":[{"label":"Safe"}]}]}}"#,
        AskKind::Question,
    ),
    HookEventSpec::lifecycle(
        "tool_execution_end",
        r#"{"session_id":"s","tool_name":"edit","tool_call_id":"t"}"#,
    )
    .progress(),
    HookEventSpec::blocking(
        "tool_approval_requested",
        r#"{"session_id":"s","tool_name":"bash","tool_call_id":"p","has_ui":true}"#,
        AskKind::Permission,
    ),
    HookEventSpec::lifecycle(
        "tool_approval_resolved",
        r#"{"session_id":"s","tool_call_id":"p"}"#,
    )
    .progress(),
    HookEventSpec::lifecycle("session_before_compact", r#"{"session_id":"s"}"#),
    HookEventSpec::lifecycle("session_compact", r#"{"session_id":"s"}"#),
    HookEventSpec::lifecycle(
        "auto_compaction_end",
        r#"{"session_id":"s","aborted":true}"#,
    ),
    HookEventSpec::lifecycle("session_shutdown", r#"{"session_id":"s"}"#).session_ended(),
];

const OMP_SOURCE: ManagedSource = ManagedSource::new(
    "omp",
    EXTENSION_SOURCE,
    OMP_HOOKS,
    "extension",
    extension_path,
    true,
);

fn extension_path(env: &BTreeMap<String, String>) -> Result<PathBuf> {
    if let Some(path) = env
        .get("RIMZ_OMP_EXTENSION")
        .filter(|path| !path.is_empty())
    {
        return Ok(PathBuf::from(path));
    }
    OmpAdapter
        .config_home(env)
        .map(|home| home.join("extensions/rimz.ts"))
        .ok_or_else(|| AgentErr::Install {
            agent: "omp",
            reason: "$HOME is not set; cannot resolve ~/.omp/agent/extensions/rimz.ts".into(),
        })
}

#[cfg(test)]
mod tests;
