//! Composed identity and workflow capabilities for one agent integration.
//!
//! One `const` [`AgentSpec`] per private adapter directory; the registry
//! composes it with selected workflow capability objects in an
//! [`AgentDefinition`]. Immutable identity, presentation, process, and launch
//! facts live here. Native parsing and provider mechanics stay in capability
//! implementations behind the adapters boundary.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde_json::Value;

use super::lifecycle::{AskKind, LifecycleSignalKind};
use super::{LaunchPreset, PresetArgMatcher, PresetErr, PresetField};
use crate::harness::run::PermissionMode;

/// Static launch shapes shared by built-in and process-plugin adapters.
#[derive(Clone, Copy, Debug)]
pub struct LaunchSpec {
    pub program: Option<&'static str>,
    pub fixed_args: &'static [&'static str],
    pub prompt: PromptStyle,
    pub resume: Option<SessionCommand>,
    pub fork: Option<SessionCommand>,
    pub permission: LaunchPermissionArgs,
    pub ping_args: Option<&'static [&'static str]>,
    pub max_turn_flag: Option<&'static str>,
    pub compact_command: Option<&'static str>,
    pub presets: PresetMatchers,
}

impl LaunchSpec {
    pub const EMPTY: Self = Self {
        program: None,
        fixed_args: &[],
        prompt: PromptStyle::None,
        resume: None,
        fork: None,
        permission: LaunchPermissionArgs::EMPTY,
        ping_args: None,
        max_turn_flag: None,
        compact_command: None,
        presets: PresetMatchers::EMPTY,
    };

    /// Render argv that forks a prior session under a provider-assigned new id.
    pub fn fork_command(self, session_id: &str) -> Option<Vec<String>> {
        self.fork.map(|command| command.render(session_id))
    }

    /// Render extra launch argv for one supervised permission posture.
    pub fn permission_args(self, mode: PermissionMode) -> Vec<String> {
        let args = match mode {
            PermissionMode::Ask => self.permission.ask,
            PermissionMode::Auto => self.permission.auto,
            PermissionMode::Yolo => self.permission.yolo,
            PermissionMode::Plan => self.permission.plan,
        };
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    /// Render the native supervised-turn cap, when supported.
    pub fn max_turns_args(self, limit: u32) -> Option<Vec<String>> {
        self.max_turn_flag
            .map(|flag| vec![flag.to_owned(), limit.to_string()])
    }

    /// Return the interactive command for manual context compaction.
    pub const fn compact_command(self) -> Option<&'static str> {
        self.compact_command
    }

    /// Describe the provider-native argv spelling rendered for a preset field.
    pub fn preset_arg_matcher(self, field: PresetField) -> Option<PresetArgMatcher> {
        let matcher = match field {
            PresetField::Model => self.presets.model,
            PresetField::Effort => self.presets.effort,
            PresetField::SystemPromptFile => self.presets.system_prompt_file,
            PresetField::AppendSystemPromptFile => self.presets.append_system_prompt_file,
        }?;
        Some(match matcher {
            StaticPresetMatcher::Flag(flags) => {
                PresetArgMatcher::Flag(flags.iter().map(|flag| (*flag).to_owned()).collect())
            }
            StaticPresetMatcher::ConfigKey { flags, key } => PresetArgMatcher::ConfigKey {
                flags: flags.iter().map(|flag| (*flag).to_owned()).collect(),
                key: key.to_owned(),
            },
        })
    }

    fn render_preset(
        self,
        agent_kind: &'static str,
        preset: &LaunchPreset,
    ) -> Result<Vec<String>, PresetErr> {
        let values: [(PresetField, &'static str, Option<String>); 4] = [
            (
                PresetField::Model,
                "model",
                preset.model.clone().filter(|value| !value.is_empty()),
            ),
            (
                PresetField::Effort,
                "effort",
                preset.effort.clone().filter(|value| !value.is_empty()),
            ),
            (
                PresetField::SystemPromptFile,
                "system-prompt-file",
                preset
                    .system_prompt_file
                    .as_deref()
                    .map(|path| path.to_string_lossy().into_owned()),
            ),
            (
                PresetField::AppendSystemPromptFile,
                "append-system-prompt-file",
                preset
                    .append_system_prompt_file
                    .as_deref()
                    .map(|path| path.to_string_lossy().into_owned()),
            ),
        ];
        let mut argv = Vec::new();
        for (field, field_name, value) in values {
            let Some(value) = value else { continue };
            let matcher = self
                .preset_arg_matcher(field)
                .ok_or(PresetErr::UnsupportedField {
                    agent: agent_kind,
                    field: field_name,
                })?;
            match matcher {
                PresetArgMatcher::Flag(flags) => {
                    let flag = flags.first().ok_or(PresetErr::UnsupportedField {
                        agent: agent_kind,
                        field: field_name,
                    })?;
                    argv.extend([flag.clone(), value]);
                }
                PresetArgMatcher::ConfigKey { flags, key } => {
                    let flag = flags.first().ok_or(PresetErr::UnsupportedField {
                        agent: agent_kind,
                        field: field_name,
                    })?;
                    argv.extend([flag.clone(), format!("{key}={value}")]);
                }
            }
        }
        Ok(argv)
    }

    pub(super) fn resume_command(self, session_id: &str) -> Option<Vec<String>> {
        self.resume.map(|command| command.render(session_id))
    }

    pub(super) fn launch_command(
        self,
        extra_args: &[String],
        prompt: Option<&str>,
    ) -> Option<Vec<String>> {
        let mut argv = vec![self.program?.to_owned()];
        argv.extend(self.fixed_args.iter().map(|arg| (*arg).to_owned()));
        argv.extend(extra_args.iter().cloned());
        if let Some(prompt) = prompt.filter(|value| !value.is_empty()) {
            match self.prompt {
                PromptStyle::None => {}
                PromptStyle::PositionalAfterDoubleDash => {
                    argv.extend(["--".to_owned(), prompt.to_owned()]);
                }
                PromptStyle::Flag(flag) => {
                    argv.extend([flag.to_owned(), prompt.to_owned()]);
                }
                PromptStyle::FlagWithSuffix { flag, suffix } => {
                    argv.extend([flag.to_owned(), prompt.to_owned()]);
                    argv.extend(suffix.iter().map(|arg| (*arg).to_owned()));
                }
            }
        }
        Some(argv)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SessionCommand {
    pub before_id: &'static [&'static str],
    pub after_id: &'static [&'static str],
}

impl SessionCommand {
    fn render(self, session_id: &str) -> Vec<String> {
        self.before_id
            .iter()
            .copied()
            .chain(std::iter::once(session_id))
            .chain(self.after_id.iter().copied())
            .map(ToOwned::to_owned)
            .collect()
    }
}

#[derive(Clone, Copy, Debug)]
pub enum PromptStyle {
    None,
    PositionalAfterDoubleDash,
    Flag(&'static str),
    FlagWithSuffix {
        flag: &'static str,
        suffix: &'static [&'static str],
    },
}

#[derive(Clone, Copy, Debug)]
pub struct LaunchPermissionArgs {
    pub ask: &'static [&'static str],
    pub auto: &'static [&'static str],
    pub yolo: &'static [&'static str],
    pub plan: &'static [&'static str],
}

