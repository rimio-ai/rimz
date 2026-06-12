//! Pi hook adapter.
//!
//! Pi's integration surface is in-process TypeScript extensions, so the
//! adapter ships one — [`extension.ts`](./extension.ts), embedded at compile
//! time and installed whole-file to `~/.pi/agent/extensions/rimz.ts`. The
//! extension forwards pi's lifecycle events to `rimz hooks feed --source pi`
//! as fire-and-forget children, inverting the Claude/Codex child direction
//! (pi runs Rimz, not the other way around); the wire it posts is the typed
//! shape in [`payloads`], with the model, effort, and context gauge
//! (`context_pct` / `context_window` / `total_tokens`) stamped on every
//! envelope from the in-process `ctx.getContextUsage()` — payload-first, so
//! the sidebar's bar stays current with no transcript tail read here.
//! Lifecycle maps per docs/internals/agents/adapter/pi.md: `session_start`
//! registers, `before_agent_start` starts the
//! turn with the prompt, `agent_end` ends it carrying the in-band error bit,
//! `tool_execution_end` is the mutating-tool heartbeat, and
//! `session_before_compact`/`session_compact`/`session_shutdown` are the
//! compaction and exit signals. Spend stays in [`spend`].
//!
//! One wired event blocks: `tool_call`, pi's pre-tool gate, whose extension
//! handler pi awaits. It classifies as a permission ask so a fresh enrolled
//! resolver can allow or deny the tool; the decision is pi's own
//! `ToolCallEventResult` — deny renders `{"block": true, "reason": …}`,
//! allow renders `{}`. Pi draws no permission prompt of its own
//! (`native_ask_ui: false`), so an ask nothing answers resolves neutrally —
//! empty stdout lets the tool run, and no `native_ui` feed item is pushed:
//! gating is opt-in via a resolver, never Rimz posing questions pi would not
//! have asked. Subagents, background tasks, and rate-limit windows stay
//! declared off (`docs/internals/adapter/pi-reference.md` → "What Pi cannot
//! support") and the absences render deliberately.

pub(crate) mod account;
pub(crate) mod payloads;
pub(crate) mod spend;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, PlanLabel, RemoteControlCapability, ThreadKey,
    ToolClassification,
};
use super::lifecycle::LifecycleSignal;
use super::observation::payload_context_pct;
use super::pricing::PriceBook;
use super::{
    AgentAdapter, AgentErr, AgentLifecycleObservation, ClassifiedHook, HookInstallPreview,
    HookInstallReport, HookUninstallReport, Result, agent_config_path, choice_is_allow,
    classify_agent_hook, optional_payload_string, read_optional_file, sanitize_user_prompt,
};
use crate::feed::{FeedItem, FeedKind, Resolution};
use crate::ids::AgentSessionId;
use crate::ledger::atomic;

/// Everything `const` about Pi, in one place. See [`AgentDescriptor`] for the
/// descriptor-vs-trait split.
static PI_DESCRIPTOR: AgentDescriptor = AgentDescriptor {
    kind: "pi",
    display_name: "Pi",
    brand: Brand {
        emblem: "
 █▜███▛█
▝▜▛▀▀▀▜▛▘
 ▝▘   ▝▘",
        color: 29,
        color_rgb: (0x27, 0xa0, 0x77),
    },
    // Pi sessions span whatever provider account the user wired, so no single
    // brand prefix is honest — the tier renders bare.
    plan_label: PlanLabel::TitleCaseOnly,
    // Pi is the multi-provider client: it runs *on* other providers'
    // subscriptions rather than metering one of its own.
    sub_providers: &[],
    // Pi's built-in tool set: `edit`/`write` edit files; `bash` mutates
    // without editing, so the reasoning phase survives it.
    tools: ToolClassification {
        mutating: &["bash", "edit", "write"],
        editing: &["edit", "write"],
    },
    capabilities: Capabilities {
        // `tool_call` is pi's awaited pre-tool gate: the extension handler
        // holds the tool until the bridge answers, so an enrolled resolver
        // can allow or deny it.
        blocking_feed: true,
        // Pi never asks natively — no permission prompts, plan approvals, or
        // questions — so an ask no resolver answers has no pi surface to
        // route the human to. It resolves neutrally (the tool runs) with no
        // `native_ui` feed item: gating is opt-in via a resolver, never Rimz
        // posing a question pi would not have asked.
        native_ask_ui: false,
        rate_limit_windows: false,
        subagents: false,
        background_tasks: false,
        registers_lazily: false,
        hook_install: true,
        remote_control: RemoteControlCapability {
            pane_sessions: false,
            background_sessions: false,
        },
    },
    default_context_window: None,
    default_model: None,
    // Pi awaits the `tool_call` handler with no kill window of its own, so
    // the cap is purely Rimz's bridge ceiling — matched to Claude's so a
    // resolver chain budgets identically across agents.
    hook_cap: Duration::from_secs(120),
    process_names: &["pi"],
    // Pi's progress-proving events, in its own wire vocabulary. The blocking
    // `tool_call` is excluded like Claude's `PreToolUse`: it fires while the
    // ask is being created, so touching on it would instantly un-block the
    // row. Every *completed* tool still touches via `tool_execution_end`.
    activity_events: &[
        "session_start",
        "before_agent_start",
        "agent_end",
        "tool_execution_end",
    ],
    hook_install_unavailable: None,
    thread_key: ThreadKey::PerFile,
};

