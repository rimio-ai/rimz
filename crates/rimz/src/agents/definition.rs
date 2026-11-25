//! Composed identity and workflow capabilities for one agent integration.
//!
//! One `const` [`AgentSpec`] per private adapter directory; the registry
//! composes it with selected workflow capability objects in an
//! [`AgentDefinition`]. Immutable identity, presentation, process, and launch
//! facts live here. Native parsing and provider mechanics stay in capability
//! implementations behind the adapters boundary.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::lifecycle::{AskKind, LifecycleSignalKind, ToolEffect};
use super::{LaunchPreset, PresetArgMatcher, PresetErr, PresetField};
use crate::pane::keys::{KeyStep, NamedKey};

#[cfg(unix)]
const HOOK_ENV_RESTORED: &str = "RIMZ_HOOK_ENV_RESTORED";

/// Resolve a scrubbed hook's owner environment before any RimZ paths are read.
/// Only built-ins can declare this process contract; loading process plugins
/// would itself resolve the RimZ home before the environment is restored.
#[cfg(unix)]
pub fn hook_owner_env(source: &str) -> std::io::Result<Option<Vec<(String, String)>>> {
    let Some(definition) = super::registry::BUILTINS.iter().find(|definition| {
        definition.spec().kind == source || definition.spec().aliases.contains(&source)
    }) else {
        return Ok(None);
    };
    hook_owner_env_with(
        definition.core,
        std::env::var("RIMZ_AGENT_PID").ok().as_deref(),
        std::env::var_os(HOOK_ENV_RESTORED).is_some(),
        crate::proc::comm,
        crate::proc::environ,
    )
}

#[cfg(unix)]
fn hook_owner_env_with(
    hook: &dyn super::capabilities::HookCapability,
    owner_pid: Option<&str>,
    restored: bool,
    mut comm: impl FnMut(u32) -> Option<String>,
    environ: impl FnOnce(u32) -> Option<Vec<(String, String)>>,
) -> std::io::Result<Option<Vec<(String, String)>>> {
    if hook.hook_inherits_env() || restored {
        return Ok(None);
    }
    let raw_pid = owner_pid.ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing RIMZ_AGENT_PID")
    })?;
    let pid = raw_pid
        .parse::<u32>()
        .ok()
        .filter(|pid| *pid > 1)
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid RIMZ_AGENT_PID")
        })?;
    if !comm(pid).is_some_and(|name| hook.spec().runs_as(&name)) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "hook owner process does not match the source",
        ));
    }
    let mut env = environ(pid).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "unreadable hook owner environment",
        )
    })?;
    if !comm(pid).is_some_and(|name| hook.spec().runs_as(&name)) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "hook owner process changed while reading its environment",
        ));
    }
    env.retain(|(key, _)| key != "RIMZ_AGENT_PID" && key != HOOK_ENV_RESTORED);
    env.push(("RIMZ_AGENT_PID".to_owned(), raw_pid.to_owned()));
    env.push((HOOK_ENV_RESTORED.to_owned(), "1".to_owned()));
    Ok(Some(env))
}

fn context_owner_start_with(
    spec: &AgentSpec,
    owner_pid: Option<&str>,
    mut comm: impl FnMut(u32) -> Option<String>,
    process_start: impl FnOnce(u32) -> Option<jiff::Timestamp>,
) -> Option<jiff::Timestamp> {
    let pid = owner_pid?.parse::<u32>().ok().filter(|pid| *pid > 1)?;
    if !comm(pid).is_some_and(|name| spec.runs_as(&name)) {
        return None;
    }
    let started_at = process_start(pid)?;
    comm(pid)
        .is_some_and(|name| spec.runs_as(&name))
        .then_some(started_at)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    Auto,
    Ask,
    Yolo,
    Plan,
}

impl PermissionMode {
    pub(crate) const fn rank(self) -> u8 {
        match self {
            Self::Plan => 0,
            Self::Ask => 1,
            Self::Auto => 2,
            Self::Yolo => 3,
        }
    }
}

impl std::str::FromStr for PermissionMode {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw {
            "auto" => Ok(Self::Auto),
            "ask" => Ok(Self::Ask),
            "yolo" => Ok(Self::Yolo),
            "plan" => Ok(Self::Plan),
            _ => Err(format!(
                "unknown permission mode `{raw}`; expected auto, ask, plan, or yolo"
            )),
        }
    }
}

impl std::fmt::Display for PermissionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Auto => "auto",
            Self::Ask => "ask",
            Self::Yolo => "yolo",
            Self::Plan => "plan",
        })
    }
}

/// A written three-state launch switch (`fast`, `code-mode`): on, forced off, or `unset`,
/// which stops inheritance and leaves the provider's own default. "Not written" is `None`
/// around it. Reads `true`/`false`, the YAML 1.1 words (`on`, `off`, `yes`, `no`), and
/// `"unset"`; writes `true`, `false`, and `"unset"`, so absent, `true`, and `false` keep
/// the bytes a boolean had.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Toggle {
    On,
    Off,
    Unset,
}

impl Toggle {
    /// The effective value: `Some(false)` forces the provider's off switch, `None` renders
    /// nothing.
    pub fn enabled(self) -> Option<bool> {
        match self {
            Self::On => Some(true),
            Self::Off => Some(false),
            Self::Unset => None,
        }
    }
}

impl From<bool> for Toggle {
    fn from(on: bool) -> Self {
        if on { Self::On } else { Self::Off }
    }
}

impl Serialize for Toggle {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::On => serializer.serialize_bool(true),
            Self::Off => serializer.serialize_bool(false),
            Self::Unset => serializer.serialize_str("unset"),
        }
    }
}

impl<'de> Deserialize<'de> for Toggle {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = Toggle;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("`on`, `off`, `true`, `false`, or `unset`")
            }

            fn visit_bool<E: serde::de::Error>(self, on: bool) -> Result<Toggle, E> {
                Ok(on.into())
            }

            fn visit_str<E: serde::de::Error>(self, raw: &str) -> Result<Toggle, E> {
                match raw.to_ascii_lowercase().as_str() {
                    "on" | "true" | "yes" | "y" => Ok(Toggle::On),
                    "off" | "false" | "no" | "n" => Ok(Toggle::Off),
                    "unset" => Ok(Toggle::Unset),
                    _ => Err(E::invalid_value(serde::de::Unexpected::Str(raw), &self)),
                }
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

/// A launch setting omitted because the final provider cannot apply it to this launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SkippedPresetField {
    AutoCompact(String),
    Effort(String),
    /// Effort on a model session-step kind launched without a model.
    EffortNeedsModel(String),
    /// Effort on a model session-step kind resumed without a model switch.
    EffortNeedsModelSwitch(String),
    /// `max` on a kind whose effort list lacks it, launched at the model's highest level.
    EffortLowered {
        value: String,
        level: &'static str,
    },
    /// The provider's fast alternative, when it has one.
    Fast {
        alternative: Option<&'static str>,
    },
    /// Off on a provider that can hold a saved fast mode no launch switch forces out.
    FastOff {
        fix: &'static str,
    },
    CodeMode,
}