impl LaunchPermissionArgs {
    pub const EMPTY: Self = Self {
        ask: &[],
        auto: &[],
        yolo: &[],
        plan: &[],
    };
}

#[derive(Clone, Copy, Debug)]
pub struct PresetMatchers {
    pub model: Option<StaticPresetMatcher>,
    pub effort: Option<StaticPresetMatcher>,
    pub system_prompt_file: Option<StaticPresetMatcher>,
    pub append_system_prompt_file: Option<StaticPresetMatcher>,
}

impl PresetMatchers {
    pub const EMPTY: Self = Self {
        model: None,
        effort: None,
        system_prompt_file: None,
        append_system_prompt_file: None,
    };
}

#[derive(Clone, Copy, Debug)]
pub enum StaticPresetMatcher {
    Flag(&'static [&'static str]),
    ConfigKey {
        flags: &'static [&'static str],
        key: &'static str,
    },
}

/// Static identity, branding, capabilities, and classification tables for one
/// agent. See the module doc for the definition-vs-trait split.
#[derive(Debug)]
pub struct AgentSpec {
    /// The stable kind string — the `--source` tag, the per-provider bucket
    /// key, the rollup `kind`.
    pub kind: &'static str,
    /// Alternate source spellings accepted by registry lookup.
    pub aliases: &'static [&'static str],
    /// Human display name; the provider dashboard panel title.
    pub display_name: &'static str,
    /// Brand emblem + color for the provider dashboard panel.
    pub brand: Brand,
    /// How a raw plan tier becomes a brand label (`max` → `Claude Max`).
    pub plan_label: PlanLabel,
    /// Subscription provider ids whose account budget this agent meters, as a
    /// multi-provider client's auth file names them (Pi's `auth.json` keys:
    /// `anthropic`, `openai`, …). Used for account labeling and
    /// provider-specific probes.
    pub sub_providers: &'static [&'static str],
    /// Budget-window labels a metered account of this kind reports, ordered
    /// short-to-long and matching the rendered `window_label` form (`5h`,
    /// `7d`). The dashboard paints these as placeholder tracks before the
    /// first reading; empty when the shape is unknown or unstable.
    pub expected_windows: &'static [&'static str],
    /// Tool-name classification tables for lifecycle and native blocking prompts.
    pub tools: ToolClassification,
    /// What this agent can and cannot do — consumed by the sidebar and doctor
    /// so a missing surface renders as a declared absence, never an
    /// accidental gap.
    pub capabilities: Capabilities,
    /// Declared integration checklist. Every [`IntegrationConcern`] appears
    /// exactly once as wired, partial, or unsupported, and conformance tests
    /// cross-check the declaration against the definition and classification
    /// corpus.
    pub coverage: CoverageAnnotations,
    /// Declared lifecycle-hook checklist. Every [`LifecycleSignalKind`] appears
    /// exactly once as native, derived, or absent; conformance checks the
    /// native event names against the installed hook events and classification
    /// corpus.
    pub lifecycle_hooks: LifecycleAnnotations,
    /// Provider-owned fallback for the model context window shown in an agent
    /// card before a richer runtime source reports the exact value.
    pub default_context_window: Option<u64>,
    /// Provider-owned default model slug. Used as the idle-row display
    /// fallback before a wired agent reports a session model and as the launch
    /// `--model` default when `rimz agents` has no configured model.
    pub default_model: Option<&'static str>,
    /// Process names this agent's instance can run under — its own `comm`
    /// plus any launcher (`node` for a JS bundle). Drives the PID-attribution
    /// `/proc` walk.
    pub process_names: &'static [&'static str],
    /// `$PATH` probe names in preference order. Most agents expose the bare
    /// kind; Cursor ships its CLI as `cursor-agent`/`agent` while `cursor` is
    /// the IDE.
    pub bin_names: &'static [&'static str],
    /// Well-known install directories, relative to `$HOME`, where this agent's
    /// binary lives when its installer has not put
    /// it on `$PATH` — OpenCode's installer drops `opencode` in `~/.opencode/bin`
    /// and edits a shell rc the daemon never sources. Searched after `$PATH` by
    /// [`locate_binary`](super::locate_binary); empty for an agent that only
    /// ever ships on `$PATH`.
    pub extra_bin_dirs: &'static [&'static str],
    /// How this agent's transcript files map to billing threads, for the
    /// spending session count.
    pub thread_key: ThreadKey,
    /// Static process launch contract and its argv renderers.
    pub launch: LaunchSpec,
}

/// The provider-neutral registry value for one agent integration.
///
/// Callers resolve a definition and use its workflow methods; the provider
/// implementation remains an internal detail of the catalog.
pub struct AgentDefinition {
    core: &'static dyn super::capabilities::CoreCapability,
    hooks: Option<&'static dyn super::capabilities::HookCapability>,
    installation: Option<&'static dyn super::capabilities::InstallationCapability>,
    launch: Option<&'static dyn super::capabilities::LaunchCapability>,
    sessions: Option<&'static dyn super::capabilities::SessionCapability>,
    transcript: Option<&'static dyn super::capabilities::TranscriptCapability>,
    context: Option<&'static dyn super::capabilities::ContextCapability>,
    account: Option<&'static dyn super::capabilities::AccountCapability>,
    spending: Option<&'static dyn super::capabilities::SpendingCapability>,
    runtime_control: Option<&'static dyn super::capabilities::RuntimeControlCapability>,
}

#[derive(Debug, thiserror::Error)]
#[error("invalid agent definition `{kind}`: {reason}")]
pub struct DefinitionValidationError {
    kind: &'static str,
    reason: String,
}

impl AgentDefinition {
    pub const fn from_capabilities(
        core: &'static dyn super::capabilities::CoreCapability,
        capabilities: AgentCapabilities,
    ) -> Self {
        Self {
            core,
            hooks: capabilities.hooks,
            installation: capabilities.installation,
            launch: capabilities.launch,
            sessions: capabilities.sessions,
            transcript: capabilities.transcript,
            context: capabilities.context,
            account: capabilities.account,
            spending: capabilities.spending,
            runtime_control: capabilities.runtime_control,
        }
    }

