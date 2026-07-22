//! Cursor CLI hook adapter.
//!
//! Cursor's native hooks expose session, turn, tool, child, exit, and
//! compaction-open signals. Version-pinned local chat readers derive pane-only
//! `AskQuestion` and plan-approval waits plus child lifecycle; permission,
//! structured answers, machine-readable spend, and post-compaction events
//! remain explicit gaps.

mod account;
mod install;
mod payloads;
mod session;
mod statusline;
mod transcript;

pub(crate) use crate::agents::capabilities::*;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
#[cfg(test)]
use serde_json::json as test_json;
use serde_json::{Value, json};

use super::definition::{
    AgentSpec, BinIdentity, Brand, Capabilities, CapabilityLevel, ConcernCoverage,
    CoverageAnnotations, HookCoverage, LifecycleAnnotations, PlanLabel, RealtimeUsageChannel,
    RemoteControlCapability, ThreadKey, ToolClassification, UserCoverage,
};
use super::hook_types::{HookEventSpec, catalog_contains, decode_catalog_hook};
use super::lifecycle::LifecycleSignal;
use super::{
    AgentLifecycleObservation, HookOutput, HookRouting, LocalSessionObservation,
    LocallyPricedTurnCost, PriceBook, Result, SubagentIdentity, TokenSplit, locate_binary,
    non_empty_trimmed, resolve_subagent_identity, sanitize_user_prompt,
};
#[cfg(test)]
use crate::harness::run::PermissionMode;
use crate::ids::AgentSessionId;

const UNAMBIGUOUS_FALLBACK_BIN: &str = "cursor-agent";

static CURSOR_DESCRIPTOR: AgentSpec = AgentSpec {
    kind: "cursor",
    aliases: &[],
    display_name: "Cursor",
    brand: Brand {
        emblem: None,
        color: 255,
        color_rgb: (0xe8, 0xe8, 0xe8),
    },
    plan_label: PlanLabel::Prefixed { prefix: "Cursor" },
    sub_providers: &[],
    expected_windows: &[],
    tools: ToolClassification {
        mutating: &["Shell", "Write", "Delete"],
        editing: &["Write", "Delete"],
        blocking: &[],
    },
    capabilities: Capabilities {
        native_ask_ui: true,
        transcript_tail_context: true,
        registers_lazily: false,
        local_session_discovery: true,
        daemon_hooked_sessions: false,
        direct_account_usage: false,
        // `/clear` skips lifecycle hooks; its next `beforeSubmitPrompt` introduces a
        // new conversation in this process and pane. Cursor has no fork surface,
        // and derived subagents carry parent linkage and never compete for the pane.
        same_pane_session: super::SamePaneSessionPolicy::FollowLatest,
        realtime_usage: RealtimeUsageChannel {
            windows_defer_to_fresh_realtime: false,
        },
        remote_control: RemoteControlCapability {
            pane_sessions: false,
            background_sessions: false,
        },
    },
    coverage: CURSOR_COVERAGE,
    user_coverage: CURSOR_USER_COVERAGE,
    lifecycle_hooks: CURSOR_LIFECYCLE_HOOKS,
    default_context_window: None,
    default_model: None,
    process_names: &["cursor-agent", "agent"],
    bin_names: &["cursor-agent", "agent"],
    // Cursor's generic `agent` name collides with the alias Grok's installer
    // symlinks onto `$PATH`, so a match on it is confirmed by Cursor's own
    // version banner before discovery accepts it.
    bin_identity: Some(BinIdentity {
        ambiguous: &["agent"],
        verify: agent_binary_is_cursor,
    }),
    extra_bin_dirs: &[],
    thread_key: ThreadKey::PerFile,
    launch: super::LaunchSpec {
        program: Some("agent"),
        fixed_args: &[],
        prompt: super::PromptStyle::PositionalAfterDoubleDash,
        resume: None,
        fork: None,
        permission: super::LaunchPermissionArgs {
            ask: &[],
            auto: &["--auto-review"],
            yolo: &["--force", "--sandbox", "disabled"],
            plan: &["--mode=plan"],
        },
        max_turn_flag: None,
        compact_command: Some("/summarize"),
        presets: super::PresetMatchers {
            model: Some(super::StaticPresetMatcher::Flag(&["--model"])),
            ..super::PresetMatchers::EMPTY
        },
    },
};

