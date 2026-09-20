//! Bring up a disposable staged room, print its navigation card, and hold it.

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::{HostSandbox, RoomRecord};

#[derive(Deserialize)]
struct Panes {
    session: String,
    tabs: Vec<Tab>,
}

#[derive(Deserialize)]
struct Tab {
    view_id: Option<String>,
    name: Option<String>,
    panes: Vec<Pane>,
}

#[derive(Deserialize)]
struct Pane {
    pane_id: String,
    kind: String,
    agent: Option<Agent>,
}

#[derive(Deserialize)]
struct Agent {
    handle: String,
}

#[derive(Deserialize)]
struct Renderer {
    role: String,
    pane_id: Option<String>,
}

#[derive(Deserialize)]
struct Snapshot {
    worktree_groups: Vec<Group>,
}

#[derive(Deserialize)]
struct Group {
    pipeline: Option<Pipeline>,
}

#[derive(Deserialize)]
struct Pipeline {
    stage: String,
    owner: Option<String>,
}

pub(super) fn run(workspace: &Path, args: &[String]) -> Result<()> {
    if !cfg!(target_os = "linux") {
        bail!(
            "sandbox room requires Linux: only the /proc sandbox reaper cleans up room servers under XDG_RUNTIME_DIR"
        );
    }
    let mut mux = None;
    let mut budget = Some(Duration::from_secs(30 * 60));
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--mux" => mux = args.next().map(String::as_str),
            "--for" => {
                budget =
                    crate::deadline::parse_budget(args.next().context("--for needs a duration")?)?;
            }
            _ => bail!("unknown room argument {arg}; use --mux <tmux|zellij> [--for <duration>]"),
        }
    }
    let mux = mux
        .filter(|mux| matches!(*mux, "tmux" | "zellij"))
        .context("sandbox room requires --mux <tmux|zellij>")?;
    let binary = std::env::var_os("RIMZ_BIN").map_or_else(
        || workspace.join("target/debug/rimz"),
        |path| workspace.join(path),
    );
    let binary = binary.canonicalize().context("development rimz missing; run cargo build -p rimz --bin rimz --features testkit (or set RIMZ_BIN)")?;
    let sandbox = HostSandbox::for_manual_command()?;
    let root = sandbox._root.path();
    let home = &sandbox.env["HOME"];
    let stub_dir = home.join("bin");
    let path = std::env::join_paths(std::iter::once(stub_dir.clone()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))?;
    let mut room = Room {
        sandbox: &sandbox,
        binary: &binary,
        mux,
        path,
        cwd: home.clone(),
    };
    room.rimz(
        "testkit preflight; run cargo build -p rimz --bin rimz --features testkit",
        &["sidebar", "click", "--help"],
    )?;
    for (key, value) in [
        ("user.email", "sandbox@rimz.invalid"),
        ("user.name", "RimZ sandbox"),
        ("init.defaultBranch", "main"),
        ("safe.directory", "*"),
    ] {
        let mut command = room.command("git");
        command
            .args(["config", "--file"])
            .arg(home.join(".gitconfig"))
            .args([key, value]);
        output("sandbox Git identity", &mut command)?;
    }
    let assets = workspace.join("xtask/assets/sandbox-room");
    std::fs::create_dir_all(&stub_dir).context("creating stub directory")?;
    let stub = stub_dir.join("claude");
    std::fs::copy(assets.join("claude"), &stub).context("copying stub provider")?;
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))?;
    let rimz_home = &sandbox.env["RIMZ_HOME"];
    for file in [
        "agents/claude.md",
        "agents/worker.md",
        "teams/forge.md",
        "config.toml",
    ] {
        let destination = rimz_home.join(file);
        std::fs::create_dir_all(destination.parent().context("fixture has no parent")?)?;
        std::fs::copy(assets.join(file), destination)
            .with_context(|| format!("copying fixture {file}"))?;
    }
    let repo = home.join("room");
    std::fs::create_dir(&repo).context("creating room repo")?;
    room.cwd = repo.clone();
    output("room git init", room.command("git").arg("init"))?;
    std::fs::write(repo.join("README.md"), "# Disposable sidebar room\n")?;
    output(
        "room git add",
        room.command("git").args(["add", "README.md"]),
    )?;
    output(
        "room git commit",
        room.command("git")
            .args(["commit", "-m", "Open sandbox room"]),
    )?;
    verify_stub(&room.path, &stub, |candidate| {
        candidate
            .metadata()
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    })?;
    let mut start = room.command(&binary);
    start
        .arg(format!("--{mux}"))
        .arg("start")
        .arg(&repo)
        .arg("--no-attach");
    output("room start", &mut start)?;
    let session = room.session()?;
    // The outer tmux injects its identity after apply_env, so clear it inside the client too.
    let attach = format!(
        "unset TMUX TMUX_PANE; for key in $(env | sed -n 's/^\\(ZELLIJ[^=]*\\)=.*/\\1/p'); do unset \"$key\"; done; exec {} --{mux} attach {}",
        quote(binary.as_os_str()),
        quote(OsStr::new(&session))
    );
    // Zellij materializes a new tab's layout panes only while a client is attached, so the
    // client comes up before the team: without it `teams` fails in the backend's
    // `wait_for_named_tab_materialized`.
    output(
        "sized client attach",
        room.command("tmux").args([
            "new-session",
            "-d",
            "-x",
            "200",
            "-y",
            "50",
            "-s",
            "room-client",
            &attach,
        ]),
    )?;
    room.wait_for_client(&session)?;
    room.rimz(
        "team launch",
        &["teams", "forge", "-w", "probe", "--isolation", "host"],
    )?;
    room.cwd = home.join("room-worktrees/probe");
    room.rimz(
        "open board",
        &["teams", "flip", "Build", "room opened", "--team", "forge"],
    )?;
    let record = RoomRecord {
        mux: mux.into(),
        session,
        worktree: room.cwd.clone(),
        repo,
        stub_dir,
    };
    let (panes, renderers, snapshot) = room.ready()?;
    // Warm every tab, the team's last, so each sidebar has been watched once and hands over
    // at the client's width.
    let mut tabs: Vec<_> = panes.tabs.iter().enumerate().collect();
    tabs.sort_by_key(|(_, tab)| tab.panes.iter().any(|pane| pane.kind == "agent"));
    for (index, tab) in tabs {
        room.look(&record.session, index, tab)?;
    }
    RoomRecord::write(root, &record)?;
    let card = render_card(root, &binary, &record, &panes, &renderers, &snapshot)?;
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(card.as_bytes())?;
    stdout.flush()?;
    drop(stdout);
    let started = Instant::now();
    loop {
        let delay = match budget {
            Some(limit) => {
                let remaining = limit.saturating_sub(started.elapsed());
                if remaining.is_zero() {
                    break;
                }
                remaining.min(Duration::from_secs(1))
            }
            None => Duration::from_secs(1),
        };
        std::thread::sleep(delay);
    }
    Ok(())
}