    /// Immutable identity, process, presentation, and runtime-policy facts.
    pub fn spec(&self) -> &'static AgentSpec {
        self.core.spec()
    }

    pub fn validate(&self) -> Result<(), DefinitionValidationError> {
        let spec = self.spec();
        let invalid = |reason: String| DefinitionValidationError {
            kind: spec.kind,
            reason,
        };
        if spec.kind.trim().is_empty() {
            return Err(invalid("kind is empty".to_owned()));
        }
        if spec.display_name.trim().is_empty() {
            return Err(invalid("display name is empty".to_owned()));
        }
        let mut aliases = std::collections::BTreeSet::new();
        for alias in spec.aliases {
            if alias.trim().is_empty() || *alias == spec.kind || !aliases.insert(*alias) {
                return Err(invalid(format!("invalid or duplicate alias `{alias}`")));
            }
        }
        if let Some(tool) = spec
            .tools
            .editing
            .iter()
            .find(|tool| !spec.tools.mutating.contains(tool))
        {
            return Err(invalid(format!(
                "editing tool `{tool}` is absent from the mutating set"
            )));
        }
        if spec.capabilities.local_session_discovery && self.sessions.is_none() {
            return Err(invalid(
                "local-session discovery requires the sessions capability".to_owned(),
            ));
        }
        if spec.capabilities.direct_account_usage && self.account.is_none() {
            return Err(invalid(
                "direct account usage requires the account capability".to_owned(),
            ));
        }
        if (spec.capabilities.remote_control.pane_sessions
            || spec.capabilities.remote_control.background_sessions)
            && self.runtime_control.is_none()
        {
            return Err(invalid(
                "remote-control policy requires the runtime-control capability".to_owned(),
            ));
        }
        if spec
            .lifecycle_hooks
            .iter()
            .any(|(_, coverage)| coverage.is_native())
            && self.hooks.is_none()
        {
            return Err(invalid(
                "native lifecycle coverage requires the hooks capability".to_owned(),
            ));
        }
        if spec.has_wired_hook_install() && self.installation.is_none() {
            return Err(invalid(
                "wired hook installation requires the installation capability".to_owned(),
            ));
        }
        Ok(())
    }

    pub fn concern_coverage(&self, concern: IntegrationConcern) -> ConcernCoverage {
        self.core.spec().coverage.get(concern)
    }

    pub fn lifecycle_coverage(&self, signal: LifecycleSignalKind) -> HookCoverage {
        self.core.spec().lifecycle_hooks.get(signal)
    }

    pub const fn has_hooks(&self) -> bool {
        self.hooks.is_some()
    }

    pub const fn has_installation(&self) -> bool {
        self.installation.is_some()
    }

    pub const fn has_sessions(&self) -> bool {
        self.sessions.is_some()
    }

    pub const fn has_transcript(&self) -> bool {
        self.transcript.is_some()
    }

    pub const fn has_context(&self) -> bool {
        self.context.is_some()
    }

    pub const fn has_account(&self) -> bool {
        self.account.is_some()
    }

    pub const fn has_spending(&self) -> bool {
        self.spending.is_some()
    }

    pub const fn has_runtime_control(&self) -> bool {
        self.runtime_control.is_some()
    }

    pub fn hook_ingress(&self, pid: Option<u32>) -> super::HookIngressDecision {
        self.hooks.map_or_else(
            || super::HookIngressDecision::Accept(super::HookIngressAcceptance::agent(pid)),
            |capability| capability.hook_ingress(pid),
        )
    }

    pub fn decode_hook(
        &self,
        event_name: &str,
        payload: &Value,
    ) -> super::Result<super::HookOutput> {
        self.hooks.map_or_else(
            || {
                Ok(super::HookOutput::new(super::ClassifiedHook {
                    class: super::AgentHookClass::Unknown,
                    ask_kind: None,
                    event_name: event_name.to_owned(),
                }))
            },
            |capability| capability.decode_hook(event_name, payload),
        )
    }

    #[cfg(test)]
    pub(crate) fn conformance(&self) -> super::AdapterConformance {
        self.hooks.map_or_else(
            || self.core.core_conformance(),
            |capability| capability.conformance(),
        )
    }

    pub fn derive_subagent_observations(
        &self,
        workspace: &Path,
    ) -> Vec<super::AgentLifecycleObservation> {
        self.hooks.map_or_else(Vec::new, |capability| {
            capability.derive_subagent_observations(workspace)
        })
    }

    pub fn correlate_subagent(
        &self,
        input: super::SubagentCorrelationInput<'_>,
    ) -> Option<super::SubagentCorrelation> {
        self.hooks?.correlate_subagent(input)
    }

    pub fn spawned_subagents(
        &self,
        input: super::SubagentSpawnInput<'_>,
    ) -> Vec<super::SpawnedSubagent> {
        self.hooks
            .map_or_else(Vec::new, |capability| capability.spawned_subagents(input))
    }

    pub fn ask_options(&self, kind: super::AskKind) -> Option<Vec<crate::transcript::AskOption>> {
        self.hooks?.ask_options(kind)
    }

    pub fn answer_plan(
        &self,
        kind: super::AskKind,
        questions: &[crate::transcript::AskQuestion],
        answers: &[super::AskReply],
    ) -> std::result::Result<Vec<super::AnswerStep>, super::AnswerPlanErr> {
        self.hooks.map_or_else(
            || Err(super::AnswerPlanErr::Unsupported(self.spec().kind)),
            |capability| capability.answer_plan(kind, questions, answers),
        )
    }

    pub fn wiring_input_paths(&self) -> Vec<PathBuf> {
        self.installation
            .map_or_else(Vec::new, |capability| capability.wiring_input_paths())
    }

    pub fn managed_integration(&self) -> Option<&'static dyn super::ManagedIntegration> {
        self.installation?.managed_integration()
    }

    pub fn install_hooks(&self) -> super::Result<super::HookInstallReport> {
        self.installation.map_or_else(
            || {
                Err(super::AgentErr::Install {
                    agent: self.spec().kind,
                    reason: "install not implemented for this agent".to_owned(),
                })
            },
            |capability| capability.install_hooks(),
        )
    }

    pub fn preview_hook_install(&self) -> super::Result<super::HookInstallPreview> {
        self.installation.map_or_else(
            || {
                Err(super::AgentErr::Install {
                    agent: self.spec().kind,
                    reason: "install preview not implemented for this agent".to_owned(),
                })
            },
            |capability| capability.preview_hook_install(),
        )
    }

    pub fn uninstall_hooks(&self) -> super::Result<super::HookUninstallReport> {
        self.installation.map_or_else(
            || {
                Err(super::AgentErr::Install {
                    agent: self.spec().kind,
                    reason: "uninstall not implemented for this agent".to_owned(),
                })
            },
            |capability| capability.uninstall_hooks(),
        )
    }

    pub fn managed_hook_artifacts_present(&self) -> bool {
        self.installation
            .is_some_and(|capability| capability.managed_hook_artifacts_present())
    }

    pub fn wrapped_status_line_command(&self) -> Option<String> {
        self.installation?.wrapped_status_line_command()
    }

    pub fn status_line_invocation(&self) -> super::StatusLineInvocation {
        self.installation
            .map_or(super::StatusLineInvocation::Shell, |capability| {
                capability.status_line_invocation()
            })
    }

    pub fn wrapped_subagent_status_line_command(&self) -> Option<String> {
        self.installation?.wrapped_subagent_status_line_command()
    }

    pub fn hooks_installed(&self) -> bool {
        self.installation
            .is_some_and(|capability| capability.hooks_installed())
    }

    pub fn untrusted_installed_hooks(&self) -> Vec<String> {
        self.installation.map_or_else(Vec::new, |capability| {
            capability.untrusted_installed_hooks()
        })
    }

    pub fn is_interactive_process(&self, command: &str) -> bool {
        self.launch
            .is_none_or(|capability| capability.is_interactive_process(command))
    }

    pub fn default_launch_model(&self) -> Option<String> {
        self.launch.map_or_else(
            || self.spec().default_model.map(ToOwned::to_owned),
            |capability| capability.default_launch_model(),
        )
    }

    pub fn configured_identity(&self) -> (Option<String>, Option<String>) {
        self.launch
            .map_or((None, None), |capability| capability.configured_identity())
    }

    pub fn parse_version(&self, stdout: &str, stderr: &str) -> Option<String> {
        self.launch.map_or_else(
            || super::version::conventional_cli_version(stdout, stderr),
            |capability| capability.parse_version(stdout, stderr),
        )
    }

    pub fn probe_version(&self) -> Option<String> {
        self.launch.map_or_else(
            || {
                super::probe_descriptor_version(self.spec(), &|stdout, stderr| {
                    self.parse_version(stdout, stderr)
                })
            },
            |capability| capability.probe_version(),
        )
    }

    pub fn resume_command(&self, session_id: &str, cwd: &Path) -> Option<Vec<String>> {
        self.launch.map_or_else(
            || self.spec().launch.resume_command(session_id),
            |capability| capability.resume_command(session_id, cwd),
        )
    }

    pub fn ping_args(&self) -> Option<Vec<String>> {
        self.launch.map_or_else(
            || {
                self.spec()
                    .launch
                    .ping_args
                    .map(|args| args.iter().map(|arg| (*arg).to_owned()).collect())
            },
            |capability| capability.ping_args(),
        )
    }

    pub fn launch_command(
        &self,
        extra_args: &[String],
        prompt: Option<&str>,
    ) -> Option<Vec<String>> {
        self.launch.map_or_else(
            || self.spec().launch.launch_command(extra_args, prompt),
            |capability| capability.launch_command(extra_args, prompt),
        )
    }

    pub fn launch_env(&self) -> Vec<(&'static str, &'static str)> {
        self.launch
            .map_or_else(Vec::new, |capability| capability.launch_env())
    }

    pub fn room_env(&self, runtime: &crate::store::RuntimePaths) -> BTreeMap<String, String> {
        self.launch
            .map_or_else(BTreeMap::new, |capability| capability.room_env(runtime))
    }

    pub fn probe_resting_interruption(
        &self,
        agent_id: &crate::ids::AgentSessionId,
    ) -> Option<Timestamp> {
        self.sessions?.probe_resting_interruption(agent_id)
    }

    pub fn daemon_session_evidence(&self) -> super::session::DaemonSessionEvidence {
        self.sessions.map_or_else(
            super::session::DaemonSessionEvidence::default,
            |capability| capability.daemon_session_evidence(),
        )
    }

    pub fn turn_death_needs_pane_confirmation(&self, error: &super::AgentTurnError) -> bool {
        self.sessions
            .is_some_and(|capability| capability.turn_death_needs_pane_confirmation(error))
    }

    pub fn refine_turn_death_from_frame(&self, error: &mut super::AgentTurnError, frame: &str) {
        if let Some(capability) = self.sessions {
            capability.refine_turn_death_from_frame(error, frame);
        }
    }

    pub fn infer_turn_death_from_spent_window(
        &self,
        error: &mut super::AgentTurnError,
        capacity: Option<&super::ProviderCapacity>,
        now: Timestamp,
    ) {
        if let Some(capability) = self.sessions {
            capability.infer_turn_death_from_spent_window(error, capacity, now);
        }
    }

    #[cfg(feature = "testkit")]
    pub fn discover_local_sessions_under(
        &self,
        home: &Path,
        workspaces: &[&Path],
    ) -> Vec<super::LocalSessionObservation> {
        self.sessions.map_or_else(Vec::new, |capability| {
            capability.discover_local_sessions_under(home, workspaces)
        })
    }

    pub fn discover_local_sessions(
        &self,
        workspaces: &[&Path],
    ) -> Vec<super::LocalSessionObservation> {
        self.sessions.map_or_else(Vec::new, |capability| {
            capability.discover_local_sessions(workspaces)
        })
    }

    pub fn resumed_session_id_from_cmdline(
        &self,
        cmdline: &str,
    ) -> Option<crate::ids::AgentSessionId> {
        self.sessions?.resumed_session_id_from_cmdline(cmdline)
    }

    pub fn local_conversation_present(
        &self,
        session_id: &crate::ids::AgentSessionId,
        cwd: &Path,
    ) -> Option<bool> {
        self.sessions?.local_conversation_present(session_id, cwd)
    }

    pub fn parse_transcript_messages(&self, lines: &str) -> Vec<super::TranscriptMessage> {
        self.transcript.map_or_else(Vec::new, |capability| {
            capability.parse_transcript_messages(lines)
        })
    }

    pub fn read_transcript_messages(
        &self,
        path: &Path,
        session_id: Option<&crate::ids::AgentSessionId>,
    ) -> std::io::Result<Vec<super::TranscriptMessage>> {
        self.transcript.map_or_else(
            || Ok(Vec::new()),
            |capability| capability.read_transcript_messages(path, session_id),
        )
    }

    pub fn stream_assistant_messages(&self, lines: &str) -> Vec<String> {
        self.transcript.map_or_else(Vec::new, |capability| {
            capability.stream_assistant_messages(lines)
        })
    }

    pub fn transcript_position(
        &self,
        path: &Path,
        session_id: Option<&crate::ids::AgentSessionId>,
    ) -> Option<super::TranscriptPosition> {
        self.transcript?.transcript_position(path, session_id)
    }

    pub fn read_assistant_transcript_page(
        &self,
        path: &Path,
        session_id: Option<&crate::ids::AgentSessionId>,
        position: super::TranscriptPosition,
    ) -> Option<super::TranscriptPage> {
        self.transcript?
            .read_assistant_transcript_page(path, session_id, position)
    }

    pub fn observe_context(
        &self,
        source: &str,
        payload: &Value,
    ) -> Option<super::ContextObservation> {
        self.context?.observe_context(source, payload)
    }

    pub fn price_turn_locally(
        &self,
        event_name: &str,
        payload: &Value,
        prices: &super::PriceBook,
    ) -> Option<super::LocallyPricedTurnCost> {
        self.context?
            .price_turn_locally(event_name, payload, prices)
    }

    pub fn context_cost(
        &self,
        payload: &Value,
        prices: &super::PriceBook,
    ) -> Option<super::AgentCost> {
        self.context?.context_cost(payload, prices)
    }

    pub fn observe_subagent_context(&self, payload: &Value) -> Vec<super::SubagentObservation> {
        self.context.map_or_else(Vec::new, |capability| {
            capability.observe_subagent_context(payload)
        })
    }

    pub fn context_refresh_spawn(
        &self,
        trigger: super::RefreshTrigger<'_>,
        ctx: &super::LifecycleRefreshCtx<'_>,
    ) -> Option<super::RefreshSpawn> {
        self.context?.context_refresh_spawn(trigger, ctx)
    }

    pub fn local_context_refresh(
        &self,
        trigger: super::RefreshTrigger<'_>,
        ctx: &super::LocalContextRefreshCtx<'_>,
    ) -> Option<super::LocalContextRefresh> {
        self.context?.local_context_refresh(trigger, ctx)
    }

    pub fn serve_context_broker(
        &self,
        session_name: Option<&str>,
        socket_path: &Path,
    ) -> std::io::Result<()> {
        self.context.map_or_else(
            || {
                Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    format!("{} has no context broker", self.spec().kind),
                ))
            },
            |capability| capability.serve_context_broker(session_name, socket_path),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn refresh_transcript_context_runtime(
        &self,
        session_id: &str,
        model_hint: Option<&str>,
        prior_transcript_path: Option<&str>,
        prior_transcript_stat: Option<&super::TranscriptStat>,
        prior_spend_fold: Option<&super::LocalSpendFold>,
        pricing_cache_path: &Path,
    ) -> Option<super::LocalContextRefresh> {
        self.context?.refresh_transcript_context_runtime(
            session_id,
            model_hint,
            prior_transcript_path,
            prior_transcript_stat,
            prior_spend_fold,
            pricing_cache_path,
        )
    }

    pub fn rich_context_refresh_due(
        &self,
        record: Option<&crate::store::agent_context::AgentContextRecord>,
        within: i64,
    ) -> bool {
        self.context
            .is_some_and(|capability| capability.rich_context_refresh_due(record, within))
    }

    pub fn refresh_runtime_enrichment(
        &self,
        session_id: Option<&str>,
        model_hint: Option<&str>,
        broker_socket: Option<&Path>,
    ) -> Option<super::context_runtime::RuntimeEnrichment> {
        self.context?
            .refresh_runtime_enrichment(session_id, model_hint, broker_socket)
    }

    pub fn merge_runtime_context(
        &self,
        runtime: &crate::RuntimePaths,
        session_id: &str,
        context: super::AgentContext,
    ) -> anyhow::Result<()> {
        self.context.map_or(Ok(()), |capability| {
            capability.merge_runtime_context(runtime, session_id, context)
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn refresh_embedded_context(
        &self,
        server_url: &str,
        session_id: &str,
        current_model: Option<&str>,
        prior: Option<&super::AgentContext>,
        rich_observed_at: Option<Timestamp>,
        now: Timestamp,
    ) -> Option<super::AgentContext> {
        self.context?.refresh_embedded_context(
            server_url,
            session_id,
            current_model,
            prior,
            rich_observed_at,
            now,
        )
    }

    pub fn merge_embedded_context(
        &self,
        current: &mut super::AgentContext,
        observed: &super::AgentContext,
    ) -> bool {
        self.context
            .is_some_and(|capability| capability.merge_embedded_context(current, observed))
    }

    pub fn probe_account(&self) -> super::account::AccountProbe {
        self.account
            .map_or(super::account::AccountProbe::LoggedOut, |capability| {
                capability.probe_account()
            })
    }

    pub fn prepare_reset_credit(&self) -> Result<super::account::ResetCreditOffer, String> {
        self.account.map_or_else(
            || {
                Err(format!(
                    "{} does not support reset-credit redemption",
                    self.spec().kind
                ))
            },
            |capability| capability.prepare_reset_credit(),
        )
    }

    pub fn probe_account_usage(&self) -> super::AccountUsageProbe {
        self.account
            .map_or(super::AccountUsageProbe::Unsupported, |capability| {
                capability.probe_account_usage()
            })
    }

    pub fn resolve_managed_launch(
        &self,
        cwd: &Path,
        env: &BTreeMap<String, String>,
        model: Option<&str>,
        argv: &[String],
    ) -> super::ManagedLaunchState {
        self.account
            .map_or(super::ManagedLaunchState::Unsupported, |capability| {
                capability.resolve_managed_launch(cwd, env, model, argv)
            })
    }

    pub fn probe_realtime_account_usage(
        &self,
        runtime: &crate::RuntimePaths,
    ) -> Option<super::AccountUsageSnapshot> {
        self.account?.probe_realtime_account_usage(runtime)
    }

    pub fn remote_control_status(
        &self,
        account: Option<&super::AgentAccount>,
    ) -> super::RemoteControlStatus {
        self.account
            .map_or_else(super::RemoteControlStatus::default, |capability| {
                capability.remote_control_status(account)
            })
    }

    pub fn transcript_files(&self) -> Vec<PathBuf> {
        self.spending
            .map_or_else(Vec::new, |capability| capability.transcript_files())
    }

    pub fn transcript_stat(&self, path: &Path) -> Option<super::TranscriptStat> {
        self.spending.map_or_else(
            || super::TranscriptStat::from_path(path),
            |capability| capability.transcript_stat(path),
        )
    }

    pub fn spending_sources(&self) -> Vec<super::spending::SpendingSource> {
        self.spending
            .map_or_else(Vec::new, |capability| capability.spending_sources())
    }

    pub fn session_transcript(
        &self,
        session_id: &str,
        prior_path: Option<&Path>,
    ) -> Option<PathBuf> {
        self.spending?.session_transcript(session_id, prior_path)
    }

    pub fn parse_spend(
        &self,
        path: &Path,
        resume: Option<&super::spending::SpendCursor>,
        prices: &super::PriceBook,
    ) -> super::spending::SpendParse {
        self.spending
            .map_or_else(super::spending::SpendParse::default, |capability| {
                capability.parse_spend(path, resume, prices)
            })
    }

    pub fn runtime_control_readiness(
        &self,
        enabled: bool,
    ) -> super::runtime_control::RuntimeControlReadiness {
        self.runtime_control.map_or(
            super::runtime_control::RuntimeControlReadiness::Disabled,
            |capability| capability.runtime_control_readiness(enabled),
        )
    }

    pub fn runtime_control_host_argv(&self) -> Option<Vec<String>> {
        self.runtime_control?.runtime_control_host_argv()
    }

    pub fn ensure_runtime_control(&self, enabled: bool) {
        if let Some(capability) = self.runtime_control {
            capability.ensure_runtime_control(enabled);
        }
    }

    pub fn reconcile_runtime_control(
        &self,
        enabled: bool,
    ) -> std::result::Result<(), super::runtime_control::RuntimeControlError> {
        self.runtime_control.map_or(Ok(()), |capability| {
            capability.reconcile_runtime_control(enabled)
        })
    }

    pub fn runtime_control_advisory(&self) -> Option<String> {
        self.runtime_control?.runtime_control_advisory()
    }

    pub fn runtime_control_wiring_input_path(&self) -> Option<PathBuf> {
        self.runtime_control?.runtime_control_wiring_input_path()
    }
}

#[doc(hidden)]
#[derive(Clone, Copy, Default)]
pub struct AgentCapabilities {
    pub hooks: Option<&'static dyn super::capabilities::HookCapability>,
    pub installation: Option<&'static dyn super::capabilities::InstallationCapability>,
    pub launch: Option<&'static dyn super::capabilities::LaunchCapability>,
    pub sessions: Option<&'static dyn super::capabilities::SessionCapability>,
    pub transcript: Option<&'static dyn super::capabilities::TranscriptCapability>,
    pub context: Option<&'static dyn super::capabilities::ContextCapability>,
    pub account: Option<&'static dyn super::capabilities::AccountCapability>,
    pub spending: Option<&'static dyn super::capabilities::SpendingCapability>,
    pub runtime_control: Option<&'static dyn super::capabilities::RuntimeControlCapability>,
}

impl AgentCapabilities {
    pub const NONE: Self = Self {
        hooks: None,
        installation: None,
        launch: None,
        sessions: None,
        transcript: None,
        context: None,
        account: None,
        spending: None,
        runtime_control: None,
    };
}

impl std::fmt::Debug for AgentDefinition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentDefinition")
            .field("kind", &self.spec().kind)
            .finish_non_exhaustive()
    }
}

