//! Amp plugin adapter.
//!
//! Amp has no command-hook or statusline protocol. RimZ installs a small
//! observation-only TypeScript plugin that forwards the active thread's native
//! lifecycle events without entering Amp's tool-decision path.

pub(crate) mod account;
pub(crate) mod payloads;
mod spend;
mod thread;
mod transcript;

pub(crate) use crate::agents::capabilities::*;

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::definition::{
    AgentSpec, Brand, Capabilities, CapabilityLevel, ConcernCoverage, CoverageAnnotations,
    HookCoverage, LifecycleAnnotations, PlanLabel, RealtimeUsageChannel, RemoteControlCapability,
    ThreadKey, ToolClassification, UserCoverage,
};
use super::hook_types::{HookEventSpec, decode_catalog_hook};
use super::lifecycle::LifecycleSignal;
use super::managed_source::ManagedSource;
use super::{
    AgentCurrentUsage, AgentErr, AgentLifecycleObservation, AgentTokenUsage, AskKind, FieldPatch,
    HookOutput, HookRouting, LocalContextPatch, LocalContextRefresh, LocalContextRefreshCtx,
    LocalTokenPatch, RefreshTrigger, Result, SessionOrigin, TranscriptStat, non_empty_trimmed,
    sanitize_user_prompt,
};
use crate::ids::AgentSessionId;

static AMP_DESCRIPTOR: AgentSpec = AgentSpec {
    kind: "amp",
    aliases: &[],
    display_name: "Amp",
    brand: Brand {
        emblem: None,
        color: 255,
        color_rgb: (0xee, 0xee, 0xee),
    },
    plan_label: PlanLabel::TitleCaseOnly,
    sub_providers: &[],
    expected_windows: &[],
    tools: ToolClassification {
        input_key: None,
        mutating: &["shell_command", "apply_patch", "create_file", "edit_file"],
        editing: &["apply_patch", "create_file", "edit_file"],
        blocking: &[],
    },
    capabilities: Capabilities {
        native_ask_ui: true,
        transcript_tail_context: false,
        registers_lazily: false,
        local_session_discovery: false,
        daemon_hooked_sessions: false,
        direct_account_usage: false,
        same_pane_session: super::SamePaneSessionPolicy::KeepPrimary,
        realtime_usage: RealtimeUsageChannel {
            windows_defer_to_fresh_realtime: false,
        },
        remote_control: RemoteControlCapability {
            pane_sessions: false,
            background_sessions: false,
        },
    },
    coverage: AMP_COVERAGE,
    user_coverage: AMP_USER_COVERAGE,
    lifecycle_hooks: AMP_LIFECYCLE_HOOKS,
    default_context_window: None,
    default_model: None,
    process_names: &["amp", "node"],
    bin_names: &["amp"],
    bin_identity: None,
    extra_bin_dirs: &[],
    thread_key: ThreadKey::PerFile,
    launch: super::LaunchSpec {
        program: Some("amp"),
        fixed_args: &[],
        prompt: super::PromptStyle::FlagWithSuffix {
            flag: "-x",
            suffix: &["--plugin-ready-timeout", "30"],
        },
        resume: Some(super::SessionCommand {
            before_id: &["amp", "threads", "continue"],
            after_id: &[],
        }),
        fork: None,
        permission: super::LaunchPermissionArgs::EMPTY,
        max_turn_flag: None,
        compact_command: None,
        presets: super::PresetMatchers {
            model: Some(super::StaticPresetMatcher::Flag(&["--mode"])),
            effort: Some(super::StaticPresetMatcher::Flag(&["--effort"])),
            system_prompt_file: None,
            append_system_prompt_file: None,
        },
    },
};