const CURSOR_COVERAGE: CoverageAnnotations = CoverageAnnotations {
    turn_lifecycle: ConcernCoverage::Wired {
        via: "sessionStart/beforeSubmitPrompt/stop including native interruption",
    },
    permission: ConcernCoverage::Unsupported {
        reason: "no local permission hook; ACP-only",
    },
    plan_approval: ConcernCoverage::Partial {
        via: "version-pinned local plan-proposal projection",
        gap: "pane-only wait; no durable RimZ ask or safe answer API",
    },
    user_question: ConcernCoverage::Partial {
        via: "version-pinned local pending AskQuestion projection",
        gap: "pane-only wait; no durable RimZ ask or safe answer API",
    },
    answer: ConcernCoverage::Unsupported {
        reason: "no safe native reply API; answer in Cursor's pane",
    },
    compaction: ConcernCoverage::Partial {
        via: "preCompact opens; the next lifecycle signal closes the bracket",
        gap: "no post-compaction event; landing status and phase held",
    },
    subagents: ConcernCoverage::Partial {
        via: "chats-store subagentInfo and child transcript derivation at parent-hook cadence; native subagentStart/subagentStop mapping retained",
        gap: "the installed CLI never issues subagent hook requests, so children land on the next parent hook, often only at turn end",
    },
    background_parking: ConcernCoverage::Unsupported {
        reason: "no background-task parking signal",
    },
    session_end: ConcernCoverage::Wired { via: "sessionEnd" },
    idle_notification: ConcernCoverage::Partial {
        via: "turn boundaries + stall window",
        gap: "no idle Notification hook",
    },
    context_usage: ConcernCoverage::Wired {
        via: "statusline window/fill/token composition; preCompact and stop fallback",
    },
    realtime_cost: ConcernCoverage::Partial {
        via: "model-priced response/stop-hook accumulation",
        gap: "live session only; no historical Cursor usage ledger",
    },
    rich_context: ConcernCoverage::Wired {
        via: "command statusline payload",
    },
    hook_install: ConcernCoverage::Wired {
        via: "managed ~/.cursor/hooks.json + cli-config.json transaction",
    },
    account_spend: ConcernCoverage::Unsupported {
        reason: "identity/plan/version are wired; no durable provider usage history",
    },
    remote_control: ConcernCoverage::Unsupported {
        reason: "no remote-control surface",
    },
};

const CURSOR_USER_COVERAGE: UserCoverage = UserCoverage {
    state: CapabilityLevel::Full {
        note: "the card tracks every turn, including interruption, to session end",
    },
    live: CapabilityLevel::Partial {
        shows: "context fill, token composition, and a priced session total",
        limit: "the price is RimZ's local estimate rather than Cursor billing",
    },
    history: CapabilityLevel::Partial {
        shows: "past conversations read end to end from the local chats store",
        limit: "no usage ledger, so past spend stays out of rimz stats",
    },
    account: CapabilityLevel::Partial {
        shows: "identity, plan, and CLI version",
        limit: "no usage counted against the plan",
    },
    ask: CapabilityLevel::Partial {
        shows: "an open question or plan approval raises Waiting and routes you to the pane",
        limit: "the question stays in Cursor's own UI, so rimz asks stays empty",
    },
    subagents: CapabilityLevel::Partial {
        shows: "children nest under the parent card with their detail",
        limit: "the installed CLI reports them late, often only at parent turn end",
    },
};