/// The keys a message delivery presses into an agent's composer, each a short sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MessageKeys {
    /// Submits typed text for the running turn, or opens a turn on a resting agent. A steer, and
    /// an interrupt after its stop, press it; a forced steer onto an open prompt drops a leading
    /// `queue` that a chord follows (`message::prompt_chord`).
    pub send_now: &'static [KeyStep],
    /// Submits typed text behind a running turn, or opens a turn on a resting agent. A boundary
    /// delivery presses it.
    pub queue: &'static [KeyStep],
    /// Stops the running turn.
    pub interrupt: &'static [KeyStep],
    /// Submits a typed slash command.
    pub command: &'static [KeyStep],
    /// Whether these keys were checked against the agent; `false` takes the defaults.
    pub implemented: bool,
}

impl MessageKeys {
    /// Enter, Enter, Escape, Enter: the keys of an agent whose own are not yet implemented.
    pub(crate) const NOT_YET_IMPLEMENTED: Self = Self {
        send_now: &[KeyStep::now(NamedKey::Enter)],
        queue: &[KeyStep::now(NamedKey::Enter)],
        interrupt: &[KeyStep::now(NamedKey::Escape)],
        command: &[KeyStep::now(NamedKey::Enter)],
        implemented: false,
    };
}

/// Static launch shapes shared by built-in and process-plugin adapters.
#[derive(Clone, Copy, Debug)]
pub struct LaunchSpec {
    pub(super) definitions: DefinitionSpec,
    pub program: Option<&'static str>,
    pub fixed_args: &'static [&'static str],
    pub prompt: PromptStyle,
    pub resume: Option<SessionCommand>,
    pub picker: Option<&'static [&'static str]>,
    pub fork: Option<SessionCommand>,
    pub restores_session_model: bool,
    pub model_session_step: bool,
    /// Value flags that require an explicit model.
    pub model_dependent_flags: &'static [&'static str],
    pub permission: LaunchPermissionArgs,
    pub max_turn_flag: Option<&'static str>,
    pub compact_command: Option<CompactCommand>,
    pub message_keys: MessageKeys,
    pub presets: PresetMatchers,
    /// Provider env pair requesting long prompt-cache retention for a keep-warm launch.
    pub long_cache_env: Option<(&'static str, &'static str)>,
}

impl LaunchSpec {
    pub const EMPTY: Self = Self {
        definitions: DefinitionSpec::EMPTY,
        program: None,
        fixed_args: &[],
        prompt: PromptStyle::None,
        resume: None,
        picker: None,
        fork: None,
        restores_session_model: false,
        model_session_step: false,
        model_dependent_flags: &[],
        permission: LaunchPermissionArgs::EMPTY,
        max_turn_flag: None,
        compact_command: None,
        message_keys: MessageKeys::NOT_YET_IMPLEMENTED,
        presets: PresetMatchers::EMPTY,
        long_cache_env: None,
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

    /// Settle final argv containing the requested mode's rendered vector, if any.
    /// Defaults yield to raw permission markers; one owned vector is excluded
    /// when deciding whether the recorded mode is known.
    pub fn settle_permission_mode(
        self,
        args: &mut Vec<String>,
        requested: Option<PermissionMode>,
        default: Option<PermissionMode>,
    ) -> Option<PermissionMode> {
        let rendered = requested
            .map(|mode| self.permission_args(mode))
            .unwrap_or_default();
        let owned = (!rendered.is_empty())
            .then(|| {
                args.windows(rendered.len())
                    .position(|part| part == rendered)
            })
            .flatten()
            .map(|start| start..start + rendered.len());
        let raw = args.iter().enumerate().any(|(index, arg)| {
            !owned.as_ref().is_some_and(|range| range.contains(&index))
                && (self.permission.flags.iter().any(|flag| {
                    let short = flag.len() == 2 && !flag.starts_with("--");
                    arg.strip_prefix(*flag)
                        .is_some_and(|rest| rest.is_empty() || rest.starts_with('=') || short)
                }) || self
                    .permission
                    .config_keys
                    .iter()
                    .any(|key| arg.contains(*key)))
        });
        if requested.is_none() && raw {
            return None;
        }
        let mode = self.permission.resolve_mode(requested.or(default))?;
        if requested != Some(mode) {
            if let Some(owned) = owned {
                args.drain(owned);
            }
            args.extend(self.permission_args(mode));
        }
        (!raw).then_some(mode)
    }

    /// Remove all exact rendered permission vectors, longest first.
    pub fn strip_permission_args(self, args: &mut Vec<String>) {
        let mut vectors = [
            self.permission.ask,
            self.permission.auto,
            self.permission.yolo,
            self.permission.plan,
        ];
        vectors.sort_by_key(|vector| std::cmp::Reverse(vector.len()));
        for vector in vectors.into_iter().filter(|vector| !vector.is_empty()) {
            while let Some(index) = args.windows(vector.len()).position(|part| part == vector) {
                args.drain(index..index + vector.len());
            }
        }
    }

    /// Render the native supervised-turn cap, when supported.
    pub fn max_turns_args(self, limit: u32) -> Option<Vec<String>> {
        self.max_turn_flag
            .map(|flag| vec![flag.to_owned(), limit.to_string()])
    }

    /// Render the interactive command for manual context compaction.
    ///
    /// Instructions ride after commands that accept them and are folded to a
    /// single line because commands are raw-typed and a newline submits them.
    pub fn compact_command(self, instruction: &str) -> Option<String> {
        let compact = self.compact_command?;
        let mut command = compact.command.to_owned();
        if compact.instruction == CompactInstruction::Trailing {
            let folded = instruction.split_whitespace().collect::<Vec<_>>().join(" ");
            if !folded.is_empty() {
                command.push(' ');
                command.push_str(&folded);
            }
        }
        Some(command)
    }

    fn static_preset_matcher(self, field: PresetField) -> Option<StaticPresetMatcher> {
        match field {
            PresetField::Model => self.presets.model,
            PresetField::Effort => self.presets.effort,
            PresetField::AutoCompact => self.presets.auto_compact,
            PresetField::SystemPromptFile => self.presets.system_prompt_file,
        }
    }

    /// Describe the provider-native argv spelling rendered for a preset field.
    pub fn preset_arg_matcher(self, field: PresetField) -> Option<PresetArgMatcher> {
        let matcher = self.static_preset_matcher(field)?;
        Some(match matcher {
            StaticPresetMatcher::Flag(flags) | StaticPresetMatcher::ValueFlag { flags, .. } => {
                PresetArgMatcher::Flag(flags.iter().map(|flag| (*flag).to_owned()).collect())
            }
            StaticPresetMatcher::TextFlag(flags) => {
                PresetArgMatcher::TextFlag(flags.iter().map(|flag| (*flag).to_owned()).collect())
            }
            StaticPresetMatcher::EnvPathVar(key) => PresetArgMatcher::EnvPathVar((*key).to_owned()),
            StaticPresetMatcher::EnvConfigMember { var, member } => {
                PresetArgMatcher::EnvConfigMember {
                    var: var.to_owned(),
                    member: member.to_owned(),
                }
            }
            StaticPresetMatcher::ConfigKey { flags, key } => PresetArgMatcher::ConfigKey {
                flags: flags.iter().map(|flag| (*flag).to_owned()).collect(),
                key: key.to_owned(),
            },
            StaticPresetMatcher::AgentFile(flag) => PresetArgMatcher::AgentFile(flag.to_owned()),
        })
    }

    /// Whether the provider can receive a composed system prompt.
    pub fn takes_system_prompt(self) -> bool {
        self.preset_arg_matcher(PresetField::SystemPromptFile)
            .is_some()
    }

    /// Whether a field is applied through argv or the adapter's session step.
    pub fn supports_preset(self, field: PresetField) -> bool {
        (matches!(field, PresetField::Model | PresetField::Effort) && self.model_session_step)
            || self.preset_arg_matcher(field).is_some()
    }

    /// The provider argv that turns fast mode on or forces it off (empty for an extension,
    /// or where off has nothing to force), or the provider's alternative to on.
    pub fn fast_args(self, on: bool) -> Result<Vec<String>, &'static str> {
        let owned = |args: &[&str]| args.iter().map(|arg| (*arg).to_owned()).collect();
        match self.presets.fast {
            FastSwitch::Args { on: args, .. } if on => Ok(owned(args)),
            FastSwitch::Args { off, .. } => Ok(owned(off)),
            FastSwitch::ExtensionEnv => Ok(Vec::new()),
            FastSwitch::Unsupported { fix, .. } if on => Err(fix),
            FastSwitch::Unsupported { .. } => Ok(Vec::new()),
        }
    }