const AMP_COVERAGE: CoverageAnnotations = CoverageAnnotations {
    turn_lifecycle: ConcernCoverage::Wired {
        via: "agent_start/agent_end",
    },
    permission: ConcernCoverage::Wired {
        via: "thread-state awaiting-approval",
    },
    plan_approval: ConcernCoverage::Unsupported {
        reason: "no native event",
    },
    user_question: ConcernCoverage::Unsupported {
        reason: "no native event",
    },
    answer: ConcernCoverage::Unsupported {
        reason: "no external resolver",
    },
    compaction: ConcernCoverage::Unsupported {
        reason: "automatic compaction has no event",
    },
    subagents: ConcernCoverage::Unsupported {
        reason: "interactive events expose no durable child identity",
    },
    background_parking: ConcernCoverage::Unsupported {
        reason: "no background-task parking signal",
    },
    session_end: ConcernCoverage::Partial {
        via: "pane liveness + rollup reaper",
        gap: "no session-end event",
    },
    idle_notification: ConcernCoverage::Partial {
        via: "turn-end + awaiting-approval + stall window",
        gap: "no notification event",
    },
    context_usage: ConcernCoverage::Partial {
        via: "private thread cache on turn boundaries + producer tick",
        gap: "no stable context-window divisor",
    },
    realtime_cost: ConcernCoverage::Partial {
        via: "private thread cache tokens + estimated model pricing",
        gap: "60s mid-turn cadence and unknown models can omit dollars",
    },
    rich_context: ConcernCoverage::Unsupported {
        reason: "no out-of-band context transport",
    },
    hook_install: ConcernCoverage::Wired {
        via: "~/.config/amp/plugins/rimz.ts",
    },
    account_spend: ConcernCoverage::Partial {
        via: "private rewritten thread-cache fold",
        gap: "estimated pricing does not reconcile Amp credits or workspace billing",
    },
    tool_stats: ConcernCoverage::Unsupported {
        reason: "tool statistics are not integrated for this adapter",
    },
    remote_control: ConcernCoverage::Unsupported {
        reason: "readiness is not detectable",
    },
};

const AMP_USER_COVERAGE: UserCoverage = UserCoverage {
    state: CapabilityLevel::Full {
        note: "the card follows every turn and clears when the pane goes away",
    },
    live: CapabilityLevel::Partial {
        shows: "token totals and an estimated dollar figure, refreshed every 60 seconds",
        limit: "no context-window fill, and unknown models show no dollars",
    },
    history: CapabilityLevel::Partial {
        shows: "past threads read end to end with tokens and estimated dollars",
        limit: "estimates do not reconcile against Amp credits or workspace billing",
    },
    account: CapabilityLevel::Partial {
        shows: "your Amp identity and estimated thread spend",
        limit: "no plan tier and no usage window",
    },
    ask: CapabilityLevel::Full {
        note: "a thread waiting on approval raises Waiting and reaches rimz asks",
    },
    subagents: CapabilityLevel::Unsupported {
        reason: "Amp's events expose no durable child identity",
    },
};

const AMP_LIFECYCLE_HOOKS: LifecycleAnnotations = LifecycleAnnotations {
    registered: HookCoverage::Native {
        event: "session_start",
    },
    turn_started: HookCoverage::Native {
        event: "agent_start",
    },
    turn_ended: HookCoverage::Native { event: "agent_end" },
    tool_used: HookCoverage::Native {
        event: "tool_result",
    },
    awaiting_input: HookCoverage::Native {
        event: "permission_ask",
    },
    subagent_started: HookCoverage::Absent {
        reason: "no interactive subagent event",
    },
    subagent_stopped: HookCoverage::Absent {
        reason: "no interactive subagent event",
    },
    compacting: HookCoverage::Absent {
        reason: "automatic compaction has no event",
    },
    compaction_ended: HookCoverage::Absent {
        reason: "automatic compaction has no event",
    },
    ended: HookCoverage::Derived {
        via: "pane liveness + rollup reaper",
        gap: "no session-end event",
    },
    lost: HookCoverage::Derived {
        via: "rimz exec wrapper",
        gap: "native hooks do not report mux-session death",
    },
};

const AMP_HOOKS: &[HookEventSpec] = &[
    HookEventSpec::lifecycle(
        "session_start",
        r#"{"session_id":"T-abc123","cwd":"/tmp/repo"}"#
    )
    .progress(),
    HookEventSpec::lifecycle(
        "agent_start",
        r#"{"session_id":"T-abc123","prompt":"fix auth"}"#
    )
    .progress(),
    HookEventSpec::lifecycle(
        "tool_result",
        r#"{"session_id":"T-abc123","tool_name":"apply_patch","status":"done","files_modified":true}"#
    )
    .progress(),
    HookEventSpec::lifecycle(
        "agent_end",
        r#"{"session_id":"T-abc123","status":"done"}"#
    )
    .progress(),
    HookEventSpec::blocking(
        "permission_ask",
        r#"{"session_id":"T-abc123"}"#,
        AskKind::Permission
    )
    .synchronous(),
];
const PLUGIN_SOURCE: &str = include_str!("plugin.ts");
const AMP_MANAGED_SOURCE: ManagedSource = ManagedSource::new(
    "amp",
    PLUGIN_SOURCE,
    AMP_HOOKS,
    "plugin",
    amp_plugin_path,
    false,
);

#[derive(Clone, Debug, Default)]
pub struct AmpAdapter;

