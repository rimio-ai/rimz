//! Pi hook adapter.
//!
//! Pi's integration surface is in-process TypeScript extensions, so the
//! adapter ships one — [`extension.ts`](./extension.ts), embedded at compile
//! time and installed whole-file to `~/.pi/agent/extensions/rimz.ts`. The
//! extension forwards pi's lifecycle events to `rimz hooks feed --source pi`
//! as fire-and-forget children, inverting the Claude/Codex child direction
//! (pi runs Rimz, not the other way around); the wire it posts is the typed
//! shape in [`payloads`]. Lifecycle maps per docs/internals/hooks.md →
//! Appendix Pi: `session_start` registers, `before_agent_start` starts the
//! turn with the prompt, `agent_end` ends it carrying the in-band error bit,
//! `tool_execution_end` is the mutating-tool heartbeat, and
//! `session_before_compact`/`session_shutdown` are the compaction and exit
//! signals. Spend stays in [`spend`].
//!
//! Pi exposes no blocking hook channel, no subagents, no background tasks,
//! and no rate-limit surface (`docs/internals/adapter/pi-reference.md` →
//! "What Pi cannot support"), so those capabilities are declared off and the
//! absences render deliberately.

pub(crate) mod payloads;
pub(crate) mod spend;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, PlanLabel, ThreadKey, ToolClassification,
};
use super::lifecycle::LifecycleSignal;
use super::pricing::PriceBook;
use super::{
    AgentAdapter, AgentErr, AgentLifecycleObservation, ClassifiedHook, HookInstallPreview,
    HookInstallReport, HookUninstallReport, Result, agent_config_path, classify_agent_hook,
    optional_payload_string, read_optional_file, sanitize_user_prompt,
};
use crate::feed::{FeedItem, Resolution};
use crate::ids::AgentSessionId;
use crate::ledger::atomic;

/// Everything `const` about Pi, in one place. See [`AgentDescriptor`] for the
/// descriptor-vs-trait split.
static PI_DESCRIPTOR: AgentDescriptor = AgentDescriptor {
    kind: "pi",
    display_name: "Pi",
    brand: Brand {
        emblem: &[" ▗▛████▜▖", "  ▐▌  ▐▌", "  ▝▘  ▝▘"],
        color: 28,
    },
    // Pi sessions span whatever provider account the user wired, so no single
    // brand prefix is honest — the tier renders bare.
    plan_label: PlanLabel::TitleCaseOnly,
    // Pi's built-in tool set: `edit`/`write` edit files; `bash` mutates
    // without editing, so the reasoning phase survives it.
    tools: ToolClassification {
        mutating: &["bash", "edit", "write"],
        editing: &["edit", "write"],
    },
    capabilities: Capabilities {
        // Pi never asks natively — no permission prompts, plan approvals, or
        // questions — so there is nothing to route. An invented gate would be
        // Rimz posing the question, never the default install.
        blocking_feed: false,
        rate_limit_windows: false,
        subagents: false,
        background_tasks: false,
        registers_lazily: false,
        hook_install: true,
    },
    // Pi imposes no handler deadline, so any cap is Rimz-chosen — moot while
    // `blocking_feed` is off, since nothing ever blocks on the bridge.
    hook_cap: Duration::from_secs(60),
    process_names: &["pi"],
    hook_install_unavailable: None,
    thread_key: ThreadKey::PerFile,
};

/// The lifecycle events the embedded extension forwards — the single source of
/// truth for classification and the install report. Mirrors the `pi.on(...)`
/// registrations in [`extension.ts`](./extension.ts) (asserted by test).
const LIFECYCLE_EVENTS: &[&str] = &[
    "session_start",
    "before_agent_start",
    "agent_end",
    "tool_execution_end",
    "session_before_compact",
    "session_shutdown",
];