    /// Omit unsupported tuning settings, leaving model rendering strict.
    pub(crate) fn normalize_preset(
        self,
        mut preset: LaunchPreset,
    ) -> (LaunchPreset, Vec<SkippedPresetField>) {
        let model = preset.model.clone();
        let mut skipped: Vec<_> = self
            .lower_max_effort(&mut preset, model.as_deref())
            .into_iter()
            .collect();
        if self.presets.auto_compact.is_none()
            && let Some(value) = preset.auto_compact.take()
        {
            skipped.push(SkippedPresetField::AutoCompact(value));
        }
        if !self.supports_preset(PresetField::Effort)
            && let Some(value) = preset.effort.take()
        {
            skipped.push(SkippedPresetField::Effort(value));
        }
        if let FastSwitch::Unsupported { fix, off_fix } = self.presets.fast {
            match preset.fast {
                Some(true) => skipped.push(SkippedPresetField::Fast {
                    alternative: (self.presets.fast != FastSwitch::NONE).then_some(fix),
                }),
                Some(false) => {
                    skipped.extend(off_fix.map(|fix| SkippedPresetField::FastOff { fix }))
                }
                None => {}
            }
            // Nothing renders either way, so the launch records fast as unset.
            preset.fast = None;
        }
        (preset, skipped)
    }

    /// Skip the effort of a catalog `model` that takes none. Only a launch's final model may
    /// drop it: a profile renders its effort for a `--model` override to keep.
    pub(crate) fn drop_refused_effort(
        self,
        preset: &mut LaunchPreset,
        model: Option<&str>,
    ) -> Option<SkippedPresetField> {
        let model = model?;
        self.definitions
            .models
            .iter()
            .find(|entry| entry.name == model || entry.id == model)
            .filter(|entry| !entry.takes_effort)?;
        preset
            .effort
            .take()
            .filter(|value| !value.is_empty())
            .map(SkippedPresetField::Effort)
    }

    /// Launch `max` effort at `model`'s highest level when the kind's effort list lacks
    /// `max`: the catalog entry's ceiling, else its foreign prefix's, else the list's highest
    /// level.
    fn lower_max_effort(
        self,
        preset: &mut LaunchPreset,
        model: Option<&str>,
    ) -> Option<SkippedPresetField> {
        let Some(StaticPresetMatcher::ValueFlag { values, .. }) =
            self.static_preset_matcher(PresetField::Effort)
        else {
            return None;
        };
        if preset.effort.as_deref() != Some("max") || values.contains(&"max") {
            return None;
        }
        let level = model
            .and_then(|model| {
                self.definitions
                    .models
                    .iter()
                    .find(|entry| entry.name == model || entry.id == model)
            })
            .and_then(|entry| entry.max_effort)
            .or_else(|| {
                let model = model?;
                self.definitions
                    .foreign_prefixes
                    .iter()
                    .find(|(prefix, _)| model.starts_with(prefix))
                    .map(|(_, ceiling)| *ceiling)
            })
            .or_else(|| values.last().copied())?;
        let value = preset.effort.replace(level.to_owned())?;
        Some(SkippedPresetField::EffortLowered { value, level })
    }

    /// Remove every exact rendered fast switch, on and off, from launch argv.
    pub fn strip_fast_args(self, args: &mut Vec<String>) {
        let FastSwitch::Args { on, off } = self.presets.fast else {
            return;
        };
        for switch in [on, off] {
            if switch.is_empty() {
                continue;
            }
            while let Some(index) = args
                .windows(switch.len())
                .position(|part| part.iter().zip(switch).all(|(arg, want)| arg == want))
            {
                args.drain(index..index + switch.len());
            }
        }
    }

    /// Which exact fast switch `args` carries, on before off.
    pub fn rendered_fast(self, args: &[String]) -> Option<bool> {
        let FastSwitch::Args { on, off } = self.presets.fast else {
            return None;
        };
        let carries = |switch: &[&str]| {
            !switch.is_empty()
                && args
                    .windows(switch.len())
                    .any(|part| part.iter().zip(switch).all(|(arg, want)| arg == want))
        };
        if carries(on) {
            Some(true)
        } else {
            carries(off).then_some(false)
        }
    }