impl crate::agents::capabilities::CoreCapability for AmpAdapter {
    fn spec(&self) -> &'static AgentSpec {
        &AMP_DESCRIPTOR
    }

    #[cfg(test)]
    fn conformance(&self) -> super::AdapterConformance {
        super::AdapterConformance {
            classification: super::hook_types::catalog_classification_corpus(AMP_HOOKS),
            spend: Some(super::SpendFixture {
                session_id: "T-conformance",
                file_name: "T-conformance.json",
                body: super::SpendFixtureBody::Jsonl(
                    r#"{"id":"T-conformance","messages":[{"role":"assistant","messageId":"m1","content":"done","usage":{"timestamp":"2026-01-01T00:00:00Z","model":"gpt-5","inputTokens":100,"outputTokens":20}}]}"#,
                ),
            }),
            ..super::AdapterConformance::default()
        }
    }
}

impl crate::agents::capabilities::HookCapability for AmpAdapter {
    fn decode_hook(&self, event_name: &str, payload: &Value) -> Result<HookOutput> {
        let ask_kind = (event_name == "permission_ask").then_some(AskKind::Permission);
        let mut decoded = decode_catalog_hook(AMP_HOOKS, event_name, ask_kind);
        let parsed = payloads::parse_payload(payload);
        let session_id = parsed
            .session_id
            .as_deref()
            .map(str::trim)
            .filter(|session_id| !session_id.is_empty());
        decoded.set_routing(HookRouting::session(session_id.map(Into::into)));
        decoded.set_final_message(
            (event_name == "agent_end")
                .then_some(parsed.last_assistant_message.as_deref())
                .flatten()
                .and_then(non_empty_trimmed),
        );

        let Some(session_id) = session_id else {
            return Ok(decoded);
        };
        let signal = match event_name {
            "session_start" => LifecycleSignal::Registered,
            "agent_start" => LifecycleSignal::TurnStarted,
            "tool_result" => LifecycleSignal::ToolUsed {
                mutates: true,
                edits: parsed
                    .files_modified
                    .unwrap_or_else(|| self.spec().tool_edits_files(payload)),
                name: None,
                native_key: None,
            },
            "agent_end" => LifecycleSignal::TurnEnded {
                errored: parsed.status.as_deref() != Some("done"),
                parked_on_background: false,
            },
            "permission_ask" => LifecycleSignal::AwaitingInput {
                kind: AskKind::Permission,
                ask_id: None,
                detail: None,
                native_key: None,
            },
            _ => return Ok(decoded),
        };
        let mut observation =
            AgentLifecycleObservation::new(Some(AgentSessionId::from(session_id)), signal)
                .with_worktree_from_payload(payload);
        let prompt = sanitize_user_prompt(parsed.prompt.as_deref());
        observation.task = prompt.clone();
        observation.prompt = prompt;
        observation.launch.model = parsed.model;
        observation.launch.effort = parsed.effort;
        stamp_transcript_path(&mut observation, session_id, &spend::data_root());
        if event_name == "session_start" {
            observation.origin = Some(SessionOrigin::Fresh);
        }
        decoded.attach_lifecycle(observation);
        Ok(decoded)
    }
}

impl crate::agents::capabilities::InstallationCapability for AmpAdapter {
    fn managed_integration(&self) -> Option<&'static dyn super::ManagedIntegration> {
        Some(&AMP_MANAGED_SOURCE)
    }
}

impl crate::agents::capabilities::LaunchCapability for AmpAdapter {
    fn parse_version(&self, stdout: &str, stderr: &str) -> Option<String> {
        parse_amp_version(stdout).or_else(|| parse_amp_version(stderr))
    }
}

impl crate::agents::capabilities::TranscriptCapability for AmpAdapter {
    fn read_transcript_messages(
        &self,
        path: &Path,
        session_id: Option<&AgentSessionId>,
    ) -> std::io::Result<Vec<crate::agents::TranscriptMessage>> {
        transcript::read_messages(path, session_id)
    }

    fn transcript_position(
        &self,
        path: &Path,
        session_id: Option<&AgentSessionId>,
    ) -> Option<crate::agents::TranscriptPosition> {
        transcript::position(path, session_id)
    }

    fn read_assistant_transcript_page(
        &self,
        path: &Path,
        session_id: Option<&AgentSessionId>,
        position: crate::agents::TranscriptPosition,
    ) -> Option<crate::agents::TranscriptPage> {
        transcript::read_assistant_page(path, session_id, position)
    }
}