const CURSOR_LIFECYCLE_HOOKS: LifecycleAnnotations = LifecycleAnnotations {
    registered: HookCoverage::Native {
        event: "sessionStart",
    },
    turn_started: HookCoverage::Native {
        event: "beforeSubmitPrompt",
    },
    turn_ended: HookCoverage::Native { event: "stop" },
    tool_used: HookCoverage::Native {
        event: "postToolUse",
    },
    awaiting_input: HookCoverage::Derived {
        via: "validated local pending AskQuestion or open plan-proposal state",
        gap: "no native hook; pane-only wait and answer surface",
    },
    subagent_started: HookCoverage::Native {
        event: "subagentStart",
    },
    subagent_stopped: HookCoverage::Native {
        event: "subagentStop",
    },
    compacting: HookCoverage::Native {
        event: "preCompact",
    },
    compaction_ended: HookCoverage::Derived {
        via: "next lifecycle signal closes the bracket in step + display-window expiry",
        gap: "no post-compaction event; landing status and phase held",
    },
    ended: HookCoverage::Native {
        event: "sessionEnd",
    },
    lost: HookCoverage::Derived {
        via: "rimz exec wrapper",
        gap: "native hooks do not report mux-session death",
    },
};

pub(super) const CURSOR_HOOKS: &[HookEventSpec] = &[
    HookEventSpec::lifecycle(
        "sessionStart",
        r#"{"conversation_id":"c1","session_id":"c1","cursor_version":"1.7"}"#
    )
    .progress(),
    HookEventSpec::lifecycle(
        "beforeSubmitPrompt",
        r#"{"conversation_id":"c1","prompt":"fix it","cursor_version":"1.7"}"#
    )
    .progress(),
    HookEventSpec::lifecycle(
        "postToolUse",
        r#"{"conversation_id":"c1","tool_name":"Write","cwd":"/tmp","cursor_version":"1.7"}"#
    )
    .progress(),
    HookEventSpec::lifecycle(
        "postToolUseFailure",
        r#"{"conversation_id":"c1","tool_name":"Shell","failure_type":"error","cursor_version":"1.7"}"#
    )
    .progress(),
    HookEventSpec::lifecycle(
        "afterAgentResponse",
        r#"{"conversation_id":"c1","text":"done","cursor_version":"1.7"}"#
    )
    .progress(),
    HookEventSpec::lifecycle(
        "stop",
        r#"{"conversation_id":"c1","status":"completed","cursor_version":"1.7"}"#
    )
    .progress(),
    HookEventSpec::lifecycle(
        "sessionEnd",
        r#"{"conversation_id":"c1","reason":"quit","cursor_version":"1.7"}"#
    )
    .session_ended(),
    HookEventSpec::lifecycle(
        "preCompact",
        r#"{"conversation_id":"c1","trigger":"manual","context_usage_percent":83.2,"context_window_size":200000,"cursor_version":"1.7"}"#
    ),
    HookEventSpec::lifecycle(
        "subagentStart",
        r#"{"subagent_id":"child-1","parent_conversation_id":"c1","subagent_type":"generalPurpose","task":"inspect hooks","cursor_version":"1.7"}"#
    )
    .progress(),
    HookEventSpec::lifecycle(
        "subagentStop",
        r#"{"subagent_id":"child-1","parent_conversation_id":"c1","subagent_type":"generalPurpose","status":"completed","cursor_version":"1.7"}"#
    )
    .progress(),
];
pub(super) const RIMZ_HOOK_COMMAND: &str =
    "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source cursor";
pub(super) const RIMZ_HOOK_MARKER: &str = "rimz hooks feed --source cursor";
const RIMZ_STATUS_LINE_COMMAND: &str = "rimz statusline feed --source cursor";
const RIMZ_STATUS_LINE_MARKER: &str = "rimz statusline feed --source cursor";
const STATUS_LINE: super::managed_statusline::ManagedStatusLineSpec =
    super::managed_statusline::ManagedStatusLineSpec {
        key_path: &["statusLine"],
        command: RIMZ_STATUS_LINE_COMMAND,
        command_marker: RIMZ_STATUS_LINE_MARKER,
        rendering_options: super::managed_statusline::RenderingOptions::Only(&[
            "padding",
            "updateIntervalMs",
            "timeoutMs",
        ]),
        wrap_policy: super::managed_statusline::WrapPolicy::Any,
        required_for_install: false,
    };