    fn render_preset(
        self,
        agent_kind: &'static str,
        preset: &LaunchPreset,
    ) -> Result<Vec<String>, PresetErr> {
        let values: [(PresetField, &'static str, Option<String>); 3] = [
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
                PresetField::AutoCompact,
                "auto-compact",
                preset
                    .auto_compact
                    .clone()
                    .filter(|value| !value.is_empty()),
            ),
        ];
        let mut argv = Vec::new();
        for (field, field_name, value) in values {
            let Some(value) = value else { continue };
            if let Some(StaticPresetMatcher::ValueFlag { values, .. }) =
                self.static_preset_matcher(field)
                && !values.contains(&value.as_str())
            {
                return Err(PresetErr::UnsupportedValue {
                    agent: agent_kind,
                    field: field_name,
                    value,
                    values,
                });
            }
            if !self.supports_preset(field) {
                return Err(PresetErr::UnsupportedField {
                    agent: agent_kind,
                    field: field_name,
                });
            }
            let Some(matcher) = self.preset_arg_matcher(field) else {
                continue;
            };
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
                PresetArgMatcher::TextFlag(_)
                | PresetArgMatcher::EnvPathVar(_)
                | PresetArgMatcher::EnvConfigMember { .. }
                | PresetArgMatcher::AgentFile(_) => {
                    return Err(PresetErr::UnsupportedField {
                        agent: agent_kind,
                        field: field_name,
                    });
                }
            }
        }
        if let Some(on) = preset.fast {
            argv.extend(
                self.fast_args(on)
                    .map_err(|fix| PresetErr::UnsupportedFast {
                        agent: agent_kind,
                        fix,
                    })?,
            );
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
                PromptStyle::Positional => argv.push(prompt.to_owned()),
                PromptStyle::Flag(flag) => {
                    argv.extend([flag.to_owned(), prompt.to_owned()]);
                }
                PromptStyle::JoinedFlag(flag) => argv.push(format!("{flag}={prompt}")),
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
pub(super) struct DefinitionModel {
    pub name: &'static str,
    pub id: &'static str,
    pub effort: Option<&'static str>,
    /// The highest level this model takes, where `max` launches when the kind's effort list
    /// lacks it; `None` takes the list's highest level.
    pub max_effort: Option<&'static str>,
    /// Whether an effort may launch with this model; when false, any effort is skipped.
    pub takes_effort: bool,
}

/// Renders a tools list, plus provider tool names to append, each once.
type ToolRenderer<T> = fn(&super::tools::ToolSet, &[&str]) -> T;

#[derive(Clone, Copy, Debug)]
pub(super) enum DefinitionTools {
    Unsupported,
    Ignored,
    Required(ToolRenderer<Result<Vec<String>, super::tools::ToolErr>>),
    Optional(ToolRenderer<(Vec<String>, Vec<super::tools::ToolNotice>)>),
    /// Required, and written into the provider's agent file rather than argv: the
    /// renderer returns only the notices for names the file cannot carry.
    RequiredFile(ToolRenderer<Vec<super::tools::ToolNotice>>),
}

#[derive(Clone, Copy, Debug)]
pub(super) struct DefinitionSpec {
    pub variants: &'static [Variant],
    pub effort: Option<&'static str>,
    pub models: &'static [DefinitionModel],
    pub runs_other_providers_models: bool,
    pub prefixes: &'static [&'static str],
    /// Other providers' model prefixes this kind also runs, only for a definition that names it
    /// in `agent:`: routing by model leaves them to the kind that owns the prefix. Each carries
    /// the highest effort its models take here, where `max` launches.
    pub foreign_prefixes: &'static [(&'static str, &'static str)],
    /// The model series this adapter's own identity implies (omitted when
    /// rendered beside an agent of this kind).
    pub home_series: Option<&'static str>,
    pub tools: DefinitionTools,
}

impl DefinitionSpec {
    pub const EMPTY: Self = Self {
        variants: &[],
        effort: None,
        models: &[],
        runs_other_providers_models: false,
        prefixes: &[],
        foreign_prefixes: &[],
        home_series: None,
        tools: DefinitionTools::Unsupported,
    };
}

/// A named launch form of an existing agent kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Variant {
    pub name: &'static str,
    pub args: &'static [&'static str],
}

#[derive(Clone, Copy, Debug)]
pub struct SessionCommand {
    pub before_id: &'static [&'static str],
    pub after_id: &'static [&'static str],
}

/// An interactive context-compaction command and its instruction support.
#[derive(Clone, Copy, Debug)]
pub struct CompactCommand {
    pub command: &'static str,
    pub instruction: CompactInstruction,
}

/// Whether a compact command accepts free text that steers its summary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactInstruction {
    /// The command takes no arguments, so guidance is dropped.
    Unsupported,
    /// Free text after the command steers the summary.
    Trailing,
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
    /// A bare trailing positional, for a CLI whose own parser drops any
    /// argument that follows `--`.
    Positional,
    Flag(&'static str),
    JoinedFlag(&'static str),
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
    pub(super) default: Option<PermissionMode>,
    pub(super) mapped_modes: [PermissionMode; 4],
    pub(super) flags: &'static [&'static str],
    pub(super) config_keys: &'static [&'static str],
}

impl LaunchPermissionArgs {
    pub const EMPTY: Self = Self {
        ask: &[],
        auto: &[],
        yolo: &[],
        plan: &[],
        default: None,
        mapped_modes: [
            PermissionMode::Plan,
            PermissionMode::Ask,
            PermissionMode::Auto,
            PermissionMode::Yolo,
        ],
        flags: &[],
        config_keys: &[],
    };

    /// The kind's permission table, or identity mapping without a default for an unknown kind.
    pub fn of_kind(kind: &str) -> Self {
        super::registry::spec_by_kind(kind).map_or(Self::EMPTY, |spec| spec.launch.permission)
    }

    /// Resolve a requested mode or the provider default to an available mode.
    pub fn resolve_mode(self, requested: Option<PermissionMode>) -> Option<PermissionMode> {
        requested
            .or(self.default)
            .map(|mode| self.mapped_modes[usize::from(mode.rank())])
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PresetMatchers {
    pub model: Option<StaticPresetMatcher>,
    pub effort: Option<StaticPresetMatcher>,
    pub auto_compact: Option<StaticPresetMatcher>,
    pub system_prompt_file: Option<StaticPresetMatcher>,
    pub fast: FastSwitch,
    pub code_mode: CodeModeSwitch,
}

impl PresetMatchers {
    pub const EMPTY: Self = Self {
        model: None,
        effort: None,
        auto_compact: None,
        system_prompt_file: None,
        fast: FastSwitch::NONE,
        code_mode: CodeModeSwitch::Unsupported,
    };
}

/// How a provider is launched in its fast mode (priority serving).
///
/// Fast takes three states: on renders the provider's on switch or tells RimZ's extension
/// through the launch env; off renders its off switch, so a fast mode saved inside the
/// provider no longer applies; unset renders nothing, so the provider's own default
/// stands. An unsupported provider carries the alternative its skip warning names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FastSwitch {
    /// Exact argv appended for on and for off, each stripped when a launch flips it.
    /// Claude's inline `--settings` member is folded into the launch's one `--settings` at
    /// the process boundary. An empty `off` is a provider with nothing saved to force out.
    Args {
        on: &'static [&'static str],
        off: &'static [&'static str],
    },
    /// RimZ's extension requests priority serving through the launch env,
    /// without adding provider argv; off sends no request.
    ExtensionEnv,
    /// No launch switch; `fix` tells the user what to do instead of on. `off_fix`, set
    /// where the provider can hold a saved fast mode no launch forces out, makes off warn
    /// with what to do instead.
    Unsupported {
        fix: &'static str,
        off_fix: Option<&'static str>,
    },
}

