//! End-user journey suite: launch the room, run agents, watch the sidebar.
//!
//! These tests read as the session story in `docs/guide/product.md` and
//! `docs/guide/experience.md`. They drive the real `rimz sidebar serve`
//! renderer through a `portable-pty` over a real ledger and assert on
//! `vt100`-parsed screen text — what the column actually shows. Renderer
//! mechanics stay in `docs/internals/sidebar.md`.
//!
//! "Running an agent" is simulated faithfully: Rimz only ever observes agents
//! through their hooks, and the work pane itself is opaque to it (resolvers own
//! pane I/O). So firing `rimz hooks feed --source claude` through an installed
//! hook is the end-user act of running an agent.
//!
#![allow(clippy::print_stdout, clippy::print_stderr)]

mod deep;
mod resize_redraw;
mod sidebar_phases;

use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use assert_cmd::cargo::cargo_bin;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use rimz::feed::PaneRef;
use rimz::ids::{MuxName, PaneId, ViewKind};
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::common::{Env, ScrubSessionEnvExt};

/// Sidebar pane dimensions for the journey. Within the documented 24–36 col
/// band, tall enough that no phase scrolls off.
const ROWS: u16 = 40;
const COLS: u16 = 36;

/// How long a phase may take to appear. The serve loop ticks every second, so
/// a couple of ticks suffice on an idle machine; `wait_for` returns the instant
/// the predicate holds, so this budget only bites on failure. It is generous
/// because the full integration suite runs these PTY tests in parallel with the
/// real-mux smokes, and a starved renderer can take several seconds to paint
/// its first frame.
pub const SETTLE: Duration = Duration::from_secs(15);