#[derive(Clone, Debug, Default)]
pub struct CursorAdapter;

fn cursor_project_dir(value: Option<&OsStr>) -> Option<PathBuf> {
    let value = value.filter(|value| !value.is_empty())?;
    let path = PathBuf::from(value);
    path.is_absolute().then_some(path)
}

#[cfg(feature = "testkit")]
#[doc(hidden)]
pub fn discover_local_sessions_under(
    cursor_home: &Path,
    workspaces: &[&Path],
) -> Vec<LocalSessionObservation> {
    workspaces
        .iter()
        .flat_map(|workspace| session::discover_under(cursor_home, workspace))
        .collect()
}

impl crate::agents::capabilities::CoreCapability for CursorAdapter {
    fn spec(&self) -> &'static AgentSpec {
        &CURSOR_DESCRIPTOR
    }

    #[cfg(test)]
    fn conformance(&self) -> super::AdapterConformance {
        super::AdapterConformance {
            classification: super::hook_types::catalog_classification_corpus(CURSOR_HOOKS),
            hook_turn_cost: Some(super::TurnCostFixture {
                event_name: "stop",
                payload: test_json!({
                    "generation_id": "gen-1",
                    "status": "completed",
                    "model_id": "default",
                    "input_tokens": 22_725,
                    "output_tokens": 26,
                    "cache_read_tokens": 8_704,
                    "cache_write_tokens": 0
                }),
            }),
            local_session: Some(session::fixture_observation()),
            ..super::AdapterConformance::default()
        }
    }
}

impl crate::agents::capabilities::TranscriptCapability for CursorAdapter {}

impl crate::agents::capabilities::HookCapability for CursorAdapter {
    fn hook_ingress(&self, pid: Option<u32>) -> super::HookIngressDecision {
        super::HookIngressDecision::Accept(super::HookIngressAcceptance {
            owner: super::HookIngressOwner::agent(pid),
            participant_start: cursor_project_dir(
                std::env::var_os("CURSOR_PROJECT_DIR").as_deref(),
            ),
        })
    }

    fn decode_hook(&self, event_name: &str, payload: &Value) -> Result<HookOutput> {
        let mut decoded = decode_catalog_hook(CURSOR_HOOKS, event_name, None);
        decoded.set_reply(
            catalog_contains(CURSOR_HOOKS, event_name)
                .then(|| json!({}))
                .map_or(super::HookReply::Silent, super::HookReply::Json),
        );
        let parsed = payloads::parse_payload(payload);
        let agent_id = parsed
            .conversation_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(ToOwned::to_owned);
        decoded.set_routing(HookRouting::session(agent_id.map(Into::into)));
        decoded.set_assistant_message(
            (event_name == "afterAgentResponse")
                .then_some(parsed.text.clone())
                .flatten(),
        );
        if parsed.model.is_some()
            || parsed.model_id.is_some()
            || !parsed.model_params.is_empty()
            || parsed.context_usage_percent.is_some()
            || parsed.context_window_size.is_some()
            || parsed.input_tokens.is_some()
            || parsed.output_tokens.is_some()
            || parsed.cache_read_tokens.is_some()
            || parsed.cache_write_tokens.is_some()
        {
            decoded.set_observed_context(self.observe_context(self.spec().kind, payload));
        }
        if matches!(event_name, "subagentStart" | "subagentStop") {
            if let Some(observation) = self.observe_subagent_lifecycle(event_name, payload, parsed)
            {
                decoded.attach_lifecycle(observation);
            }
            return Ok(decoded);
        }
        let turn_usage = (event_name == "stop")
            .then(|| parsed.turn_usage())
            .flatten();
        let signal = match event_name {
            "sessionStart" => LifecycleSignal::Registered,
            "beforeSubmitPrompt" => LifecycleSignal::TurnStarted,
            "postToolUse" if self.spec().tool_mutates(payload) => LifecycleSignal::ToolUsed {
                mutates: true,
                edits: self.spec().tool_edits_files(payload),
                native_key: None,
            },
            "stop" if parsed.stop_outcome() == payloads::StopOutcome::Aborted => {
                LifecycleSignal::TurnInterrupted
            }
            "stop" => LifecycleSignal::TurnEnded {
                errored: parsed.stop_outcome() == payloads::StopOutcome::Error,
                parked_on_background: false,
            },
            "sessionEnd" => LifecycleSignal::Ended,
            "preCompact" => LifecycleSignal::Compacting,
            _ => return Ok(decoded),
        };
        let mut observation =
            AgentLifecycleObservation::new(decoded.event_agent_id().cloned(), signal)
                .with_worktree_from_payload(payload);
        let prompt = sanitize_user_prompt(parsed.prompt.as_deref());
        observation.task = prompt.clone();
        observation.prompt = prompt;
        let effort = parsed.model_param("effort").map(ToOwned::to_owned);
        observation.transcript_path = parsed.transcript_path;
        observation.launch.model = parsed
            .model_id
            .or(parsed.model)
            .map(statusline::normalize_model);
        observation.launch.effort = effort;
        observation.usage.context_pct = parsed
            .context_usage_percent
            .filter(|value| value.is_finite())
            .map(|value| value.round().clamp(0.0, 100.0) as u8);
        observation.usage.context_window = parsed.context_window_size;
        if event_name == "stop" {
            observation.usage.fresh_input_tokens = turn_usage.and_then(|usage| usage.fresh_input);
            observation.usage.output_tokens = turn_usage.and_then(|usage| usage.output);
            observation.usage.cache_read_input_tokens =
                turn_usage.and_then(|usage| usage.cache_read);
            observation.usage.cache_write_input_tokens =
                turn_usage.and_then(|usage| usage.cache_write);
        }
        decoded.attach_lifecycle(observation);
        Ok(decoded)
    }