impl FastSwitch {
    /// A provider with no fast mode RimZ can reach and no alternative to name.
    pub const NONE: Self = Self::Unsupported {
        fix: "launch without --fast",
        off_fix: None,
    };
}

/// What a provider's code mode adds to a launch.
///
/// Code mode takes three states: on adds the provider's code-mode tools, argv, or env;
/// off forces it off where the provider can turn it on by itself; unset renders nothing.
/// An unsupported provider warns on on and launches without it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeModeSwitch {
    Supported {
        /// Provider tool names appended, each once, to a rendered tools list.
        tools: &'static [&'static str],
        /// Argv added when the launch renders no tools list.
        without_tools: &'static [&'static str],
        /// Env pair an on launch sets; a later launch clears it only where RimZ stamped it.
        env: Option<(&'static str, &'static str)>,
        /// Env pair an off launch sets over any inherited or trusted value, stamped like
        /// `env`.
        off_env: Option<(&'static str, &'static str)>,
        /// `(flags, tool)` an off launch excludes: the tool joins the list of the last of
        /// these flags (one flag's spellings, the first appended when none is present) in
        /// the final argv, since the provider honours only the last.
        off_exclude: Option<(&'static [&'static str], &'static str)>,
    },
    /// Exact argv an on or an off launch appends, with or without a tools list.
    Args {
        on: &'static [&'static str],
        off: &'static [&'static str],
    },
    /// A member the adapter writes into the provider config it composes in
    /// `finalize_launch_env`: the provider's on value or its off value. An on or an off launch
    /// records `marker` in `RIMZ_CODE_MODE_CONFIG`, which launches of other kinds pass through,
    /// so a nested unset launch of this kind can take the member back out of the config it
    /// inherits.
    Config {
        marker: &'static str,
    },
    Unsupported,
}

impl CodeModeSwitch {
    /// The tool names a rendered tools list gains.
    pub(super) fn tools(self, on: bool) -> &'static [&'static str] {
        match self {
            Self::Supported { tools, .. } if on => tools,
            _ => &[],
        }
    }

    /// The env pair a launch in this state sets, on or forced off.
    pub fn env(self, on: bool) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Supported { env, .. } if on => env,
            Self::Supported { off_env, .. } => off_env,
            Self::Args { .. } | Self::Config { .. } | Self::Unsupported => None,
        }
    }

    /// Join an off launch's exclusion into its final `argv`, into the last exclusion list
    /// it carries under any spelling.
    pub fn exclude_off(self, argv: &mut Vec<String>) {
        let Self::Supported {
            off_exclude: Some((flags, tool)),
            ..
        } = self
        else {
            return;
        };
        let last = argv
            .iter()
            .rposition(|arg| flags.contains(&arg.as_str()))
            .filter(|at| at + 1 < argv.len());
        match last {
            Some(at) => {
                let list = &mut argv[at + 1];
                if !list.split(',').any(|entry| entry == tool) {
                    list.push(',');
                    list.push_str(tool);
                }
            }
            None => argv.extend([flags[0].to_owned(), tool.to_owned()]),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum StaticPresetMatcher {
    Flag(&'static [&'static str]),
    ValueFlag {
        flags: &'static [&'static str],
        values: &'static [&'static str],
    },
    TextFlag(&'static [&'static str]),
    EnvPathVar(&'static str),
    EnvConfigMember {
        var: &'static str,
        member: &'static str,
    },
    ConfigKey {
        flags: &'static [&'static str],
        key: &'static str,
    },
    /// A provider agent file selected by name through this flag.
    AgentFile(&'static str),
}

/// Provider-identity check for a [`bin_names`](AgentSpec::bin_names) entry that
/// another provider's installer also ships under the same filename, where a
/// bare filename match cannot prove the located binary is this provider's. No
/// built-in claims such a name today; this is the hook for one that must.
#[derive(Clone, Copy, Debug)]
pub struct BinIdentity {
    /// The subset of [`bin_names`](AgentSpec::bin_names) that collide with
    /// another provider's install alias. A candidate matched by one of these is
    /// accepted only after `verify` confirms it; every other name matches on
    /// filename alone.
    pub ambiguous: &'static [&'static str],
    /// Confirms a candidate is genuinely this provider from its `--version`
    /// stdout and stderr. The adapter's own version parser recognizes only its
    /// own release banner, so a colliding alias fails the check and discovery
    /// skips it.
    pub verify: fn(&str, &str) -> bool,
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
    /// How a provider display name or raw plan tier becomes a brand label.
    pub plan_label: PlanLabel,
    /// Weekly capacity per subscription plan, keyed by raw plan tier; `None`
    /// when the provider's plans are not sized.
    pub plan_capacities: Option<PlanCapacities>,
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
    /// Declared user-facing capability checklist — the six marks the
    /// compatibility matrix prints. Every [`UserCapability`] appears exactly
    /// once as full, partial, or unsupported, phrased in what the user sees;
    /// conformance cross-checks each mark against the concerns backing it.
    pub user_coverage: UserCoverage,
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
    /// kind; Cursor claims only `cursor-agent`, since `cursor` is the IDE and
    /// its installer's generic `agent` link collides with other providers'.
    pub bin_names: &'static [&'static str],
    /// Identity check that keeps an ambiguous [`bin_names`](Self::bin_names)
    /// entry from resolving to a different provider's colliding install alias.
    /// `None` when every `bin_names` entry is provider-unique and a filename
    /// match settles identity.
    pub bin_identity: Option<BinIdentity>,
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
    pub host_skills: super::skills::HostSkills,
    pub tool_rules: super::skills::ToolRules,
}

/// The provider-neutral registry value for one agent integration.
///
/// Callers resolve a definition and use its workflow methods; the provider
/// implementation remains an internal detail of the catalog.
pub struct AgentDefinition {
    core: &'static dyn super::capabilities::AgentIntegration,
}

