//! `rimz events` — streaming lifecycle transitions from durable history.

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use serde_json::{Map, Value};

use super::{GlobalFlags, render};

const DEFAULT_POLL: Duration = Duration::from_millis(250);
const POLL_ENV: &str = "RIMZ_EVENTS_POLL_MS";

#[derive(Debug, Args)]
pub struct EventsArgs {
    #[command(subcommand)]
    command: EventsSubcmd,
}

#[derive(Debug, Subcommand)]
enum EventsSubcmd {
    /// Follow agent lifecycle transitions as JSON Lines.
    Follow {
        /// Replay the current active log generation before following new events.
        #[arg(long)]
        replay: bool,
        /// Emit JSON Lines (implied for this streaming command).
        #[arg(long)]
        json: bool,
    },
    /// Emit a durable signal and fire matching loop tasks.
    Emit {
        /// Lowercase dot-separated signal name.
        name: String,
        /// Top-level JSON object carried by the signal.
        #[arg(long, value_name = "OBJECT")]
        json: Option<String>,
        /// Internal producer provenance.
        #[arg(long, hide = true, value_parser = ["cli", "forge"], default_value = "cli")]
        source: String,
    },
}

pub fn run(args: EventsArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        EventsSubcmd::Follow { replay, json } => {
            let _ = json;
            follow(replay, globals)
        }
        EventsSubcmd::Emit { name, json, source } => emit(&name, json.as_deref(), &source, globals),
    }
}

fn follow(replay: bool, globals: &GlobalFlags) -> Result<()> {
    let workspace = rimz::WorkspaceResolver::resolve_participant(".", globals.root.clone())
        .context("resolving current workspace")?;
    let paths = rimz::StatePaths::for_workspace(workspace.workspace_id)
        .context("resolving lifecycle event-log paths")?;
    let mut follower = rimz::store::follow::EventFollower::open(paths, replay)
        .context("opening lifecycle event stream")?;
    let stop = Arc::new(AtomicBool::new(false));
    register_stop_signals(Arc::clone(&stop))?;
    let poll = poll_interval();
    let mut stdout = std::io::stdout().lock();
    while !stop.load(Ordering::Relaxed) {
        let batch = follower.poll().context("following lifecycle events")?;
        for warning in batch.warnings {
            let _ = writeln!(render::err(), "rimz: warning: {warning}");
        }
        for event in batch.events {
            if !write_json_line(&mut stdout, &event)? {
                return Ok(());
            }
        }
        std::thread::sleep(poll);
    }
    Ok(())
}

fn register_stop_signals(stop: Arc<AtomicBool>) -> Result<()> {
    use signal_hook::consts::signal::{SIGINT, SIGTERM};

    signal_hook::flag::register(SIGINT, Arc::clone(&stop))
        .context("registering lifecycle stream SIGINT handler")?;
    signal_hook::flag::register(SIGTERM, stop)
        .context("registering lifecycle stream SIGTERM handler")?;
    Ok(())
}

fn poll_interval() -> Duration {
    std::env::var(POLL_ENV)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|millis| *millis > 0)
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_POLL)
}

fn emit(name: &str, raw_payload: Option<&str>, source: &str, globals: &GlobalFlags) -> Result<()> {
    let name = name
        .parse::<rimz::store::event::SignalName>()
        .map_err(anyhow::Error::msg)?;
    let source = validate_emit_source(&name, source)?;
    let payload = parse_payload(raw_payload)?;
    let workspace = rimz::WorkspaceResolver::resolve_participant(".", globals.root.clone())
        .context("resolving current workspace")?;
    let store = super::open_store(&workspace)?;
    let signal = rimz::harness::schedule::signal::Signal {
        name: name.clone(),
        payload,
        source,
        watch: None,
    };
    let event_id = store
        .append_signal(&workspace.session_name, &signal)
        .context("appending signal event")?;
    let fired = rimz::harness::schedule::signal::fire_signal(
        store.runtime_paths(),
        &workspace.project_root,
        &signal,
    )
    .context("firing signal tasks")?;
    let mut out = render::out();
    writeln!(
        out,
        "emitted {name} ({event_id}) · fired {} tasks",
        fired.len()
    )?;
    for task in fired {
        writeln!(out, "  {task}")?;
    }
    Ok(())
}