impl AgentSpec {
    /// Render typed launch profile presets into provider-native argv.
    pub fn render_preset(&self, preset: &LaunchPreset) -> Result<Vec<String>, PresetErr> {
        self.launch.render_preset(self.kind, preset)
    }
}

/// Architecture tokens that open a Rust target triple, so a release binary
/// named `<kind>-<triple>` (`codex-aarch64-apple-darwin`) still reads as
/// `<kind>`. Matching the arch, not the full triple, stays correct under the
/// kernel's 15-char `comm` truncation (`codex-aarch64-a`), where the arch fits
/// and the vendor/os tail does not.
const TARGET_ARCHES: &[&str] = &[
    "x86_64",
    "aarch64",
    "arm64",
    "armv7",
    "arm",
    "i686",
    "i386",
    "riscv64",
    "powerpc64",
    "powerpc",
    "s390x",
    "loongarch64",
];

/// Whether a program `comm`/argv0 basename names `kind`: the bare kind, or the
/// kind under a target-triple release-binary suffix (`codex-aarch64-apple-darwin`,
/// or its `comm`-truncated `codex-aarch64-a`).
pub fn program_names_kind(name: &str, kind: &str) -> bool {
    if name == kind {
        return true;
    }
    let Some(rest) = name
        .strip_prefix(kind)
        .and_then(|rest| rest.strip_prefix('-'))
    else {
        return false;
    };
    TARGET_ARCHES.iter().any(|arch| {
        rest == *arch
            || rest
                .strip_prefix(arch)
                .is_some_and(|tail| tail.starts_with('-'))
    })
}