#[derive(Debug, thiserror::Error)]
#[error("invalid agent definition `{kind}`: {reason}")]
pub struct DefinitionValidationError {
    kind: &'static str,
    reason: String,
}

impl AgentDefinition {
    pub const fn new(core: &'static dyn super::capabilities::AgentIntegration) -> Self {
        Self { core }
    }

    pub fn spec(&self) -> &'static AgentSpec {
        self.core.spec()
    }

    /// Whether the kind's system prompt rides an agent file, which also carries the launch's
    /// `tools` and `mcps` lists, so a launch with no prompt sources applies neither list.
    pub fn rides_agent_file(&self) -> bool {
        matches!(
            self.spec()
                .launch
                .preset_arg_matcher(PresetField::SystemPromptFile),
            Some(PresetArgMatcher::AgentFile(_))
        )
    }

    /// Start of the verified `RIMZ_AGENT_PID` owner, bounding the age of a
    /// statusline's first windows after an in-process session switch.
    /// Missing, mismatched or unreadable owners supply no floor.
    pub fn context_owner_start(&self) -> Option<jiff::Timestamp> {
        context_owner_start_with(
            self.spec(),
            std::env::var("RIMZ_AGENT_PID").ok().as_deref(),
            crate::proc::comm,
            crate::proc::process_start,
        )
    }

    /// The same floor for a reading reported by a child of the provider
    /// process itself (a mod's `$.process.run`): its parent is the owner.
    pub fn parent_owner_start(&self) -> Option<jiff::Timestamp> {
        context_owner_start_with(
            self.spec(),
            Some(&std::os::unix::process::parent_id().to_string()),
            crate::proc::comm,
            crate::proc::process_start,
        )
    }

    pub fn is_foreign_hook_payload(&self, payload: &Value) -> bool {
        super::registry::all_definitions().any(|definition| {
            definition.spec().kind != self.spec().kind
                && definition.recognizes_hook_payload(payload)
        })
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
        Ok(())
    }

    pub fn concern_coverage(&self, concern: IntegrationConcern) -> ConcernCoverage {
        self.core.spec().coverage.get(concern)
    }

    pub fn lifecycle_coverage(&self, signal: LifecycleSignalKind) -> HookCoverage {
        self.core.spec().lifecycle_hooks.get(signal)
    }

    pub fn capability_level(&self, capability: UserCapability) -> CapabilityLevel {
        self.core.spec().user_coverage.get(capability)
    }

    /// Attach additive model context when the decoded native event accepts it,
    /// in the reply shape the spec declares.
    pub fn attach_hook_context(&self, decoded: &mut super::HookOutput, text: &str) -> bool {
        let field = match self.spec().capabilities.hook_context {
            Some(HookContextReply::HookSpecificOutput { event_name })
                if decoded.event_name() == "PostToolUse" =>
            {
                let output = if event_name {
                    serde_json::json!({"hookEventName": "PostToolUse", "additionalContext": text})
                } else {
                    serde_json::json!({"additionalContext": text})
                };
                ("hookSpecificOutput".to_owned(), output)
            }
            Some(HookContextReply::TopLevel { key }) if decoded.event_name() == "postToolUse" => {
                (key.to_owned(), Value::from(text))
            }
            _ => return false,
        };
        decoded.merge_reply_object([field]);
        true
    }
}

/// Every capability method reaches the adapter directly. The trait default in
/// [`super::capabilities`] is the single home for "this agent does not do that".
impl std::ops::Deref for AgentDefinition {
    type Target = dyn super::capabilities::AgentIntegration;

    fn deref(&self) -> &Self::Target {
        self.core
    }
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

/// Fallback grouping of transcript files into sessions when neither a row nor
/// its path supplies a native session id.
#[derive(Clone, Copy, Debug)]
pub enum ThreadKey {
    /// One transcript file per session — the file path is the thread.
    PerFile,
    /// The transcript's immediate parent directory is the session.
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

/// How a provider display name or raw plan tier becomes its brand label.
#[derive(Debug)]
pub enum PlanLabel {
    /// Prefix the display name, or the title-cased tier without one.
    Prefixed { prefix: &'static str },
    /// Prefix the display name, otherwise an exact tier's declared name or title-case.
    NamedTiers {
        prefix: &'static str,
        tiers: &'static [(&'static str, &'static str)],
    },
    /// The display name verbatim, otherwise the title-cased tier — for an agent whose
    /// sessions span many provider accounts, where no single brand prefix is honest.
    TitleCaseOnly,
}

impl PlanLabel {
    /// Prefer the provider's display name verbatim, otherwise its declared tier vocabulary.
    pub fn format(&self, raw: &str, name: Option<&str>) -> String {
        let tier = name.map_or_else(|| crate::theme::provider_title_case(raw), str::to_owned);
        match self {
            Self::Prefixed { prefix } => format!("{prefix} {tier}"),
            Self::NamedTiers { prefix, tiers } => {
                let name = name
                    .or_else(|| {
                        tiers
                            .iter()
                            .find_map(|(key, name)| (*key == raw).then_some(*name))
                    })
                    .unwrap_or(&tier);
                format!("{prefix} {name}")
            }
            Self::TitleCaseOnly => tier,
        }
    }
}

/// A provider's subscription plans sized in one weekly unit.
#[derive(Debug)]
pub struct PlanCapacities {
    /// The unit every plan's `units` counts: 1% of the provider's 1x plan's
    /// week (`claude_pro_week_percent`). Units of different providers never
    /// compare.
    pub unit: &'static str,
    pub plans: &'static [PlanCapacity],
}

/// One subscription plan: its stable key, the raw tiers that name it, and its
/// weekly capacity in the provider's unit.
#[derive(Debug)]
pub struct PlanCapacity {
    pub key: &'static str,
    pub tiers: &'static [&'static str],
    pub units: u32,
}

impl PlanCapacities {
    /// The plan an exact raw tier names; `None` for a tier no plan lists.
    pub fn resolve(&self, tier: &str) -> Option<&PlanCapacity> {
        self.plans.iter().find(|plan| plan.tiers.contains(&tier))
    }
}

/// The agent's tool vocabulary, classified for lifecycle and native blocking prompts.
#[derive(Debug)]
pub struct ToolClassification {
    /// Raw provider-payload key that holds a tool call's arguments. `None`
    /// when the protocol exposes no arguments RimZ can compare accurately.
    pub input_key: Option<&'static str>,
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

impl ToolClassification {
    /// What the named tool did: editing when it is in `editing`, else
    /// mutating when it is in `mutating`, else observed.
    pub(super) fn effect(&self, name: &str) -> ToolEffect {
        match (self.editing.contains(&name), self.mutating.contains(&name)) {
            (true, _) => ToolEffect::Editing,
            (false, true) => ToolEffect::Mutating,
            (false, false) => ToolEffect::Observed,
        }
    }
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
    LaunchReminders => "remind",
    BackgroundParking => "bg",
    BackgroundShells => "bgsh",
    SessionEnd => "end",
    IdleNotification => "idle",
    ContextUsage => "usage",
    RealtimeCost => "live$",
    RichContext => "rich",
    HookInstall => "install",
    AccountSpend => "spend",
    ToolStats => "tools",
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
    /// RimZ-composed launch reminders (subagent policy, team launch context)
    /// reaching the agent's system or developer prompt through
    /// `append_system_text_channel`.
    pub launch_reminders: ConcernCoverage,
    pub background_parking: ConcernCoverage,
    /// Background shells listed on the card while they run.
    pub background_shells: ConcernCoverage,
    pub session_end: ConcernCoverage,
    pub idle_notification: ConcernCoverage,
    pub context_usage: ConcernCoverage,
    pub realtime_cost: ConcernCoverage,
    pub rich_context: ConcernCoverage,
    pub hook_install: ConcernCoverage,
    pub account_spend: ConcernCoverage,
    pub tool_stats: ConcernCoverage,
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
            IntegrationConcern::LaunchReminders => self.launch_reminders,
            IntegrationConcern::BackgroundParking => self.background_parking,
            IntegrationConcern::BackgroundShells => self.background_shells,
            IntegrationConcern::SessionEnd => self.session_end,
            IntegrationConcern::IdleNotification => self.idle_notification,
            IntegrationConcern::ContextUsage => self.context_usage,
            IntegrationConcern::RealtimeCost => self.realtime_cost,
            IntegrationConcern::RichContext => self.rich_context,
            IntegrationConcern::HookInstall => self.hook_install,
            IntegrationConcern::AccountSpend => self.account_spend,
            IntegrationConcern::ToolStats => self.tool_stats,
            IntegrationConcern::RemoteControl => self.remote_control,
        }
    }