fn validate_emit_source(
    name: &rimz::store::event::SignalName,
    source: &str,
) -> Result<rimz::store::event::SignalSource> {
    use rimz::store::event::SignalSource;

    match source {
        "forge" => {
            if !matches!(
                name.as_str(),
                "ci.passed" | "ci.failed" | "pr.merged" | "pr.closed"
            ) {
                anyhow::bail!(
                    "--source forge accepts only ci.passed, ci.failed, pr.merged, or pr.closed"
                );
            }
            Ok(SignalSource::Forge)
        }
        "cli" => {
            if name.is_reserved() {
                anyhow::bail!("signal name `{name}` is reserved for RimZ");
            }
            Ok(SignalSource::Cli)
        }
        _ => unreachable!("clap restricts signal sources"),
    }
}

fn parse_payload(raw: Option<&str>) -> Result<Map<String, Value>> {
    let Some(raw) = raw else {
        return Ok(Map::new());
    };
    if raw.len() > 64 * 1024 {
        anyhow::bail!("--json payload exceeds 64 KiB");
    }
    serde_json::from_str::<Value>(raw)
        .context("parsing --json payload")?
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("--json payload must be a JSON object"))
}

fn write_json_line(
    writer: &mut impl Write,
    event: &rimz::store::follow::FollowEvent,
) -> Result<bool> {
    match serde_json::to_writer(&mut *writer, event) {
        Ok(()) => {}
        Err(error) if error.io_error_kind() == Some(std::io::ErrorKind::BrokenPipe) => {
            return Ok(false);
        }
        Err(error) => return Err(error.into()),
    }
    if let Err(error) = writer.write_all(b"\n").and_then(|()| writer.flush()) {
        if error.kind() == std::io::ErrorKind::BrokenPipe {
            return Ok(false);
        }
        return Err(error.into());
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_families_reject_custom_emit_but_accept_internal_forge() {
        use rimz::store::event::SignalSource;

        for raw in ["ci.passed", "ci.failed", "pr.merged", "pr.closed"] {
            let name = raw.parse().unwrap();
            assert!(validate_emit_source(&name, "cli").is_err(), "{raw}");
            assert_eq!(
                validate_emit_source(&name, "forge").unwrap(),
                SignalSource::Forge
            );
        }
        for raw in [
            "agent.idle",
            "wake.task",
            "team.idle",
            "ci.finished",
            "ci.custom",
            "pr.custom",
        ] {
            let name = raw.parse().unwrap();
            assert!(validate_emit_source(&name, "cli").is_err(), "{raw}");
            assert!(validate_emit_source(&name, "forge").is_err(), "{raw}");
        }
        let name = "deploy.finished".parse().unwrap();
        assert_eq!(
            validate_emit_source(&name, "cli").unwrap(),
            SignalSource::Cli
        );
        assert!(validate_emit_source(&name, "forge").is_err());
    }

    struct BrokenPipe;

    impl Write for BrokenPipe {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
    }

    #[test]
    fn closed_stream_consumer_is_a_clean_stop() {
        let event = rimz::agents::LifecycleEvent {
            v: rimz::agents::LIFECYCLE_EVENT_VERSION,
            event_id: rimz::ids::EventId::parse("evt_018f47a2c00070008000000000000000").unwrap(),
            at: "2026-06-01T12:00:00Z".parse().unwrap(),
            workspace_id: rimz::ids::WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
            kind: rimz::ids::AgentKind::new_unchecked("claude"),
            agent_id: rimz::ids::AgentSessionId::from("session-1"),
            agent_name: None,
            parent_agent_id: None,
            signal: rimz::agents::LifecycleSignal::Registered,
            prior_status: None,
            status: rimz::agents::AgentStatus::Idle,
            phase: rimz::agents::TurnPhase::Idle,
            transition: rimz::agents::LifecycleTransition::Normal,
            compaction_closed: false,
            waiting_cleared: false,
        };
        assert!(
            !write_json_line(
                &mut BrokenPipe,
                &rimz::store::follow::FollowEvent::Lifecycle(event)
            )
            .unwrap()
        );
    }
}