/// How a provider's transcript files map to billing threads (sessions), so the
/// spending pass counts one thread once however many files it spread across.
#[derive(Clone, Copy, Debug)]
pub enum ThreadKey {
    /// One transcript file per session — the file path is the thread.
    PerFile,
    /// One directory per session holding a main JSONL plus `subagents/*.jsonl`
    /// children — the session directory is the thread, so a subagent file
    /// folds under its parent session (Claude).
    SessionDir,
}

/// Brand styling for the provider dashboard panel.
#[derive(Debug)]
pub struct Brand {
    /// Optional plugin-supplied emblem override. Built-in agents resolve their
    /// art from the embedded emblem catalog by kind.
    pub emblem: Option<&'static str>,
    /// 256-color index.
    pub color: u8,
    /// Truecolor brand tone for renderers using RGB depth.
    pub color_rgb: (u8, u8, u8),
}

/// How a raw plan tier string becomes its brand label.
#[derive(Debug)]
pub enum PlanLabel {
    /// `"<prefix> <TitleCase(tier)>"` — Claude → "Claude Max",
    /// Codex → "ChatGPT Pro".
    Prefixed { prefix: &'static str },
    /// Just title-case the tier — for an agent whose sessions span many
    /// provider accounts, where no single brand prefix is honest.
    TitleCaseOnly,
}

impl PlanLabel {
    /// Format a raw provider tier in the provider's declared plan vocabulary.
    pub fn format(&self, raw: &str) -> String {
        let tier = crate::theme::provider_title_case(raw);
        match self {
            Self::Prefixed { prefix } => format!("{prefix} {tier}"),
            Self::TitleCaseOnly => tier,
        }
    }
}

/// The agent's tool vocabulary, classified for lifecycle and native blocking prompts.
#[derive(Debug)]
pub struct ToolClassification {
    /// Tools that mutate the workspace — write files or run commands. A
    /// mutating tool is proof of real work, so its completed tool signal is
    /// durable even when it does not change state.
    pub mutating: &'static [&'static str],
    /// The file-editing subset of `mutating` — the turn's first edit moves it
    /// from reasoning to acting. A shell tool mutates but does not edit, so a
    /// research turn that only runs commands keeps the thinking head.
    pub editing: &'static [&'static str],
    /// Tools whose pre-use hook is a blocking ask, paired with the ask kind
    /// they raise. Empty when the agent's blocking gate is an event, not a tool.
    pub blocking: &'static [(&'static str, AskKind)],
}