    pub fn iter(self) -> impl Iterator<Item = (IntegrationConcern, ConcernCoverage)> {
        IntegrationConcern::ALL
            .into_iter()
            .map(move |concern| (concern, self.get(concern)))
    }
}

/// The six capabilities the user-facing compatibility matrix names. These are
/// the questions a person asks before picking an agent, coarser than
/// [`IntegrationConcern`] and phrased in what they can see rather than what the
/// adapter reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserCapability {
    /// The card tracks the agent's whole life: start, working, waiting, idle, end.
    State,
    /// What the card shows mid-turn: context fill, token breakdown, live dollars.
    Live,
    /// Past sessions read end to end, with per-turn tokens and dollars.
    History,
    /// Login, plan, and each usage window with its fill and reset.
    Account,
    /// A blocked agent raising Waiting, and its question reaching `rimz asks`.
    Ask,
    /// Child agents nested under the parent card while they work.
    Subagents,
}

impl UserCapability {
    pub const ALL: [Self; 6] = [
        Self::State,
        Self::Live,
        Self::History,
        Self::Account,
        Self::Ask,
        Self::Subagents,
    ];

    pub const fn short_label(self) -> &'static str {
        match self {
            Self::State => "state",
            Self::Live => "live",
            Self::History => "history",
            Self::Account => "account",
            Self::Ask => "ask",
            Self::Subagents => "subagents",
        }
    }
}