/// The non-blocking events the embedded extension forwards — the lifecycle
/// channel, the single source of truth for classification. Mirrors the
/// `pi.on(...)` registrations in [`extension.ts`](./extension.ts) (asserted
/// by test).
const LIFECYCLE_EVENTS: &[&str] = &[
    "session_start",
    "before_agent_start",
    "agent_end",
    "tool_execution_end",
    "session_before_compact",
    "session_compact",
    "session_shutdown",
];

/// Everything the extension wires, for the install/uninstall reports: the
/// lifecycle set plus the blocking `tool_call` gate.
const WIRED_EVENTS: &[&str] = &[
    "session_start",
    "before_agent_start",
    "agent_end",
    "tool_execution_end",
    "session_before_compact",
    "session_compact",
    "session_shutdown",
    "tool_call",
];

/// The Rimz pi extension, embedded at compile time and written whole-file on
/// install. Carries [`RIMZ_MANAGED_MARKER`] on its first line.
const EXTENSION_SOURCE: &str = include_str!("extension.ts");

/// Ownership marker on the extension's first line. Install reclaims a marked
/// file (Rimz wrote it) and refuses an unmarked one (the user wrote it);
/// uninstall removes only a marked file.
const RIMZ_MANAGED_MARKER: &str = "_rimz_managed";

#[derive(Clone, Debug, Default)]
pub struct PiAdapter;