/// The Rimz pi extension, embedded at compile time and written whole-file on
/// install. Carries the `_rimz_managed` marker [`hooks_installed_at`] checks.
const EXTENSION_SOURCE: &str = include_str!("extension.ts");

#[derive(Clone, Debug, Default)]
pub struct PiAdapter;

impl AgentAdapter for PiAdapter {
    fn descriptor(&self) -> &'static AgentDescriptor {
        &PI_DESCRIPTOR
    }

    fn classify_hook(&self, event_name: &str, _payload: &Value) -> ClassifiedHook {
        // No blocking events: pi has no native ask to route, so every wired
        // event rides the lifecycle channel.
        classify_agent_hook(event_name, None, LIFECYCLE_EVENTS)
    }

    fn render_decision(&self, _item: &FeedItem, _resolution: &Resolution) -> Result<Value> {
        Err(AgentErr::Render {
            agent: "pi",
            reason: "pi exposes no blocking hook channel".to_owned(),
        })
    }

    fn render_neutral(&self, _event_name: &str) -> Result<Option<Value>> {
        // The extension's child is fire-and-forget — nothing reads its
        // stdout, so the neutral path prints nothing.
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
        // mapping is docs/internals/hooks.md → Appendix Pi.
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
        observation.total_tokens = parsed.total_tokens;
        // Declared absences: pi reports no context gauge on this wire (the
        // in-process `ctx.getContextUsage()` is future enrichment), and no
        // todo surface exists.
        Some(observation)
    }

    fn ends_session(&self, event_name: &str) -> bool {
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

    /// Pi logs `costUSD` directly, so the price book is unused. Lines are
    /// independent, so a resume is a plain offset.
    fn parse_spend(
        &self,
        path: &Path,
        resume: Option<&crate::agents::spending::SpendCursor>,
        _prices: &PriceBook,
    ) -> crate::agents::spending::SpendParse {
        spend::parse_pi_spend(path, resume.map_or(0, |cursor| cursor.offset))
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
}

fn pi_extension_path() -> Result<PathBuf> {
    // Honour an explicit override (`RIMZ_PI_EXTENSION`) so tests and tooling
    // can point the installer at a tempdir without touching real config.
    agent_config_path(
        "pi",
        "RIMZ_PI_EXTENSION",
        Path::new(".pi/agent/extensions/rimz.ts"),
    )
}

/// Install is whole-file ownership: pi has no config to merge into, so the
/// embedded source overwrites the path verbatim — idempotent by construction,
/// and a user edit is reclaimed on re-install (stated in the file header).
fn install_into(path: &Path) -> Result<HookInstallReport> {
    let existed = path.exists();
    atomic::write_bytes_atomically(path, EXTENSION_SOURCE.as_bytes())?;
    Ok(HookInstallReport {
        agent: "pi",
        config_path: path.to_path_buf(),
        installed_events: installed_event_names(),
        merged: existed,
    })
}

fn preview_install_at(path: &Path) -> Result<HookInstallPreview> {
    Ok(HookInstallPreview {
        agent: "pi",
        config_path: path.to_path_buf(),
        planned_events: installed_event_names(),
        original_config: read_optional_file("pi", path)?,
        candidate_config: EXTENSION_SOURCE.to_owned(),
        merged: path.exists(),
        // Pi manages no statusline.
        status_line_change: None,
        subagent_status_line_change: None,
    })
}

fn uninstall_from(path: &Path) -> Result<HookUninstallReport> {
    let existed = match std::fs::remove_file(path) {
        Ok(()) => true,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
        Err(source) => {
            return Err(AgentErr::InstallIo {
                agent: "pi",
                path: path.to_path_buf(),
                source,
            });
        }
    };
    Ok(HookUninstallReport {
        agent: "pi",
        config_path: path.to_path_buf(),
        removed_events: if existed {
            installed_event_names()
        } else {
            Vec::new()
        },
        existed,
    })
}

/// Best-effort like the other adapters: a missing or unreadable file reads as
/// "not installed". The `_rimz_managed` marker distinguishes the Rimz-owned
/// extension from a user's own file at the same path.
fn hooks_installed_at(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|text| text.contains("_rimz_managed"))
}