macro_rules! integration_concerns {
    ($($variant:ident => $label:literal),+ $(,)?) => {
        /// Product-level integration concerns every adapter declares explicitly.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum IntegrationConcern {
            $($variant),+
        }

        impl IntegrationConcern {
            pub const ALL: [Self; integration_concerns!(@count $($variant),+)] = [
                $(Self::$variant),+
            ];

            pub const fn short_label(self) -> &'static str {
                match self {
                    $(Self::$variant => $label),+
                }
            }
        }
    };
    (@count $($variant:ident),+ $(,)?) => {
        <[()]>::len(&[$(integration_concerns!(@unit $variant)),+])
    };
    (@unit $variant:ident) => {
        ()
    };
}

integration_concerns! {
    TurnLifecycle => "turn",
    Permission => "perm",
    PlanApproval => "plan",
    UserQuestion => "ask",
    Answer => "answer",
    Compaction => "compact",
    Subagents => "sub",
    BackgroundParking => "bg",
    SessionEnd => "end",
    IdleNotification => "idle",
    ContextUsage => "usage",
    RealtimeCost => "live$",
    RichContext => "rich",
    HookInstall => "install",
    AccountSpend => "spend",
    RemoteControl => "remote",
}

/// How an adapter covers a concern: a native signal carries it directly,
/// derivation reconstructs it where the native signal is absent, or it is
/// unreachable from the current protocol surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConcernCoverage {
    /// The concern reaches a user-complete state; `via` names the path. This is
    /// a native signal that carries it directly, or a value RimZ reconciles to
    /// its authoritative figure at each turn boundary so the surface is visually
    /// full — complete to the user even without a continuous native push (for
    /// example the realtime-cost dollar, settled to the session spend sum every
    /// turn). Reserve `Partial` for a surface the user can still see is missing
    /// something between reconciliations.
    Wired { via: &'static str },
    /// Native coverage is incomplete, but RimZ reconstructs the behaviour from
    /// another signal or state: `via` is the combined path, `gap` what it still
    /// lacks.
    Partial {
        via: &'static str,
        gap: &'static str,
    },
    /// Unreachable from the current protocol surface, by any inference; `reason`
    /// says why.
    Unsupported { reason: &'static str },
}

impl ConcernCoverage {
    pub const fn is_wired(self) -> bool {
        matches!(self, Self::Wired { .. })
    }

    /// The reason-like text: the via for wired, the gap for partial, the
    /// unsupported reason — what the matrix prints after the concern label.
    pub const fn detail(self) -> &'static str {
        match self {
            Self::Wired { via } => via,
            Self::Partial { gap, .. } => gap,
            Self::Unsupported { reason } => reason,
        }
    }
}

/// How an adapter covers a lifecycle signal: a native event carries it directly,
/// derivation reconstructs it where the native event is absent, or the agent
/// cannot produce the signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookCoverage {
    /// A native event carries the lifecycle signal directly.
    Native { event: &'static str },
    /// No native event, but RimZ reconstructs the behaviour from other state:
    /// `via` is the derivation, `gap` what the reconstruction still lacks.
    Derived {
        via: &'static str,
        gap: &'static str,
    },
    /// Unreachable from the current protocol surface; `reason` says why.
    Absent { reason: &'static str },
}

impl HookCoverage {
    pub const fn is_native(self) -> bool {
        matches!(self, Self::Native { .. })
    }

    /// The reason-like text: event for native, gap for derived, reason for
    /// absent — what the matrix prints after the signal label.
    pub const fn detail(self) -> &'static str {
        match self {
            Self::Native { event } => event,
            Self::Derived { gap, .. } => gap,
            Self::Absent { reason } => reason,
        }
    }
}