    fn derive_subagent_observations(&self, workspace: &Path) -> Vec<AgentLifecycleObservation> {
        let Some(home) = session::cursor_home(std::env::var_os("HOME").as_deref()) else {
            return Vec::new();
        };
        self.derive_subagent_observations_under(&home, workspace)
    }
}

impl crate::agents::capabilities::InstallationCapability for CursorAdapter {
    fn managed_integration(&self) -> Option<&'static dyn super::ManagedIntegration> {
        Some(&install::MANAGED_INTEGRATION)
    }

    fn status_line_invocation(&self) -> super::StatusLineInvocation {
        super::StatusLineInvocation::DirectArgv
    }
}

impl crate::agents::capabilities::LaunchCapability for CursorAdapter {
    fn parse_version(&self, stdout: &str, stderr: &str) -> Option<String> {
        parse_cursor_version(stdout).or_else(|| parse_cursor_version(stderr))
    }

    fn resume_command(&self, session_id: &str, _cwd: &Path) -> Option<Vec<String>> {
        let bin = cursor_launch_binary(locate_binary(self.spec()));
        Some(vec![bin, "--resume".to_owned(), session_id.to_owned()])
    }

    fn launch_command(&self, extra_args: &[String], prompt: Option<&str>) -> Option<Vec<String>> {
        let bin = cursor_launch_binary(locate_binary(self.spec()));
        let mut argv = self.spec().launch.launch_command(extra_args, prompt)?;
        argv[0] = bin;
        Some(argv)
    }
}

/// Select the verified Cursor path when discovery found one. The fallback stays
/// provider-unique so a colliding `agent` alias can fail to resolve but can
/// never launch another provider as Cursor.
fn cursor_launch_binary(located: Option<PathBuf>) -> String {
    located.map_or_else(
        || UNAMBIGUOUS_FALLBACK_BIN.to_owned(),
        |path| path.to_string_lossy().into_owned(),
    )
}

impl crate::agents::capabilities::SessionCapability for CursorAdapter {
    #[cfg(feature = "testkit")]
    fn discover_local_sessions_under(
        &self,
        home: &Path,
        workspaces: &[&Path],
    ) -> Vec<LocalSessionObservation> {
        discover_local_sessions_under(home, workspaces)
    }