/// A live Rimz "room": a real `rimz sidebar serve` renderer in a
/// `portable-pty`, reading the [`Env`] ledger that hooks mutate.
///
/// The renderer shares the `Env` *state* (the ledger, via `XDG_STATE_HOME`) but
/// gets its own short `XDG_RUNTIME_DIR`: the per-instance wakeup socket
/// (`sock/sidebar.<35-char-id>.sock`) must stay under the 108-byte AF_UNIX
/// limit, which `Env`'s deep tempdir would overflow. The renderer only needs
/// that socket for nudges; the 1 s tick covers the rest, so a separate runtime
/// dir is harmless.
pub struct RoomHarness<'a> {
    env: &'a Env,
    parser: Arc<Mutex<vt100::Parser>>,
    pane_file: PathBuf,
    pane_roster: Arc<Mutex<PaneRoster>>,
    mux: MuxName,
    _runtime: TempDir,
    _master: Box<dyn MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl<'a> RoomHarness<'a> {
    /// Spawn the renderer against `env`'s workspace. `mux` only selects the
    /// pane-discovery/wakeup backend; with no live mux session the self-close
    /// latch never trips (the renderer never sees a sibling pane), so the
    /// column renders deterministically off the ledger and the tick.
    pub fn launch(env: &'a Env, mux: MuxName) -> Self {
        Self::launch_inner(env, mux, false)
    }

    /// Spawn the renderer with its pane fixture unreadable, so every
    /// in-process produce fails from the very first cycle — the degraded loop
    /// a dead mux or moved workspace would feed. Blackbox: the real sidebar
    /// process runs; only its produce input is broken.
    pub fn launch_degraded(env: &'a Env, mux: MuxName) -> Self {
        Self::launch_inner(env, mux, true)
    }

    fn launch_inner(env: &'a Env, mux: MuxName, broken_pane_fixture: bool) -> Self {
        let bin = cargo_bin("rimz");
        assert!(bin.exists(), "rimz binary missing: {}", bin.display());

        // Materialize the workspace ledger so a never-used room answers
        // `sidebar snapshot` with an empty-but-valid snapshot (Phase 0), not a
        // degraded "ledger not found" banner.
        let _ = env.ledger();

        let runtime = tempfile::Builder::new()
            .prefix("rz")
            .rand_bytes(6)
            .tempdir()
            .expect("short runtime tempdir");
        let pane_file = runtime.path().join("panes.json");
        let initial_roster = PaneRoster::from_ledger(env);
        if broken_pane_fixture {
            // Unparseable on purpose: the produce's fixture read errors every
            // cycle, exercising the degraded outcome path end to end.
            std::fs::write(&pane_file, b"not json").expect("write broken pane fixture");
        } else {
            write_panes(&pane_file, initial_roster.panes(mux, &env.project_root));
        }
        let pane_roster = Arc::new(Mutex::new(initial_roster));

        let pair = native_pty_system()
            .openpty(PtySize {
                rows: ROWS,
                cols: COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");

        let mut cmd = CommandBuilder::new(&bin);
        cmd.scrub_session_env();
        cmd.args([
            "sidebar",
            "serve",
            "--mux",
            mux.as_str(),
            "--workspace-id",
            env.workspace_id.as_str(),
            "--session-name",
            "rimz-journey",
            "--tick-seconds",
            "1",
        ]);
        cmd.env("RIMZ_BIN", env.rimz_bin());
        cmd.env("XDG_STATE_HOME", env.state_root());
        cmd.env("XDG_CONFIG_HOME", env.config_root());
        cmd.env("XDG_RUNTIME_DIR", runtime.path());
        cmd.env("HOME", &env.home_root);
        cmd.env("RIMZ_TEST_PANE_LIST", &pane_file);
        cmd.env_remove("RUST_LOG");

        let child = pair.slave.spawn_command(cmd).expect("spawn rimz sidebar");
        drop(pair.slave);

        // Feed one long-lived parser incrementally from the reader thread, so
        // each `screen()` poll is O(grid) instead of re-parsing the whole
        // accumulated byte stream (which is O(n²) over a 15 s wait loop).
        let parser = Arc::new(Mutex::new(vt100::Parser::new(ROWS, COLS, 0)));
        let mut reader = pair.master.try_clone_reader().expect("clone reader");
        let sink = Arc::clone(&parser);
        let reader = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => sink.lock().expect("parser").process(&buf[..n]),
                }
            }
        });

        Self {
            env,
            parser,
            pane_file,
            pane_roster,
            mux,
            _runtime: runtime,
            _master: pair.master,
            child,
            reader: Some(reader),
        }
    }

    /// Plain-text contents of the current pane — glyphs only, control codes
    /// collapsed by the vt100 grid (so we match what the pane *shows*).
    pub fn screen(&self) -> String {
        self.parser.lock().expect("parser").screen().contents()
    }

    /// Poll the pane until `pred` holds or `budget` elapses; return the final
    /// screen either way so assertions can print it.
    pub fn wait_for(&self, pred: impl Fn(&str) -> bool, budget: Duration) -> String {
        let deadline = Instant::now() + budget;
        loop {
            let text = self.screen();
            if pred(&text) || Instant::now() >= deadline {
                return text;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Wire the room the way the user does on first run: `rimz hooks install`
    /// for each agent. Until this runs, the agents have no Rimz hook to fire,
    /// so [`agent_hook`](Self::agent_hook) is a no-op — exactly the state of a
    /// freshly-started room that nobody onboarded.
    pub fn onboard(&self, agents: &[&str]) {
        for agent in agents {
            self.env.install_agent_hooks(agent);
        }
    }

    /// Run an agent the way the end user does: the agent fires its *installed*
    /// hook. The event is read from the payload's `hook_event_name`.
    ///
    /// With no hook wired (the room was never onboarded), the agent never
    /// calls Rimz — so this is a no-op and nothing reaches the ledger,
    /// faithfully reproducing "I ran an agent and nothing showed up". Hand-firing
    /// `rimz hooks feed` regardless would mask exactly that bug.
    pub fn agent_hook(&self, source: &str, payload: &Value) {
        let event = payload
            .get("hook_event_name")
            .and_then(Value::as_str)
            .expect("payload carries hook_event_name");
        let session_id = payload_session_id(payload, source);
        if event == "SessionStart" {
            let cwd = payload
                .get("worktree_path")
                .and_then(Value::as_str)
                .unwrap_or_else(|| self.env.project_root.to_str().unwrap_or("."));
            self.start_agent_process(&session_id, source, cwd);
        } else if event == "SessionEnd" {
            self.stop_agent_process(&session_id);
        }
        if !self.env.agent_hooks_installed(source) {
            return;
        }
        // The hook reads the mux's per-pane env var to stamp the pane it ran
        // inside; feed it the roster's pane id so the bind matches the fixture.
        let pane_env = self.pane_env(&session_id);
        let pane_env: Vec<(&str, &str)> = pane_env
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        let out = self
            .env
            .run_installed_hook_in_pane(source, &payload.to_string(), &pane_env);
        assert!(
            out.status.success(),
            "{source} {event} hook failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Spawn a blocking agent hook (the bridge path) through its installed
    /// command, returning the live child. Requires an onboarded room.
    pub fn spawn_agent(&self, source: &str, payload: &Value) -> std::process::Child {
        assert!(
            self.env.agent_hooks_installed(source),
            "spawn_agent needs an onboarded room — call onboard(&[{source:?}]) first"
        );
        let session_id = payload_session_id(payload, source);
        let pane_env = self.pane_env(&session_id);
        let pane_env: Vec<(&str, &str)> = pane_env
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        self.env
            .spawn_installed_hook_in_pane(source, &payload.to_string(), &pane_env)
    }

    /// The per-pane env the mux exports for `session_id`'s pane, matching the
    /// roster fixture's pane id, so the simulated hook stamps the same pane the
    /// renderer sees live. Empty when the session has no pane (e.g. SessionEnd
    /// already retired it) — the hook then stamps nothing, as it would for a
    /// pane that has closed.
    fn pane_env(&self, session_id: &str) -> Vec<(String, String)> {
        let roster = self.pane_roster.lock().expect("pane roster");
        let Some(index) = roster.index_of(session_id) else {
            return Vec::new();
        };
        let pair = match self.mux {
            MuxName::Tmux => ("TMUX_PANE".to_owned(), format!("%{index}")),
            MuxName::Zellij => ("ZELLIJ_PANE_ID".to_owned(), index.to_string()),
        };
        vec![pair]
    }

    fn start_agent_process(&self, session_id: &str, command: &str, cwd: &str) {
        let mut roster = self.pane_roster.lock().expect("pane roster");
        roster.start(session_id, command, cwd);
        write_panes(
            &self.pane_file,
            roster.panes(self.mux, &self.env.project_root),
        );
    }

    fn stop_agent_process(&self, session_id: &str) {
        let mut roster = self.pane_roster.lock().expect("pane roster");
        roster.stop(session_id);
        write_panes(
            &self.pane_file,
            roster.panes(self.mux, &self.env.project_root),
        );
    }
}

#[derive(Default)]
struct PaneRoster {
    next_index: usize,
    agents: BTreeMap<String, PaneProcess>,
    extra_panes: BTreeMap<String, PaneRef>,
}

struct PaneProcess {
    index: usize,
    command: String,
    cwd: String,
}

impl PaneRoster {
    fn from_ledger(env: &Env) -> Self {
        let mut roster = Self::default();
        let Ok(snapshot) = env.ledger().snapshot() else {
            return roster;
        };
        for agent in snapshot.agents {
            let Some(cwd) = agent.worktree_path.as_deref() else {
                continue;
            };
            roster.start(&agent.agent_id, &agent.kind, cwd);
        }
        for item in snapshot
            .needs_attention
            .iter()
            .chain(snapshot.resolver_working.iter())
        {
            if item.source_kind == "agent-hook" {
                continue;
            }
            if let Some(pane) = &item.pane {
                roster
                    .extra_panes
                    .insert(item.request_id.to_string(), pane.clone());
            }
        }
        roster
    }

    fn start(&mut self, session_id: &str, command: &str, cwd: &str) {
        let index = if let Some(existing) = self.agents.get(session_id) {
            existing.index
        } else if self.agents.is_empty() {
            0
        } else {
            self.next_index = self.next_index.max(1);
            let index = self.next_index;
            self.next_index += 1;
            index
        };
        self.next_index = self.next_index.max(index + 1);
        self.agents.insert(
            session_id.to_owned(),
            PaneProcess {
                index,
                command: command.to_owned(),
                cwd: cwd.to_owned(),
            },
        );
    }

    fn stop(&mut self, session_id: &str) {
        self.agents.remove(session_id);
    }

    fn index_of(&self, session_id: &str) -> Option<usize> {
        self.agents.get(session_id).map(|process| process.index)
    }

    fn panes(&self, mux: MuxName, project_root: &std::path::Path) -> Vec<PaneRef> {
        if self.agents.is_empty() && self.extra_panes.is_empty() {
            return vec![process_pane(
                mux,
                0,
                "zsh",
                project_root.display().to_string(),
            )];
        }
        let mut panes = self
            .agents
            .values()
            .map(|process| process_pane(mux, process.index, &process.command, process.cwd.clone()))
            .collect::<Vec<_>>();
        panes.extend(self.extra_panes.values().cloned());
        panes.sort_by_key(|pane| pane.pane_id.raw().to_owned());
        panes
    }
}

impl Drop for RoomHarness<'_> {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(handle) = self.reader.take() {
            let _ = handle.join();
        }
    }
}

// --- agent hook payload builders ---
//
// The adapter reads worktree/model/effort/task from the hook payload, so a
// test controls the worktree group and capability line by what it sends.

/// The session id a hook payload carries (`session_id`, then `agent_id`),
/// falling back to the source name for payloads that name neither.
fn payload_session_id(payload: &Value, source: &str) -> String {
    payload
        .get("session_id")
        .or_else(|| payload.get("agent_id"))
        .and_then(Value::as_str)
        .unwrap_or(source)
        .to_owned()
}

/// `SessionStart` lifecycle payload. Groups key on `worktree_path` (the
/// renderer labels by branch), so each worktree needs a distinct path — without
/// one, `run_feed` backfills the cwd and every agent collapses into one group.
/// Mode is interactive by default, omitted from the pill.
pub fn session_start(session_id: &str, model: &str, effort: &str, branch: &str) -> Value {
    session_start_at(
        session_id,
        model,
        effort,
        format!("/work/query-engine-{branch}"),
        Some(branch),
    )
}

pub fn session_start_at(
    session_id: &str,
    model: &str,
    effort: &str,
    worktree_path: impl Into<String>,
    branch: Option<&str>,
) -> Value {
    json!({
        "hook_event_name": "SessionStart",
        "session_id": session_id,
        "permission_mode": "default",
        "approval_policy": "ask",
        "model": model,
        "thinking_level": effort,
        "reasoning_effort": effort,
        "worktree_path": worktree_path.into(),
        "worktree_branch": branch,
    })
}

/// `UserPromptSubmit` lifecycle payload carrying the prompt as the task.
pub fn user_prompt_submit(session_id: &str, prompt: &str) -> Value {
    json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": session_id,
        "prompt": prompt,
    })
}