/// Compile-time-complete integration support claims for one adapter.
#[derive(Clone, Copy, Debug)]
pub struct CoverageAnnotations {
    pub turn_lifecycle: ConcernCoverage,
    pub permission: ConcernCoverage,
    pub plan_approval: ConcernCoverage,
    pub user_question: ConcernCoverage,
    pub answer: ConcernCoverage,
    pub compaction: ConcernCoverage,
    pub subagents: ConcernCoverage,
    pub background_parking: ConcernCoverage,
    pub session_end: ConcernCoverage,
    pub idle_notification: ConcernCoverage,
    pub context_usage: ConcernCoverage,
    pub realtime_cost: ConcernCoverage,
    pub rich_context: ConcernCoverage,
    pub hook_install: ConcernCoverage,
    pub account_spend: ConcernCoverage,
    pub remote_control: ConcernCoverage,
}

impl CoverageAnnotations {
    pub const fn get(self, concern: IntegrationConcern) -> ConcernCoverage {
        match concern {
            IntegrationConcern::TurnLifecycle => self.turn_lifecycle,
            IntegrationConcern::Permission => self.permission,
            IntegrationConcern::PlanApproval => self.plan_approval,
            IntegrationConcern::UserQuestion => self.user_question,
            IntegrationConcern::Answer => self.answer,
            IntegrationConcern::Compaction => self.compaction,
            IntegrationConcern::Subagents => self.subagents,
            IntegrationConcern::BackgroundParking => self.background_parking,
            IntegrationConcern::SessionEnd => self.session_end,
            IntegrationConcern::IdleNotification => self.idle_notification,
            IntegrationConcern::ContextUsage => self.context_usage,
            IntegrationConcern::RealtimeCost => self.realtime_cost,
            IntegrationConcern::RichContext => self.rich_context,
            IntegrationConcern::HookInstall => self.hook_install,
            IntegrationConcern::AccountSpend => self.account_spend,
            IntegrationConcern::RemoteControl => self.remote_control,
        }
    }

    pub fn iter(self) -> impl Iterator<Item = (IntegrationConcern, ConcernCoverage)> {
        IntegrationConcern::ALL
            .into_iter()
            .map(move |concern| (concern, self.get(concern)))
    }
}

/// Compile-time-complete lifecycle-signal support claims for one adapter.
#[derive(Clone, Copy, Debug)]
pub struct LifecycleAnnotations {
    pub registered: HookCoverage,
    pub turn_started: HookCoverage,
    pub turn_ended: HookCoverage,
    pub tool_used: HookCoverage,
    pub awaiting_input: HookCoverage,
    pub subagent_started: HookCoverage,
    pub subagent_stopped: HookCoverage,
    pub compacting: HookCoverage,
    pub compaction_ended: HookCoverage,
    pub ended: HookCoverage,
    pub lost: HookCoverage,
}

impl LifecycleAnnotations {
    pub const fn get(self, signal: LifecycleSignalKind) -> HookCoverage {
        match signal {
            LifecycleSignalKind::Registered => self.registered,
            LifecycleSignalKind::TurnStarted => self.turn_started,
            LifecycleSignalKind::TurnEnded => self.turn_ended,
            LifecycleSignalKind::ToolUsed => self.tool_used,
            LifecycleSignalKind::AwaitingInput => self.awaiting_input,
            LifecycleSignalKind::SubagentStarted => self.subagent_started,
            LifecycleSignalKind::SubagentStopped => self.subagent_stopped,
            LifecycleSignalKind::Compacting => self.compacting,
            LifecycleSignalKind::CompactionEnded => self.compaction_ended,
            LifecycleSignalKind::Ended => self.ended,
            LifecycleSignalKind::Lost => self.lost,
        }
    }

    pub fn iter(self) -> impl Iterator<Item = (LifecycleSignalKind, HookCoverage)> {
        LifecycleSignalKind::ALL
            .into_iter()
            .map(move |signal| (signal, self.get(signal)))
    }
}

/// Operational policy that cannot be derived from integration coverage.
#[derive(Clone, Copy, Debug)]
pub struct Capabilities {
    /// Renders its own ask UI in the pane — permission prompts, plan
    /// approvals, questions — so RimZ can mark the agent waiting while the
    /// prompt stays in the native UI. An agent without one (pi gates tools
    /// only through the extension) resolves the same ask neutrally with no
    /// waiting state: there is no native prompt to route the human to.
    pub native_ask_ui: bool,
    /// Local transcript/rollout tail is a live context source refreshable
    /// outside hooks. Drives producer ticks and renderer transcript watches.
    pub transcript_tail_context: bool,
    /// Registers its session lazily and/or routes hooks through a daemon, so
    /// an instance can be present without a stamped session. The sidebar binds
    /// such a session to its pane by cwd.
    pub registers_lazily: bool,
    /// Discovers live session identity and lifecycle from a provider-owned
    /// machine-local store, without executable hooks or RimZ store writes.
    pub local_session_discovery: bool,
    /// Sessions route hooks through a per-user daemon that outlives any one
    /// conversation, so a new session may succeed another in the same pane
    /// before the reaper clears the stamp.
    pub daemon_hooked_sessions: bool,
    /// Provides an authoritative, identity-bearing direct account-usage
    /// probe. Scheduling uses this static declaration before provider work.
    pub direct_account_usage: bool,
    /// Which co-resident root session owns a live pane when one agent process
    /// carries more than one session id.
    pub same_pane_session: SamePaneSessionPolicy,
    /// How this provider's realtime usage channel interacts with the uniform
    /// account-usage driver.
    pub realtime_usage: RealtimeUsageChannel,
    /// Remote-control surfaces the provider can host.
    pub remote_control: RemoteControlCapability,
}

/// Pane ownership for multiple root sessions hosted by one live agent process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SamePaneSessionPolicy {
    /// Keep the earliest registered session as the pane's primary owner.
    KeepPrimary,
    /// Hand the pane to the most recently registered active conversation.
    FollowLatest,
}

/// How a provider's realtime usage channel interacts with the uniform direct
/// account-usage driver.
#[derive(Clone, Copy, Debug)]
pub struct RealtimeUsageChannel {
    /// A content-fresh realtime windows reading owns the included-budget
    /// windows, so the direct merge defers to it.
    pub windows_defer_to_fresh_realtime: bool,
}

/// Static remote-control capability. Dynamic "enabled on this machine" state
/// lives on [`AgentDefinition`](super::AgentDefinition), because it may read provider
/// settings.
#[derive(Clone, Copy, Debug)]
pub struct RemoteControlCapability {
    /// Living pane sessions can be driven remotely.
    pub pane_sessions: bool,
    /// The provider can spawn background remote sessions without a local pane.
    pub background_sessions: bool,
}

impl AgentSpec {
    /// Declared support for one product concern.
    pub const fn concern_coverage(&self, concern: IntegrationConcern) -> ConcernCoverage {
        self.coverage.get(concern)
    }