    fn discover_local_sessions(&self, workspaces: &[&Path]) -> Vec<LocalSessionObservation> {
        session::discover(workspaces)
    }
}

impl crate::agents::capabilities::ContextCapability for CursorAdapter {
    fn observe_context(&self, source: &str, payload: &Value) -> Option<super::ContextObservation> {
        let payload =
            serde_json::from_value::<statusline::StatuslinePayload>(payload.clone()).ok()?;
        let agent_id = payload.session_id.clone()?;
        let markers = payload
            .session_id
            .as_deref()
            .and_then(transcript::statusline_turn_markers);
        let mut context = payload.into_context(source, Timestamp::now());
        if let Some(markers) = markers {
            context.settle = markers.settle;
            context.turn_error = markers.turn_error;
        }
        super::ContextObservation::new(agent_id, context)
    }

    fn price_turn_locally(
        &self,
        event_name: &str,
        payload: &Value,
        prices: &PriceBook,
    ) -> Option<LocallyPricedTurnCost> {
        if !matches!(event_name, "afterAgentResponse" | "stop") {
            return None;
        }
        let parsed = payloads::parse_payload(payload);
        if event_name == "stop"
            && !matches!(
                parsed.status.as_deref(),
                Some("completed" | "aborted" | "error")
            )
        {
            return None;
        }
        let usage = parsed.turn_usage()?;
        let turn_id = parsed.generation_id?.trim().to_owned();
        if turn_id.is_empty() {
            return None;
        }
        let model = parsed.model_id.or(parsed.model)?;
        let model = model.trim();
        if model.is_empty() {
            return None;
        }
        let price_key = if model.eq_ignore_ascii_case("default") {
            "cursor-auto"
        } else {
            model
        };
        let price = prices.price(price_key)?;
        // Each generation id identifies one turn, so one-request pricing applies.
        let cost_usd = price.cost_of(
            TokenSplit::new(usage.fresh_input.unwrap_or(0), usage.output.unwrap_or(0))
                .cached(
                    usage.cache_write.unwrap_or(0),
                    usage.cache_read.unwrap_or(0),
                )
                .fast(model.to_ascii_lowercase().ends_with("-fast")),
        );
        (cost_usd.is_finite() && cost_usd > 0.0)
            .then_some(LocallyPricedTurnCost { turn_id, cost_usd })
    }

    fn local_context_refresh(
        &self,
        trigger: super::RefreshTrigger<'_>,
        ctx: &super::LocalContextRefreshCtx<'_>,
    ) -> Option<super::LocalContextRefresh> {
        if let super::RefreshTrigger::Hook(event) = trigger
            && !matches!(
                event,
                "sessionStart"
                    | "beforeSubmitPrompt"
                    | "postToolUse"
                    | "postToolUseFailure"
                    | "afterAgentResponse"
                    | "stop"
                    | "preCompact"
            )
        {
            return None;
        }
        transcript::refresh(ctx)
    }
}

impl crate::agents::capabilities::AccountCapability for CursorAdapter {
    fn probe_account(&self) -> super::account::AccountProbe {
        account::probe(self.spec())
    }
}

impl crate::agents::capabilities::SpendingCapability for CursorAdapter {
    fn session_transcript(&self, session_id: &str, prior_path: Option<&Path>) -> Option<PathBuf> {
        transcript::resolve_transcript(session_id, None, prior_path)
    }
}

/// Whether a binary located under Cursor's ambiguous `agent` name is genuinely
/// Cursor. Cursor's date-build banner (`YYYY.MM.DD-hash`) is unique to Cursor,
/// so a candidate whose `--version` parses under it is Cursor and no other CLI
/// sharing the `agent` filename (Grok's install alias) is.
fn agent_binary_is_cursor(stdout: &str, stderr: &str) -> bool {
    parse_cursor_version(stdout)
        .or_else(|| parse_cursor_version(stderr))
        .is_some()
}