fn installed_event_names() -> Vec<String> {
    LIFECYCLE_EVENTS.iter().map(|&e| e.to_owned()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentHookClass;
    use crate::agents::lifecycle::TurnPhase;
    use crate::feed::{FeedKind, ResolutionMethod, Surface};
    use crate::ids::WorkspaceId;
    use serde_json::json;

    #[test]
    fn pi_classifies_lifecycle_events_and_unknowns() {
        for event in LIFECYCLE_EVENTS {
            let classified = PiAdapter.classify_hook(event, &Value::Null);
            assert_eq!(classified.class, AgentHookClass::Lifecycle, "event {event}");
            assert_eq!(classified.feed_kind, None, "event {event} never blocks");
        }
        for event in ["PermissionRequest", "SessionStart", "tool_call", "bogus"] {
            let classified = PiAdapter.classify_hook(event, &Value::Null);
            assert_eq!(classified.class, AgentHookClass::Unknown, "event {event}");
        }
    }

    #[test]
    fn pi_declares_its_absent_surfaces() {
        let capabilities = PiAdapter.descriptor().capabilities;
        assert!(!capabilities.blocking_feed);
        assert!(!capabilities.rate_limit_windows);
        assert!(!capabilities.subagents);
        assert!(!capabilities.background_tasks);
        assert!(capabilities.hook_install);
        assert!(PI_DESCRIPTOR.hook_install_unavailable.is_none());
    }

    #[test]
    fn session_start_registers_with_worktree() {
        let observation = PiAdapter
            .observe_lifecycle(
                "session_start",
                &json!({ "session_id": "sess-1", "cwd": "/home/u/code/query-engine" }),
            )
            .expect("observation");
        assert_eq!(observation.agent_id.as_deref(), Some("sess-1"));
        assert_eq!(observation.signal, LifecycleSignal::Registered);
        assert_eq!(
            observation.worktree_path.as_deref(),
            Some("/home/u/code/query-engine"),
        );
        assert_eq!(observation.parent_agent_id, None);
    }

    #[test]
    fn before_agent_start_starts_the_turn_with_the_sanitized_prompt() {
        let observation = PiAdapter
            .observe_lifecycle(
                "before_agent_start",
                &json!({ "session_id": "sess-1", "prompt": "  add a dark mode toggle  " }),
            )
            .expect("observation");
        assert_eq!(observation.signal, LifecycleSignal::TurnStarted);
        assert_eq!(
            observation.prompt.as_deref(),
            Some("add a dark mode toggle"),
        );
        assert_eq!(observation.task.as_deref(), Some("add a dark mode toggle"));
        // Harness control text never labels a row.
        let injected = PiAdapter
            .observe_lifecycle(
                "before_agent_start",
                &json!({ "session_id": "sess-1", "prompt": "<system-reminder>noise" }),
            )
            .expect("observation");
        assert_eq!(injected.prompt, None);
        assert_eq!(injected.task, None);
    }

    #[test]
    fn agent_end_completes_the_turn_with_model_and_tokens() {
        let observation = PiAdapter
            .observe_lifecycle(
                "agent_end",
                &json!({
                    "session_id": "sess-1",
                    "stop_reason": "stop",
                    "model": "gpt-5",
                    "total_tokens": 4200,
                }),
            )
            .expect("observation");
        assert_eq!(
            observation.signal,
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
        );
        assert_eq!(observation.model.as_deref(), Some("gpt-5"));
        assert_eq!(observation.total_tokens, Some(4200));
    }

    #[test]
    fn agent_end_carries_the_in_band_error_bit() {
        for payload in [
            json!({ "session_id": "sess-1", "stop_reason": "aborted" }),
            json!({ "session_id": "sess-1", "stop_reason": "error" }),
            json!({ "session_id": "sess-1", "stop_reason": "stop", "error_message": "boom" }),
        ] {
            let observation = PiAdapter
                .observe_lifecycle("agent_end", &payload)
                .expect("observation");
            assert_eq!(
                observation.signal,
                LifecycleSignal::TurnEnded {
                    errored: true,
                    parked_on_background: false,
                },
                "payload {payload}",
            );
        }
    }

    #[test]
    fn tool_execution_end_maps_the_mutating_subset() {
        // `edit` writes files — the acting transition.
        let edit = PiAdapter
            .observe_lifecycle(
                "tool_execution_end",
                &json!({ "session_id": "sess-1", "tool_name": "edit" }),
            )
            .expect("observation");
        assert_eq!(
            edit.signal,
            LifecycleSignal::ToolUsed {
                mutates: true,
                edits: true,
            },
        );
        // `bash` mutates without editing — the reasoning phase survives.
        let bash = PiAdapter
            .observe_lifecycle(
                "tool_execution_end",
                &json!({ "session_id": "sess-1", "tool_name": "bash" }),
            )
            .expect("observation");
        assert_eq!(
            bash.signal,
            LifecycleSignal::ToolUsed {
                mutates: true,
                edits: false,
            },
        );
        // Read-only tools stay silent.
        assert_eq!(
            PiAdapter.observe_lifecycle(
                "tool_execution_end",
                &json!({ "session_id": "sess-1", "tool_name": "read" }),
            ),
            None,
        );
    }

    /// The descriptor's `edits` split drives the shared phase machine: the
    /// first `edit` of a running turn moves reasoning → acting.
    #[test]
    fn an_edit_tool_ends_the_reasoning_phase() {
        use crate::agents::lifecycle::{LifecycleState, step};
        use crate::feed::AgentStatus;
        let running = LifecycleState {
            status: AgentStatus::Running,
            phase: TurnPhase::Reasoning,
            compacting: false,
        };
        let edit = PiAdapter
            .observe_lifecycle(
                "tool_execution_end",
                &json!({ "session_id": "sess-1", "tool_name": "edit" }),
            )
            .expect("observation");
        let next = step(Some(&running), &edit.signal);
        assert_eq!(next.next.phase, TurnPhase::Acting);
    }

    #[test]
    fn compaction_and_shutdown_signals() {
        let compacting = PiAdapter
            .observe_lifecycle("session_before_compact", &json!({ "session_id": "sess-1" }))
            .expect("observation");
        assert_eq!(compacting.signal, LifecycleSignal::Compacting);
        let ended = PiAdapter
            .observe_lifecycle("session_shutdown", &json!({ "session_id": "sess-1" }))
            .expect("observation");
        assert_eq!(ended.signal, LifecycleSignal::Ended);
    }

    #[test]
    fn unrecognized_events_observe_nothing() {
        assert_eq!(
            PiAdapter.observe_lifecycle("tool_call", &json!({ "session_id": "sess-1" })),
            None,
        );
        assert_eq!(PiAdapter.observe_lifecycle("bogus", &json!({})), None);
    }

    #[test]
    fn session_boundaries_end_and_move_on() {
        assert!(PiAdapter.ends_session("session_shutdown"));
        assert!(!PiAdapter.ends_session("agent_end"));
        assert!(PiAdapter.moves_on("before_agent_start"));
        assert!(PiAdapter.moves_on("agent_end"));
        assert!(!PiAdapter.moves_on("session_start"));
    }

    #[test]
    fn resume_command_is_pi_with_the_session_id() {
        assert_eq!(
            PiAdapter.resume_command("0199aaf2", Path::new("/tmp")),
            Some(vec![
                "pi".to_owned(),
                "--session".to_owned(),
                "0199aaf2".to_owned(),
            ]),
        );
    }

    /// Empty stdout is pi's neutral: the extension's child is fire-and-forget
    /// and nothing reads it. Golden so the shape never drifts.
    #[test]
    fn render_neutral_prints_nothing() {
        let rendered = PiAdapter.render_neutral("agent_end").unwrap();
        insta::assert_snapshot!(format!("{rendered:?}"), @"None");
    }

    #[test]
    fn render_decision_is_an_explicit_error() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/rimz-test"));
        let item = FeedItem::new(
            workspace,
            Surface::Bridge,
            FeedKind::Permission,
            "allow?",
            "pi",
            "agent-hook",
        );
        let resolution =
            Resolution::new(json!({ "choice": "allow" }), ResolutionMethod::HookBridge);
        let err = PiAdapter.render_decision(&item, &resolution).unwrap_err();
        assert!(matches!(err, AgentErr::Render { agent: "pi", .. }));
    }

    #[test]
    fn install_round_trip_owns_the_whole_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("extensions").join("rimz.ts");

        let report = install_into(&path).unwrap();
        assert_eq!(report.agent, "pi");
        assert!(!report.merged, "fresh install creates the file");
        assert_eq!(report.installed_events, installed_event_names());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), EXTENSION_SOURCE);
        assert!(hooks_installed_at(&path));

        // Re-install over an edited file reclaims it verbatim.
        std::fs::write(&path, "// user edit\n").unwrap();
        let again = install_into(&path).unwrap();
        assert!(again.merged, "overwrote an existing file");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), EXTENSION_SOURCE);

        let removed = uninstall_from(&path).unwrap();
        assert!(removed.existed);
        assert_eq!(removed.removed_events, installed_event_names());
        assert!(!path.exists());
        assert!(!hooks_installed_at(&path));

        // Uninstall on a missing file is a clean no-op.
        let missing = uninstall_from(&path).unwrap();
        assert!(!missing.existed);
        assert!(missing.removed_events.is_empty());
    }

    #[test]
    fn preview_carries_the_embedded_source_without_touching_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rimz.ts");
        let preview = preview_install_at(&path).unwrap();
        assert_eq!(preview.agent, "pi");
        assert_eq!(preview.planned_events, installed_event_names());
        assert_eq!(preview.original_config, None);
        assert_eq!(preview.candidate_config, EXTENSION_SOURCE);
        assert!(!preview.merged);
        assert_eq!(preview.status_line_change, None);
        assert_eq!(preview.subagent_status_line_change, None);
        assert!(!path.exists(), "preview never writes");

        std::fs::write(&path, "// user file\n").unwrap();
        let over = preview_install_at(&path).unwrap();
        assert!(over.merged, "an existing file is reported as overwritten");
        assert_eq!(over.original_config.as_deref(), Some("// user file\n"));
    }

    #[test]
    fn hooks_installed_requires_the_managed_marker() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rimz.ts");
        assert!(!hooks_installed_at(&path), "missing file is not installed");
        std::fs::write(&path, "export default function user(pi) {}\n").unwrap();
        assert!(
            !hooks_installed_at(&path),
            "a user's own extension at the path is not Rimz's",
        );
    }

    /// The embedded extension and this adapter agree: the marker, the feed
    /// command, and every wired event registration are present in the source.
    #[test]
    fn extension_source_wires_every_lifecycle_event() {
        assert!(EXTENSION_SOURCE.contains("_rimz_managed"));
        assert!(EXTENSION_SOURCE.contains(r#"["hooks", "feed", "--source", "pi"]"#));
        assert!(EXTENSION_SOURCE.contains("RIMZ_AGENT_PID"));
        for event in LIFECYCLE_EVENTS {
            assert!(
                EXTENSION_SOURCE.contains(&format!("pi.on(\"{event}\"")),
                "extension registers {event}",
            );
        }
    }
}