    /// Whether RimZ can install hooks this adapter executes.
    pub const fn has_wired_hook_install(&self) -> bool {
        self.concern_coverage(IntegrationConcern::HookInstall)
            .is_wired()
    }

    /// User-facing reason shown when hook installation is unavailable.
    pub const fn hook_install_failure_detail(&self) -> Option<&'static str> {
        match self.concern_coverage(IntegrationConcern::HookInstall) {
            ConcernCoverage::Wired { .. } => None,
            coverage => Some(coverage.detail()),
        }
    }

    /// Whether this adapter publishes authoritative account-level dollars that
    /// can safely enforce a provider-account budget.
    pub fn has_authoritative_account_spend(&self) -> bool {
        matches!(
            self.concern_coverage(IntegrationConcern::AccountSpend),
            ConcernCoverage::Wired { .. }
        )
    }

    /// The kind as a typed identity — the one sanctioned mint of an
    /// [`AgentKind`](crate::ids::AgentKind) for a known adapter.
    pub fn kind_id(&self) -> crate::ids::AgentKind {
        crate::ids::AgentKind::new_unchecked(self.kind)
    }

    /// Whether a process `comm`/argv0 basename belongs to this agent: one of
    /// its declared process names (its own binary plus any launcher), including
    /// a declared name under a target-triple release-binary suffix.
    pub fn runs_as(&self, name: &str) -> bool {
        self.process_names
            .iter()
            .any(|program| program_names_kind(name, program))
    }

    /// Whether a command basename launches this agent. This is distinct from
    /// [`runs_as`](Self::runs_as): a resolved executable may expose a provider-
    /// independent name (`agent` for Cursor) while its kernel `comm` names the
    /// runtime instead. Target-triple suffix matching applies to each declared
    /// name rather than implicitly accepting the kind, so `antigravity` does
    /// not shadow the actual `agy` process.
    pub fn launches_as(&self, name: &str) -> bool {
        self.bin_names
            .iter()
            .any(|program| program_names_kind(name, program))
    }

    /// Whether a tool-use payload names a workspace-mutating tool. The tool
    /// name rides `tool_name` in every provider's payload.
    pub fn tool_mutates(&self, payload: &Value) -> bool {
        self.tool_in(payload, self.tools.mutating)
    }

    /// Whether a tool-use payload names a *file-editing* tool.
    pub fn tool_edits_files(&self, payload: &Value) -> bool {
        self.tool_in(payload, self.tools.editing)
    }

    /// Whether a pre-tool-use payload names a blocking ask tool.
    pub fn blocking_tool_kind(&self, tool_name: Option<&str>) -> Option<AskKind> {
        let name = tool_name?;
        self.tools
            .blocking
            .iter()
            .find_map(|(tool, kind)| (*tool == name).then_some(*kind))
    }

    fn tool_in(&self, payload: &Value, set: &[&str]) -> bool {
        payload
            .get("tool_name")
            .and_then(Value::as_str)
            .is_some_and(|name| set.contains(&name))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{LaunchSpec, PromptStyle, program_names_kind};
    use crate::agents::AskKind;
    use crate::agents::registry::BUILTINS;

    #[test]
    fn every_descriptor_keeps_editing_a_subset_of_mutating() {
        for adapter in BUILTINS {
            let definition = adapter.spec();
            for tool in definition.tools.editing {
                assert!(
                    definition.tools.mutating.contains(tool),
                    "{}: editing tool {tool} missing from the mutating set",
                    definition.kind,
                );
            }
        }
    }

    #[test]
    fn prompt_flag_suffix_is_conditional_on_nonempty_prompt() {
        let launch = LaunchSpec {
            program: Some("agent"),
            fixed_args: &["--fixed"],
            prompt: PromptStyle::FlagWithSuffix {
                flag: "--prompt",
                suffix: &["--format", "jsonl"],
            },
            ..LaunchSpec::EMPTY
        };
        let extra = ["--verbose".to_owned()];
        assert_eq!(
            launch.launch_command(&extra, Some("review")),
            Some(
                [
                    "agent",
                    "--fixed",
                    "--verbose",
                    "--prompt",
                    "review",
                    "--format",
                    "jsonl",
                ]
                .map(ToOwned::to_owned)
                .to_vec()
            )
        );
        for prompt in [None, Some("")] {
            assert_eq!(
                launch.launch_command(&extra, prompt),
                Some(
                    ["agent", "--fixed", "--verbose"]
                        .map(ToOwned::to_owned)
                        .to_vec()
                )
            );
        }
    }

    #[test]
    fn descriptor_classifies_mutating_editing_and_blocking_tools() {
        let claude = crate::agents::registry::spec_by_kind("claude").unwrap();
        assert!(claude.tool_mutates(&json!({ "tool_name": "Edit" })));
        assert!(claude.tool_mutates(&json!({ "tool_name": "Bash" })));
        assert!(!claude.tool_mutates(&json!({ "tool_name": "Read" })));
        assert!(!claude.tool_mutates(&json!({})));
        // Command runners mutate but do not edit — the reasoning phase survives.
        assert!(!claude.tool_edits_files(&json!({ "tool_name": "Bash" })));
        assert!(claude.tool_edits_files(&json!({ "tool_name": "Write" })));
        assert_eq!(
            claude.blocking_tool_kind(Some("ExitPlanMode")),
            Some(AskKind::PlanApproval)
        );
        assert_eq!(
            claude.blocking_tool_kind(Some("AskUserQuestion")),
            Some(AskKind::Question)
        );
        assert_eq!(claude.blocking_tool_kind(Some("request_user_input")), None);

        let codex = crate::agents::registry::spec_by_kind("codex").unwrap();
        assert!(codex.tool_mutates(&json!({ "tool_name": "apply_patch" })));
        assert!(codex.tool_edits_files(&json!({ "tool_name": "apply_patch" })));
        assert!(!codex.tool_edits_files(&json!({ "tool_name": "shell" })));
        assert_eq!(
            codex.blocking_tool_kind(Some("request_user_input")),
            Some(AskKind::Question)
        );
        assert_eq!(codex.blocking_tool_kind(Some("ExitPlanMode")), None);
        assert_eq!(codex.blocking_tool_kind(Some("update_plan")), None);
        assert_eq!(codex.blocking_tool_kind(None), None);
    }

    #[test]
    fn target_triple_binary_names_still_name_the_agent_kind() {
        for name in [
            "codex",
            "codex-aarch64-apple-darwin",
            "codex-x86_64-apple-darwin",
            "codex-x86_64-unknown-linux-musl",
            "codex-aarch64-unknown-linux-gnu",
            "codex-aarch64-a",
        ] {
            assert!(program_names_kind(name, "codex"), "{name}");
        }

        for name in ["codexfoo", "codex-plan", "codex-appserver-stub", "node"] {
            assert!(!program_names_kind(name, "codex"), "{name}");
        }
    }

    #[test]
    fn descriptor_run_names_include_launchers_and_target_triples() {
        let codex = crate::agents::registry::spec_by_kind("codex").unwrap();

        assert!(codex.runs_as("codex"));
        assert!(codex.runs_as("node"));
        assert!(codex.runs_as("codex-aarch64-a"));
        assert!(!codex.runs_as("zsh"));
    }
}