impl crate::agents::capabilities::ContextCapability for AmpAdapter {
    fn local_context_refresh(
        &self,
        trigger: RefreshTrigger<'_>,
        ctx: &LocalContextRefreshCtx<'_>,
    ) -> Option<LocalContextRefresh> {
        if let RefreshTrigger::Hook(event_name) = trigger
            && !matches!(event_name, "session_start" | "agent_end")
        {
            return None;
        }
        let path =
            spend::resolve_session_file(ctx.agent_id, ctx.prior_transcript_path.map(Path::new))?;
        let stat = TranscriptStat::from_path(&path)?;
        if ctx.prior_transcript_stat == Some(&stat) {
            return None;
        }
        let parsed = thread::AmpThread::read(&path).ok()?;
        if parsed.id != ctx.agent_id {
            return None;
        }
        let latest = parsed.usage.iter().max_by_key(|usage| usage.at);
        let model_id = latest
            .map(|usage| usage.model.clone())
            .or_else(|| ctx.model_hint.map(ToOwned::to_owned));
        let tokens = latest.map(|usage| AgentTokenUsage {
            context_window_size: None,
            used_percentage: None,
            remaining_percentage: None,
            current_context_tokens: None,
            current_usage: Some(AgentCurrentUsage {
                input_tokens: Some(usage.input),
                output_tokens: Some(usage.output),
                cache_creation_input_tokens: Some(usage.cache_write),
                cache_read_input_tokens: Some(usage.cache_read),
            }),
            session_usage: None,
        });
        let prices = super::pricing::cached_book(ctx.shared_pricing_cache_path);
        let (entries, _) = spend::entries_from_thread(&parsed, &prices);
        Some(LocalContextRefresh {
            context: LocalContextPatch {
                model_id: model_id.map_or(FieldPatch::Keep, FieldPatch::Set),
                tokens: LocalTokenPatch::PreserveEstablished(tokens),
                cost: crate::agents::spending::session_cost_from_entries(&entries, ctx.agent_id)
                    .map_or(FieldPatch::Keep, FieldPatch::Set),
                ..LocalContextPatch::authoritative_current()
            },
            transcript_path: Some(path.to_string_lossy().into_owned()),
            transcript_stat: Some(stat),
            ..LocalContextRefresh::authoritative_current()
        })
    }
}

impl crate::agents::capabilities::AccountCapability for AmpAdapter {
    fn probe_account(&self) -> crate::agents::account::AccountProbe {
        account::probe()
    }
}

impl crate::agents::capabilities::SpendingCapability for AmpAdapter {
    fn spending_sources(&self) -> Vec<crate::agents::spending::SpendingSource> {
        crate::agents::spending::SpendingSource::tree(
            spend::data_root().join("threads"),
            "T-?*.json",
        )
    }

    fn session_transcript(&self, session_id: &str, prior_path: Option<&Path>) -> Option<PathBuf> {
        spend::resolve_session_file(session_id, prior_path)
    }

    fn parse_spend(
        &self,
        path: &Path,
        _resume: Option<&super::spending::SpendCursor>,
        prices: &super::PriceBook,
    ) -> super::spending::SpendParse {
        spend::parse(path, prices)
    }
}

fn parse_amp_version(output: &str) -> Option<String> {
    let line = output.lines().find(|line| !line.trim().is_empty())?.trim();
    let (token, annotation) = line.split_once(' ')?;
    if !annotation.starts_with("(released ") || !annotation.ends_with(')') {
        return None;
    }
    let (base, hash) = token.split_once("-g")?;
    let base = base.parse::<super::version::CliVersion>().ok()?;
    (!hash.is_empty() && hash.chars().all(|ch| ch.is_ascii_hexdigit()))
        .then(|| format!("{base}-g{hash}"))
}

fn amp_plugin_path() -> Result<PathBuf> {
    if let Some(raw) = std::env::var_os("RIMZ_AMP_PLUGIN").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(raw));
    }
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .map(|home| home.join(".config"))
        })
        .ok_or_else(|| AgentErr::Install {
            agent: "amp",
            reason: "$HOME is not set; cannot resolve ~/.config/amp/plugins/rimz.ts".to_owned(),
        })?;
    Ok(config_home.join("amp/plugins/rimz.ts"))
}

fn stamp_transcript_path(
    observation: &mut AgentLifecycleObservation,
    session_id: &str,
    data_root: &Path,
) {
    observation.transcript_path = spend::resolve_session_file_at(data_root, session_id)
        .map(|path| path.to_string_lossy().into_owned());
}

// Capabilities this agent has no behavior for; every method keeps its
// default from `agents::capabilities`.
impl crate::agents::capabilities::RuntimeControlCapability for AmpAdapter {}
impl crate::agents::capabilities::SessionCapability for AmpAdapter {}

#[cfg(test)]
mod tests;