fn parse_cursor_version(output: &str) -> Option<String> {
    let token = output
        .lines()
        .find(|line| !line.trim().is_empty())?
        .split_whitespace()
        .next()?;
    let (date, hash) = token.split_once('-')?;
    let mut parts = date.split('.');
    let (year, month, day) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some()
        || year.len() != 4
        || month.len() != 2
        || day.len() != 2
        || ![year, month, day]
            .into_iter()
            .all(|part| part.chars().all(|ch| ch.is_ascii_digit()))
        || hash.is_empty()
        || !hash.chars().all(|ch| ch.is_ascii_hexdigit())
    {
        return None;
    }
    Some(token.to_owned())
}

impl CursorAdapter {
    fn derive_subagent_observations_under(
        &self,
        home: &Path,
        workspace: &Path,
    ) -> Vec<AgentLifecycleObservation> {
        session::discover_subagent_chats(home, workspace)
            .into_iter()
            .flat_map(|record| {
                let mut observations = vec![Self::mapped_subagent_observation(
                    record.child_id.clone(),
                    record.parent_agent_id.clone(),
                    LifecycleSignal::SubagentStarted,
                    record.type_name.clone(),
                    record.task.clone(),
                )];
                if let Some(terminal) = record.terminal {
                    let mut stopped = Self::mapped_subagent_observation(
                        record.child_id,
                        record.parent_agent_id,
                        LifecycleSignal::SubagentStopped {
                            errored: terminal.errored,
                        },
                        record.type_name,
                        record.task,
                    );
                    stopped.transcript_path = record
                        .transcript_path
                        .map(|path| path.to_string_lossy().into_owned());
                    observations.push(stopped);
                }
                observations
            })
            .collect()
    }
    fn observe_subagent_lifecycle(
        &self,
        event_name: &str,
        payload: &Value,
        parsed: payloads::CursorHookPayload,
    ) -> Option<AgentLifecycleObservation> {
        let signal = match event_name {
            "subagentStart" => LifecycleSignal::SubagentStarted,
            "subagentStop" => LifecycleSignal::SubagentStopped {
                errored: parsed.stop_outcome() == payloads::StopOutcome::Error,
            },
            _ => return None,
        };
        let (agent_id, parent_agent_id) = match resolve_subagent_identity(
            self.spec().kind,
            event_name,
            parsed.subagent_id.as_deref(),
            parsed.parent_conversation_id.as_deref(),
            payload,
        ) {
            SubagentIdentity::Resolved {
                agent_id,
                parent_agent_id,
            } => (agent_id, parent_agent_id),
            SubagentIdentity::Quarantined => return None,
        };

        let subagent_type = parsed.subagent_type.as_deref().and_then(non_empty_trimmed);
        let task = sanitize_user_prompt(parsed.task.as_deref()).or_else(|| {
            (event_name == "subagentStop")
                .then(|| sanitize_user_prompt(parsed.description.as_deref()))
                .flatten()
        });
        let mut observation = Self::mapped_subagent_observation(
            agent_id,
            parent_agent_id,
            signal,
            subagent_type,
            task,
        )
        .with_worktree_from_payload(payload);
        observation.worktree_branch = parsed.git_branch;
        if event_name == "subagentStart" {
            observation.launch.model = parsed.subagent_model.map(statusline::normalize_model);
        } else {
            observation.transcript_path = parsed.agent_transcript_path;
        }
        Some(observation)
    }

    fn mapped_subagent_observation(
        agent_id: AgentSessionId,
        parent_agent_id: AgentSessionId,
        signal: LifecycleSignal,
        type_name: Option<String>,
        task: Option<String>,
    ) -> AgentLifecycleObservation {
        let mut observation = AgentLifecycleObservation::new(Some(agent_id), signal);
        observation.parent_agent_id = Some(parent_agent_id);
        observation.agent_name = type_name.clone();
        observation.launch.role = type_name;
        observation.task = task;
        observation
    }
}

// Capabilities this agent has no behavior for; every method keeps its
// default from `agents::capabilities`.
impl crate::agents::capabilities::RuntimeControlCapability for CursorAdapter {}

#[cfg(test)]
mod tests;