struct Room<'a> {
    sandbox: &'a HostSandbox,
    binary: &'a Path,
    mux: &'a str,
    path: OsString,
    cwd: PathBuf,
}

impl Room<'_> {
    fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut command = Command::new(program);
        self.sandbox.apply_to(&mut command, true);
        command
            .env("PATH", &self.path)
            .current_dir(&self.cwd)
            .stdin(Stdio::null());
        command
    }

    fn rimz(&self, step: &str, args: &[&str]) -> Result<Vec<u8>> {
        output(
            step,
            self.command(self.binary)
                .arg(format!("--{}", self.mux))
                .args(args),
        )
    }

    fn json<T: serde::de::DeserializeOwned>(&self, step: &str, args: &[&str]) -> Result<T> {
        serde_json::from_slice(&self.rimz(step, args)?)
            .with_context(|| format!("{step}: decoding JSON"))
    }

    /// `start` returns before the fresh room answers a pane roster, so poll for its session.
    fn session(&self) -> Result<String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match self.json::<Panes>("room session", &["pane", "list", "--json"]) {
                Ok(panes) => return Ok(panes.session),
                Err(error) if Instant::now() >= deadline => return Err(error),
                Err(_) => std::thread::sleep(Duration::from_millis(200)),
            }
        }
    }

    /// Bring one tab into the attached client's view, so its sidebar repaints and takes the
    /// client's width.
    fn look(&self, session: &str, index: usize, tab: &Tab) -> Result<()> {
        let anchor = tab
            .panes
            .iter()
            .find(|pane| pane.kind == "sidebar")
            .or_else(|| tab.panes.first());
        let Some(anchor) = anchor else {
            return Ok(());
        };
        let argv = look_argv(
            self.binary,
            self.mux,
            session,
            tab_number(tab, index),
            &anchor.pane_id,
        );
        let (program, args) = argv.split_first().context("look command is empty")?;
        output("visit tab", self.command(program).args(args))?;
        Ok(())
    }

    /// The client attaches asynchronously, and Zellij materializes a new tab's layout panes
    /// only while one is attached; tmux needs no client, so only Zellij waits.
    fn wait_for_client(&self, session: &str) -> Result<()> {
        if self.mux != "zellij" {
            return Ok(());
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            // A session with no client prints the header row alone.
            let attached = output(
                "client list",
                self.command("zellij")
                    .args(["--session", session, "action", "list-clients"]),
            )
            .is_ok_and(|clients| String::from_utf8_lossy(&clients).lines().count() > 1);
            if attached {
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!(
                    "no client attached to {session}; Zellij materializes a new tab's layout panes only for one"
                );
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn ready(&self) -> Result<(Panes, Vec<Renderer>, Snapshot)> {
        let started = Instant::now();
        loop {
            let panes: Panes = self.json("readiness pane list", &["pane", "list", "--json"])?;
            let renderers: Vec<Renderer> =
                self.json("readiness renderers", &["sidebar", "renderers", "--json"])?;
            let snapshot: Snapshot =
                self.json("readiness snapshot", &["sidebar", "snapshot", "--json"])?;
            let sidebars: Vec<_> = panes
                .tabs
                .iter()
                .flat_map(|tab| &tab.panes)
                .filter(|pane| pane.kind == "sidebar")
                .collect();
            let missing: Vec<_> = sidebars
                .iter()
                .filter(|pane| {
                    !renderers
                        .iter()
                        .any(|renderer| renderer.pane_id.as_deref() == Some(&pane.pane_id))
                })
                .map(|pane| pane.pane_id.as_str())
                .collect();
            // `pane list` lags a fresh sidebar by seconds, and a sidebar it still omits gets no
            // tab warm-up and no card block, so wait for the listing to name every live renderer.
            let unlisted: Vec<_> = renderers
                .iter()
                .filter_map(|renderer| renderer.pane_id.as_deref())
                .filter(|pane_id| !sidebars.iter().any(|pane| pane.pane_id == *pane_id))
                .collect();
            let pipeline = snapshot
                .worktree_groups
                .iter()
                .filter_map(|group| group.pipeline.as_ref())
                .find(|pipeline| pipeline.stage == "Build");
            let agents = panes
                .tabs
                .iter()
                .flat_map(|tab| &tab.panes)
                .filter(|pane| pane.kind == "agent")
                .count();
            if !sidebars.is_empty()
                && missing.is_empty()
                && unlisted.is_empty()
                && pipeline.is_some()
                && agents == 2
            {
                return Ok((panes, renderers, snapshot));
            }
            if started.elapsed() >= Duration::from_secs(30) {
                bail!(
                    "room readiness timed out: {} sidebars, missing live renderer for {missing:?}, renderers on unlisted panes {unlisted:?}; {} agents (need 2); Build pipeline present: {}; stages seen: {:?}",
                    sidebars.len(),
                    agents,
                    pipeline.is_some(),
                    snapshot
                        .worktree_groups
                        .iter()
                        .filter_map(|group| group.pipeline.as_ref().map(|pipeline| &pipeline.stage))
                        .collect::<Vec<_>>()
                );
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

fn output(step: &str, command: &mut Command) -> Result<Vec<u8>> {
    let result = command
        .output()
        .with_context(|| format!("{step}: {command:?}"))?;
    if !result.status.success() {
        bail!(
            "{step}: {command:?} exited with {}: {}",
            result.status,
            String::from_utf8_lossy(&result.stderr)
        );
    }
    Ok(result.stdout)
}

fn verify_stub(path: &OsStr, stub: &Path, executable: impl Fn(&Path) -> bool) -> Result<()> {
    let resolved = std::env::split_paths(path)
        .map(|dir| dir.join("claude"))
        .find(|candidate| executable(candidate));
    if resolved.as_deref() != Some(stub) {
        bail!(
            "claude resolves to {resolved:?}, not {}; put the executable room stub first on PATH",
            stub.display()
        );
    }
    Ok(())
}

fn quote(value: &OsStr) -> String {
    format!("'{}'", value.to_string_lossy().replace('\'', "'\\''"))
}

/// The tab number `zellij action go-to-tab` counts from one. A tab carries its own position in
/// `view_id` (`mux/zellij/pane_topology.rs` builds it as `tab_<position>`); the listing groups
/// tabs in first-seen pane order, which is not promised to match, so the index is only the
/// fallback for a tab the mux left unidentified.
fn tab_number(tab: &Tab, index: usize) -> usize {
    tab.view_id
        .as_deref()
        .and_then(|view| view.strip_prefix("tab_"))
        .and_then(|position| position.parse::<usize>().ok())
        .unwrap_or(index)
        + 1
}

/// The command that brings a tab into the client's view, program first. `rimz pane focus` does
/// not move a Zellij client across tabs — only its own `go-to-tab` does — so the backends part
/// here, and the card prints what the room itself ran.
fn look_argv(
    binary: &Path,
    mux: &str,
    session: &str,
    number: usize,
    sidebar: &str,
) -> Vec<OsString> {
    if mux == "zellij" {
        return vec![
            "zellij".into(),
            "--session".into(),
            session.into(),
            "action".into(),
            "go-to-tab".into(),
            number.to_string().into(),
        ];
    }
    vec![
        binary.into(),
        format!("--{mux}").into(),
        "pane".into(),
        "focus".into(),
        sidebar.into(),
    ]
}

fn render_card(
    root: &Path,
    binary: &Path,
    record: &RoomRecord,
    panes: &Panes,
    renderers: &[Renderer],
    snapshot: &Snapshot,
) -> Result<String> {
    let mut card = format!(
        "Sandbox root: {}\nMux: {}  Session: {}\nWorktree: {}\n",
        root.display(),
        record.mux,
        record.session,
        record.worktree.display()
    );
    let sandbox = format!(
        "target/debug/xtask sandbox in {} --",
        quote(root.as_os_str())
    );
    let join = format!("{sandbox} {} --{}", quote(binary.as_os_str()), record.mux);
    for (index, tab) in panes.tabs.iter().enumerate() {
        for pane in &tab.panes {
            if pane.kind == "sidebar" {
                let renderer = renderers
                    .iter()
                    .find(|renderer| renderer.pane_id.as_deref() == Some(&pane.pane_id))
                    .context("sidebar lacks live renderer")?;
                writeln!(
                    card,
                    "Sidebar: {}  Tab: {}  {}",
                    pane.pane_id,
                    tab.name.as_deref().unwrap_or("-"),
                    renderer.role
                )?;
                let id = quote(OsStr::new(&pane.pane_id));
                let look = look_argv(
                    binary,
                    &record.mux,
                    &record.session,
                    tab_number(tab, index),
                    &pane.pane_id,
                )
                .iter()
                .map(|argument| quote(argument))
                .collect::<Vec<_>>()
                .join(" ");
                writeln!(
                    card,
                    "  Look: {sandbox} {look}\n  Capture: {join} pane capture {id}\n  Click: {join} sidebar click {id} 2 \"${{ROOM_ROW:?set ROOM_ROW to the 0-based pipeline row from a fresh capture}}\""
                )?;
            }
            if pane.kind == "agent"
                && let Some(agent) = &pane.agent
            {
                writeln!(card, "Role: {}  {}", agent.handle, pane.pane_id)?;
            }
        }
    }
    let pipeline = snapshot
        .worktree_groups
        .iter()
        .filter_map(|group| group.pipeline.as_ref())
        .find(|pipeline| pipeline.stage == "Build")
        .context("no Build pipeline")?;
    writeln!(
        card,
        "Stage: {}  Owner: {}",
        pipeline.stage,
        pipeline.owner.as_deref().unwrap_or("-")
    )?;
    writeln!(
        card,
        "Flip: {join} teams flip Review 'live check' --team forge"
    )?;
    if record.mux == "tmux" {
        writeln!(
            card,
            "Focus: {sandbox} tmux -S {} display -p '#{{pane_id}}'",
            quote(root.join("runtime/rimz/tmux/server").as_os_str())
        )?;
    } else {
        writeln!(
            card,
            "Focus: {sandbox} zellij --session {} action list-panes -a -j",
            quote(OsStr::new(&record.session))
        )?;
    }
    writeln!(
        card,
        "Run a sidebar's Look first, then capture or click it: an unwatched sidebar can hold a stale frame."
    )?;
    Ok(card)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_refuses_provider_path_that_does_not_resolve_to_stub() {
        let stub = Path::new("/room/bin/claude");
        assert!(verify_stub(OsStr::new("/room/bin:/host/bin"), stub, |_| true).is_ok());
        let error = verify_stub(OsStr::new("/host/bin:/room/bin"), stub, |_| true).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("put the executable room stub first on PATH")
        );
        assert!(verify_stub(OsStr::new("/room/bin:/host/bin"), stub, |path| path != stub).is_err());
    }

    #[test]
    fn room_card_uses_live_roles_and_snapshot_owner() {
        let panes = serde_json::from_str(r##"{"session":"private","tabs":[{"name":"shell","panes":[{"kind":"sidebar","pane_id":"tmux:%1"}]},{"name":"#probe","panes":[{"kind":"sidebar","pane_id":"tmux:%3"},{"kind":"agent","pane_id":"tmux:%4","agent":{"handle":"@coder#probe"}}]}]}"##).unwrap();
        let renderers: Vec<Renderer> = serde_json::from_str(
            r#"[{"role":"producer","pane_id":"tmux:%3"},{"role":"consumer","pane_id":"tmux:%1"}]"#,
        )
        .unwrap();
        let snapshot = serde_json::from_str(
            r#"{"worktree_groups":[{"pipeline":{"stage":"Build","owner":"coder"}}]}"#,
        )
        .unwrap();
        let record = RoomRecord {
            mux: "tmux".into(),
            session: "private".into(),
            repo: "/room".into(),
            worktree: "/room-worktrees/probe".into(),
            stub_dir: "/bin".into(),
        };
        let card = render_card(
            Path::new("/sandbox"),
            Path::new("/dev/rimz"),
            &record,
            &panes,
            &renderers,
            &snapshot,
        )
        .unwrap();
        assert!(card.contains("Sidebar: tmux:%1  Tab: shell  consumer"));
        assert!(card.contains("Sidebar: tmux:%3  Tab: #probe  producer"));
        assert!(card.contains("Stage: Build  Owner: coder"));
        assert!(card.contains("Role: @coder#probe  tmux:%4"));
        assert!(card.contains(
            "target/debug/xtask sandbox in '/sandbox' -- '/dev/rimz' --tmux pane capture 'tmux:%3'"
        ));
        assert!(card.contains("teams flip Review 'live check' --team forge"));
        assert!(card.contains("sidebar click 'tmux:%3' 2"));
        assert!(
            card.contains("tmux -S '/sandbox/runtime/rimz/tmux/server' display -p '#{pane_id}'")
        );
        assert!(card.contains(
            "Look: target/debug/xtask sandbox in '/sandbox' -- '/dev/rimz' '--tmux' 'pane' 'focus' 'tmux:%3'"
        ));
        assert!(card.contains("unwatched sidebar can hold a stale frame"));
    }

    #[test]
    fn zellij_looks_at_a_tab_by_number_because_pane_focus_does_not_move_its_client() {
        let binary = Path::new("/dev/rimz");
        assert_eq!(
            look_argv(binary, "zellij", "room", 3, "zellij:terminal_6"),
            ["zellij", "--session", "room", "action", "go-to-tab", "3"]
        );
        assert_eq!(
            look_argv(binary, "tmux", "room", 3, "tmux:%3"),
            ["/dev/rimz", "--tmux", "pane", "focus", "tmux:%3"]
        );
    }

    #[test]
    fn tab_number_follows_the_view_id_not_the_listing_order() {
        let panes: Panes = serde_json::from_str(
            r##"{"session":"private","tabs":[
                {"view_id":"tab_1","name":"rimzd","panes":[]},
                {"view_id":"tab_0","name":"rimz","panes":[]},
                {"name":"#probe","panes":[]}
            ]}"##,
        )
        .unwrap();
        let numbers: Vec<_> = panes
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| tab_number(tab, index))
            .collect();
        // The first two follow their own position; the unidentified tab falls back to its index.
        assert_eq!(numbers, [2, 1, 3]);
    }
}