/// What a user gets for one capability. The mark answers what they see and when
/// they see it; how RimZ obtains the figure stays out of it. A value folded from
/// a transcript tail and a value pushed by a native hook both read [`Full`] when
/// the surface is complete and current, and a native signal carrying half the
/// story reads [`Partial`].
///
/// [`Full`]: CapabilityLevel::Full
/// [`Partial`]: CapabilityLevel::Partial
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapabilityLevel {
    /// Complete and live: the capability reads the way it does on Claude Code.
    /// `note` names what the user gets, in their own terms.
    Full { note: &'static str },
    /// A working version with a stated limit — part of the detail, or the whole
    /// of it a beat late. `shows` is what lands, `limit` what the user can still
    /// see is missing.
    Partial {
        shows: &'static str,
        limit: &'static str,
    },
    /// Nothing to render for this capability; `reason` says why.
    Unsupported { reason: &'static str },
}

impl CapabilityLevel {
    pub const fn is_full(self) -> bool {
        matches!(self, Self::Full { .. })
    }

    pub const fn is_unsupported(self) -> bool {
        matches!(self, Self::Unsupported { .. })
    }

    /// The text the matrix prints after the capability label: what a full cell
    /// gives, what a partial cell still lacks, why an unsupported cell is empty.
    pub const fn detail(self) -> &'static str {
        match self {
            Self::Full { note } => note,
            Self::Partial { limit, .. } => limit,
            Self::Unsupported { reason } => reason,
        }
    }
}

/// Compile-time-complete user-facing capability claims for one adapter. Every
/// [`UserCapability`] appears exactly once; conformance checks each mark against
/// the [`CoverageAnnotations`] that back it, so a full claim always rests on
/// wired mechanism while a wired mechanism may still roll up to partial when the
/// user-visible result is incomplete or late.
#[derive(Clone, Copy, Debug)]
pub struct UserCoverage {
    pub state: CapabilityLevel,
    pub live: CapabilityLevel,
    pub history: CapabilityLevel,
    pub account: CapabilityLevel,
    pub ask: CapabilityLevel,
    pub subagents: CapabilityLevel,
}

impl UserCoverage {
    pub const fn get(self, capability: UserCapability) -> CapabilityLevel {
        match capability {
            UserCapability::State => self.state,
            UserCapability::Live => self.live,
            UserCapability::History => self.history,
            UserCapability::Account => self.account,
            UserCapability::Ask => self.ask,
            UserCapability::Subagents => self.subagents,
        }
    }

    pub fn iter(self) -> impl Iterator<Item = (UserCapability, CapabilityLevel)> {
        UserCapability::ALL
            .into_iter()
            .map(move |capability| (capability, self.get(capability)))
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

/// How a native post-tool hook reply carries additive model context.
#[derive(Clone, Copy, Debug)]
pub enum HookContextReply {
    /// `PostToolUse` nests the text under `hookSpecificOutput.additionalContext`,
    /// echoing `hookEventName` when the agent requires it.
    HookSpecificOutput { event_name: bool },
    /// `postToolUse` carries the text at the top level under `key`.
    TopLevel { key: &'static str },
}

/// Operational policy that cannot be derived from integration coverage.
#[derive(Clone, Copy, Debug)]
pub struct Capabilities {
    /// The reply shape when the agent accepts additive model context in a
    /// native post-tool hook reply; `None` when it has no such channel.
    pub hook_context: Option<HookContextReply>,
    /// Accepts additive model context in a native prompt-submit hook reply.
    pub prompt_context: bool,
    /// Renders its own ask UI in the pane — permission prompts, plan
    /// approvals, questions — so RimZ can mark the agent waiting while the
    /// prompt stays in the native UI. An agent without one (pi gates tools
    /// only through the extension) resolves the same ask neutrally with no
    /// waiting state: there is no native prompt to route the human to.
    pub native_ask_ui: bool,
    /// Permission hooks precede the provider's automatic reviewer, rather
    /// than necessarily opening a human prompt.
    pub permission_before_auto_review: bool,
    /// Local transcript/rollout tail is a live context source refreshable
    /// outside hooks. Drives producer ticks and renderer transcript watches.
    pub transcript_tail_context: bool,
    /// The provider's context sidecar keeps its pre-compaction occupancy
    /// reading across a compaction until a later turn updates it, so the store
    /// stamps that reading onto a usage-retiring close event for the fold to
    /// retire.
    pub context_lags_compaction: bool,
    /// Lifecycle hook events report context fill during the turn, so the
    /// smart-compaction reminder's repeat guard requires a fold-observed
    /// low-water reading under the seat's line in addition to a moved
    /// compaction count.
    pub lifecycle_reports_fill: bool,
    /// Registers its session lazily and/or routes hooks through a daemon, so
    /// an instance can be present without a stamped session. The sidebar binds
    /// such a session to its pane by cwd.
    pub registers_lazily: bool,
    /// A resume reports the provider's session start (a lifecycle hook under the resumed session
    /// id) before its first prompt, so a resume follow-up waits for that report instead of a
    /// timed hold, and fails at its deadline without it.
    pub reports_start_on_resume: bool,
    /// How the room host reads provider-local session observations, without
    /// executable hooks or RimZ store writes.
    pub local_session_discovery: LocalSessionDiscovery,
    /// Sessions route hooks through a per-user daemon that outlives any one
    /// conversation, so a new session may succeed another in the same pane
    /// before the reaper clears the stamp.
    pub daemon_hooked_sessions: bool,
    /// Provides an authoritative, identity-bearing direct account-usage
    /// probe. Scheduling uses this static declaration before provider work.
    pub direct_account_usage: bool,
    /// How co-resident roots are ordered when one agent process carries
    /// more than one conversation. Every same-instance conversation carries
    /// launch identity; only the selected occupant claims launch-derived
    /// handles. Without on-screen evidence, rested roots follow latest
    /// activity, independent of this policy.
    pub same_pane_session: SamePaneSessionPolicy,
    /// Remote-control surfaces the provider can host.
    pub remote_control: RemoteControlCapability,
}

/// Provider-local session discovery performed by the room host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalSessionDiscovery {
    /// No provider-local discovery.
    Unsupported,
    /// Scan the provider store for admitted workspaces through
    /// `SessionCapability::discover_local_sessions`.
    FullScan,
    /// Read only what live panes can bind through
    /// `SessionCapability::discover_pane_sessions`. The full scan remains
    /// available to `rimz agents resume`.
    PerPane,
}

/// Pane ownership for root sessions hosted by one live agent process. The
/// policy selects the launch's current occupant; without on-screen evidence
/// it orders open turns and leaves rested-root ordering to activity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SamePaneSessionPolicy {
    /// Among open turns, keep the earliest registered as the current occupant.
    KeepPrimary,
    /// With on-screen evidence, prefer open asks, then the later claim.
    /// Otherwise select the most recently registered among open turns.
    FollowLatest,
}

/// Static remote-control capability. Dynamic "enabled on this machine" state
/// lives on [`AgentDefinition`], because it may read provider
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
    /// a declared name under a target-triple release-binary suffix. A name
    /// ending in `<version>` opts into a numeric version prefix, including
    /// the kernel's truncated process name.
    pub fn runs_as(&self, name: &str) -> bool {
        self.process_names
            .iter()
            .any(|program| match program.strip_suffix("<version>") {
                Some(prefix) => name
                    .strip_prefix(prefix)
                    .is_some_and(|version| version.starts_with(|ch: char| ch.is_ascii_digit())),
                None => program_names_kind(name, program),
            })
    }

    /// Whether a command basename launches this agent. This is distinct from
    /// [`runs_as`](Self::runs_as): a resolved executable may expose one name
    /// (`cursor-agent` for Cursor) while its kernel `comm` names the runtime
    /// instead. Target-triple suffix matching applies to each declared
    /// name rather than implicitly accepting the kind, so `antigravity` does
    /// not shadow the actual `agy` process. Only RimZ's `rimz agents exec
    /// <kind>` wrapper is matched by kind, in the registry's classifiers.
    pub fn launches_as(&self, name: &str) -> bool {
        self.bin_names
            .iter()
            .any(|program| program_names_kind(name, program))
    }

    /// The identity check to run against a candidate located by `name`, when
    /// `name` is one of this agent's ambiguous [`bin_names`](Self::bin_names).
    /// `None` for a provider-unique name, which discovery accepts on filename
    /// alone.
    pub(crate) fn ambiguous_bin_identity(&self, name: &str) -> Option<&BinIdentity> {
        self.bin_identity
            .as_ref()
            .filter(|identity| identity.ambiguous.contains(&name))
    }

    /// What the tool a tool-use payload names did to the workspace. The tool
    /// name rides `tool_name` in every provider's payload; a payload without
    /// one is [`ToolEffect::Observed`].
    pub(super) fn tool_effect(&self, payload: &Value) -> ToolEffect {
        payload
            .get("tool_name")
            .and_then(Value::as_str)
            .map_or(ToolEffect::Observed, |name| self.tools.effect(name))
    }

    /// A stable digest of a tool call's identity: its name plus its arguments.
    /// Returns `None` when the adapter declares no input key or the payload
    /// carries neither a name nor input there, keeping repeat detection off
    /// instead of falling back to an imprecise name-only key.
    pub fn tool_signature(&self, payload: &Value) -> Option<String> {
        let name = payload.get("tool_name").and_then(Value::as_str)?.trim();
        if name.is_empty() {
            return None;
        }
        let input = payload.get(self.tools.input_key?)?;
        let mut hasher = Sha256::new();
        hasher.update(name.as_bytes());
        hasher.update([0]);
        hasher.update(serde_json::to_vec(input).ok()?);
        let digest = hasher.finalize();
        Some(
            digest[..8]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        )
    }

    /// Whether a pre-tool-use payload names a blocking ask tool.
    pub fn blocking_tool_kind(&self, tool_name: Option<&str>) -> Option<AskKind> {
        let name = tool_name?;
        self.tools
            .blocking
            .iter()
            .find_map(|(tool, kind)| (*tool == name).then_some(*kind))
    }
}

#[cfg(test)]
mod tests;