impl AgentAdapter for PiAdapter {
    fn descriptor(&self) -> &'static AgentDescriptor {
        &PI_DESCRIPTOR
    }

    fn classify_hook(&self, event_name: &str, _payload: &Value) -> ClassifiedHook {
        // `tool_call` is pi's only blocking gate — every tool routes through
        // it, so it classifies as a permission ask. Everything else rides the
        // lifecycle channel.
        let feed_kind = (event_name == "tool_call").then_some(FeedKind::Permission);
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
                "tool_call",
                json!({ "session_id": "sess-1", "tool_name": "bash" }),
                AgentHookClass::BlockingFeed,
                Some(FeedKind::Permission),
            ),
            ClassificationSample::new(
                "session_start",
                json!({ "session_id": "sess-1" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "before_agent_start",
                json!({ "session_id": "sess-1", "prompt": "fix auth" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "agent_end",
                json!({ "session_id": "sess-1", "stop_reason": "stop" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "tool_execution_end",
                json!({ "session_id": "sess-1", "tool_name": "bash" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "session_before_compact",
                json!({ "session_id": "sess-1" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "session_compact",
                json!({ "session_id": "sess-1" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "session_shutdown",
                json!({ "session_id": "sess-1" }),
                AgentHookClass::Lifecycle,
                None,
            ),
        ]
    }

    fn render_decision(&self, item: &FeedItem, resolution: &Resolution) -> Result<Value> {
        match item.kind {
            FeedKind::Permission => {
                if choice_is_allow(resolution) {
                    // Pi mutates tool arguments only in-process (the extension
                    // handler edits `event.input`); the bridge cannot reach
                    // that, so an `updatedInput` riding the resolution is
                    // ignored and a plain allow renders.
                    Ok(json!({}))
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
                    Ok(json!({ "block": true, "reason": reason }))
                }
            }
            other => Err(AgentErr::Render {
                agent: "pi",
                reason: format!("unsupported feed kind {other:?}"),
            }),
        }
    }

    fn render_neutral(&self, _event_name: &str) -> Result<Option<Value>> {
        // Empty stdout is the extension's allow: pi has no native prompt to
        // fall back to, so "no answer" must let the tool run. This is the
        // neutral the no-resolver and bridge-timeout paths print.
        Ok(None)
    }

    fn observe_lifecycle(
        &self,
        event_name: &str,
        payload: &Value,
    ) -> Option<AgentLifecycleObservation> {
        let parsed = payloads::parse_payload(payload);
        // The status decision lives in the shared `lifecycle::step` table —
        // here the adapter only names the intent. The native-event → signal
        // mapping is docs/internals/agents/adapter/pi.md.
        let signal = match event_name {
            "session_start" => LifecycleSignal::Registered,
            // Pi's `agent_start`/`agent_end` bracket one user prompt — pi's
            // `agent_*` pair is what Rimz calls a turn. `before_agent_start`
            // carries the prompt.
            "before_agent_start" => LifecycleSignal::TurnStarted,
            // The last assistant message is the in-band death certificate:
            // `stopReason: "error" | "aborted"` plus `errorMessage`, no
            // transcript forensics needed. Pi has no background-task parking.
            "agent_end" => LifecycleSignal::TurnEnded {
                errored: payloads::agent_end_errored(&parsed),
                parked_on_background: false,
            },
            // Only a *mutating* tool rides the lifecycle channel: it is proof
            // of real work (read-only tools stay silent). The `edits` bit
            // marks the file-writing subset, which ends the turn's thinking
            // head.
            "tool_execution_end" if self.descriptor().tool_mutates(payload) => {
                LifecycleSignal::ToolUsed {
                    mutates: true,
                    edits: self.descriptor().tool_edits_files(payload),
                }
            }
            // A leading signal, like Claude's `PreCompact`.
            "session_before_compact" => LifecycleSignal::Compacting,
            // Pi's extension hook reports no manual/auto trigger, so this
            // only clears the transient head and preserves the prior state.
            "session_compact" => LifecycleSignal::CompactionEnded { auto: None },
            // Fires on quit including Ctrl+C/SIGHUP/SIGTERM and on every
            // session replacement (`/new`, `/resume`) — a true session end.
            "session_shutdown" => LifecycleSignal::Ended,
            _ => return None,
        };
        // No subagents: every pi event keys on its own session id, no parent
        // link, no quarantine path.
        let agent_id = optional_payload_string(payload, &["session_id"]).map(AgentSessionId::from);
        let mut observation =
            AgentLifecycleObservation::new(agent_id, signal).with_worktree_from_payload(payload);
        // A pi row labels with the user's *sanitized* prompt, so harness
        // control text never reaches the row; absent fields are carry-forward.
        observation.task = sanitize_user_prompt(parsed.prompt.as_deref());
        observation.prompt = sanitize_user_prompt(parsed.prompt.as_deref());
        observation.model = parsed.model;
        observation.effort = parsed.effort;
        // The gauge is payload-first and payload-only: the extension stamps
        // it on every envelope from the in-process `ctx.getContextUsage()`,
        // so no transcript tail is ever read (the `None` fallback). The one
        // declared absence left is the todo surface — pi has none.
        observation.context_pct = payload_context_pct(payload, None);
        observation.context_window = parsed.context_window;
        observation.total_tokens = parsed.total_tokens;
        Some(observation)
    }

    fn ends_session(&self, event_name: &str) -> bool {
        // The extension skips the `/reload` shutdown (the same session id
        // re-registers in place, and a fire-and-forget tombstone would race
        // it), so every shutdown that arrives here is a real end: quit, or a
        // new/resume/fork replacing this session.
        event_name == "session_shutdown"
    }

    fn moves_on(&self, event_name: &str) -> bool {
        // A new prompt starts a fresh turn; an agent_end completes the current
        // one. Pi raises no native asks today, so this only future-proofs the
        // native_ui expiry against enrichment-sourced asks.
        matches!(event_name, "before_agent_start" | "agent_end")
    }

    fn transcript_files(&self) -> Vec<PathBuf> {
        spend::pi_session_files()
    }

    /// Pi logs `costUSD` directly, so the price book is unused. The resume
    /// cursor carries the session header cwd so appended usage entries retain
    /// their workspace origin.
    fn parse_spend(
        &self,
        path: &Path,
        resume: Option<&crate::agents::spending::SpendCursor>,
        _prices: &PriceBook,
    ) -> crate::agents::spending::SpendParse {
        spend::parse_pi_spend(path, resume)
    }

    /// `pi --session <id>` resolves the session (a partial UUID suffices) and
    /// restores it interactively; the launching pane sets the cwd. The
    /// extension re-fires `session_start` with `reason: "resume"`.
    fn resume_command(&self, session_id: &str, _cwd: &Path) -> Option<Vec<String>> {
        Some(vec![
            "pi".to_owned(),
            "--session".to_owned(),
            session_id.to_owned(),
        ])
    }

    fn launch_command(&self, extra_args: &[String], prompt: Option<&str>) -> Option<Vec<String>> {
        let mut argv = vec!["pi".to_owned()];
        argv.extend(extra_args.iter().cloned());
        if let Some(prompt) = prompt.filter(|value| !value.is_empty()) {
            argv.push(prompt.to_owned());
        }
        Some(argv)
    }

    fn install_hooks(&self) -> Result<HookInstallReport> {
        let path = pi_extension_path()?;
        install_into(&path)
    }

    fn preview_hook_install(&self) -> Result<HookInstallPreview> {
        let path = pi_extension_path()?;
        preview_install_at(&path)
    }

    fn uninstall_hooks(&self) -> Result<HookUninstallReport> {
        let path = pi_extension_path()?;
        uninstall_from(&path)
    }

    fn hooks_installed(&self) -> bool {
        pi_extension_path().is_ok_and(|path| hooks_installed_at(&path))
    }

    fn managed_hook_artifacts_present(&self) -> bool {
        self.hooks_installed()
    }

    fn probe_account(&self) -> crate::agents::account::AccountProbe {
        account::probe()
    }
}

fn pi_extension_path() -> Result<PathBuf> {
    // Honour an explicit override (`RIMZ_PI_EXTENSION`) so tests and tooling
    // can point the installer at a tempdir without touching real config. Pi
    // auto-discovers `*.ts`/`*.js` under this directory; install is
    // deliberately user-global — never pi's *project-local* discovery dir
    // (`<project>/.pi/extensions/`, a different path) — so the project trust
    // hash is untouched.
    agent_config_path(
        "pi",
        "RIMZ_PI_EXTENSION",
        Path::new(".pi/agent/extensions/rimz.ts"),
    )
}

/// Whether the on-disk extension is Rimz-owned: the ownership marker rides
/// the first line of every build of [`EXTENSION_SOURCE`].
fn file_is_rimz_managed(content: &str) -> bool {
    content
        .lines()
        .next()
        .is_some_and(|line| line.contains(RIMZ_MANAGED_MARKER))
}

/// Refuse to clobber a user-authored `rimz.ts`. One shared guard so install
/// and preview agree on what is reclaimable.
fn refuse_unmarked(path: &Path, original: Option<&str>) -> Result<()> {
    match original {
        Some(existing) if !file_is_rimz_managed(existing) => Err(AgentErr::Install {
            agent: "pi",
            reason: format!(
                "refusing to overwrite an unmarked user extension at {}; move it aside or remove it to let Rimz manage this file",
                path.display()
            ),
        }),
        _ => Ok(()),
    }
}

/// Install is whole-file ownership: pi has no config to merge into, so the
/// embedded source overwrites the path verbatim — idempotent by construction.
/// A marked file (Rimz wrote it, however edited since) is reclaimed
/// byte-for-byte; an unmarked file is the user's own extension and refuses.
fn install_into(path: &Path) -> Result<HookInstallReport> {
    let original = read_optional_file("pi", path)?;
    refuse_unmarked(path, original.as_deref())?;
    atomic::write_bytes_atomically(path, EXTENSION_SOURCE.as_bytes())?;
    Ok(HookInstallReport {
        agent: "pi",
        config_path: path.to_path_buf(),
        installed_events: installed_event_names(),
        merged: original.is_some(),
    })
}

fn preview_install_at(path: &Path) -> Result<HookInstallPreview> {
    let original = read_optional_file("pi", path)?;
    // Mirror install's refusal so the consent gate surfaces the conflict
    // before a doomed install, not after.
    refuse_unmarked(path, original.as_deref())?;
    Ok(HookInstallPreview {
        agent: "pi",
        config_path: path.to_path_buf(),
        planned_events: installed_event_names(),
        merged: original.is_some(),
        original_config: original,
        candidate_config: EXTENSION_SOURCE.to_owned(),
        // Pi manages no statusline; the gauge rides the hook envelope.
        status_line_change: None,
        subagent_status_line_change: None,
    })
}

fn uninstall_from(path: &Path) -> Result<HookUninstallReport> {
    let original = read_optional_file("pi", path)?;
    let existed = original.is_some();
    let mut removed_events = Vec::new();
    if original.as_deref().is_some_and(file_is_rimz_managed) {
        std::fs::remove_file(path).map_err(|source| AgentErr::InstallIo {
            agent: "pi",
            path: path.to_path_buf(),
            source,
        })?;
        removed_events = installed_event_names();
    }
    // An unmarked `rimz.ts` is user-owned: left in place, nothing removed.
    Ok(HookUninstallReport {
        agent: "pi",
        config_path: path.to_path_buf(),
        removed_events,
        existed,
    })
}

/// Best-effort like the other adapters: a missing or unreadable file reads as
/// "not installed". The first-line marker distinguishes the Rimz-owned
/// extension from a user's own file at the same path.
fn hooks_installed_at(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|content| file_is_rimz_managed(&content))
}

fn installed_event_names() -> Vec<String> {
    WIRED_EVENTS.iter().map(|&e| e.to_owned()).collect()
}

#[cfg(test)]
mod tests;