/// `PostToolUse` lifecycle payload naming the completed tool. A file-editing
/// tool (`apply_patch`, `Edit`) ends the turn's thinking head.
pub fn post_tool_use(session_id: &str, tool_name: &str) -> Value {
    json!({
        "hook_event_name": "PostToolUse",
        "session_id": session_id,
        "tool_name": tool_name,
    })
}

/// Permission request payload. `secret` is embedded in the
/// payload command so a test can assert the sidebar never reproduces it
/// (notify-don't-answer).
pub fn permission_request(session_id: &str, secret: &str) -> Value {
    json!({
        "hook_event_name": "PermissionRequest",
        "tool_name": "shell",
        "command": ["echo", secret],
        "session_id": session_id,
    })
}

/// Absolute path to the built `rimz` binary, or `None` if it is not
/// built (lets deep tests self-skip rather than fail).
pub fn rimz_bin() -> Option<PathBuf> {
    let bin = cargo_bin("rimz");
    bin.exists().then_some(bin)
}

fn process_pane(mux: MuxName, index: usize, command: &str, cwd: String) -> PaneRef {
    let raw = match mux {
        MuxName::Tmux => format!("%{index}"),
        MuxName::Zellij => format!("terminal_{index}"),
    };
    PaneRef {
        pane_id: PaneId::from_parts(mux, raw),
        session_name: "rimz-journey".to_owned(),
        view_id: Some(match mux {
            MuxName::Tmux => "@0".to_owned(),
            MuxName::Zellij => "tab_0".to_owned(),
        }),
        view_kind: Some(match mux {
            MuxName::Tmux => ViewKind::Window,
            MuxName::Zellij => ViewKind::Tab,
        }),
        view_name: None,
        is_focused: false,
        command: Some(command.to_owned()),
        spawn_command: None,
        cwd: Some(cwd),
        pane_pid: None,
        pane_process_start: None,
        resumed_session_id: None,
        elevated_agent: None,
    }
}

fn write_panes(path: &std::path::Path, panes: Vec<PaneRef>) {
    std::fs::write(path, serde_json::to_vec_pretty(&panes).expect("pane json"))
        .expect("write pane fixture");
}
