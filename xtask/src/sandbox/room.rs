//! Bring up a disposable staged room, print its navigation card, and hold it.

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::io::Write as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::{Agent, DEV_BINARY_MISSING, HostSandbox, Panes, RoomRecord, Tab};

#[derive(Deserialize)]
struct Workspace {
    session_name: String,
}

#[derive(Deserialize)]
struct Team {
    instances: Vec<TeamInstance>,
}

#[derive(Deserialize)]
struct TeamInstance {
    channel: String,
    members: Vec<Agent>,
}

#[derive(Deserialize)]
struct Renderer {
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
        || super::build_dir(workspace).join("rimz"),
        |path| workspace.join(path),
    );
    let binary = binary
        .canonicalize()
        .with_context(|| format!("{DEV_BINARY_MISSING} (or set RIMZ_BIN)"))?;
    let (_sandbox, card) = open(workspace, &binary, mux)?;
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

fn open(workspace: &Path, binary: &Path, mux: &str) -> Result<(HostSandbox, String)> {
    let sandbox = HostSandbox::for_manual_command()?;
    let card = start(workspace, binary.to_path_buf(), mux, &sandbox).map_err(|error| {
        let note = match sandbox.keep(&format!("{error:?}")) {
            Ok(()) => format!(
                "sandbox room failed to start; root kept at {}; remove with rm -rf {}",
                sandbox.root.display(),
                quote(sandbox.root.as_os_str())
            ),
            Err(keep_error) => format!(
                "sandbox room failed to start; could not keep root {}: {keep_error:#}",
                sandbox.root.display()
            ),
        };
        error.context(note)
    })?;
    Ok((sandbox, card))
}

fn start(workspace: &Path, binary: PathBuf, mux: &str, sandbox: &HostSandbox) -> Result<String> {
    let root = &sandbox.root;
    let home = &sandbox.env["HOME"];
    let stub_dir = home.join("bin");
    let path = std::env::join_paths(std::iter::once(stub_dir.clone()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))?;
    let mut room = Room {
        sandbox,
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
    // A launch refuses a provider account whose home is missing or carries no RimZ hooks, and
    // the sandbox `HOME` starts with neither for the stub provider.
    room.rimz("stub provider hooks", &["hooks", "install", "claude"])?;
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
    let client_pane = output(
        "sized client attach",
        room.command("tmux").args([
            "new-session",
            "-d",
            "-P",
            "-F",
            "#{pane_pid}",
            "-x",
            "200",
            "-y",
            "50",
            "-s",
            "room-client",
            &attach,
        ]),
    )?;
    let pane_pid = std::str::from_utf8(&client_pane)?
        .trim()
        .parse()
        .context("parsing client pane PID")?;
    room.wait_for_client(&session, pane_pid)?;
    room.rimz(
        "team launch",
        &["teams", "forge", "-w", "probe", "--isolation", "host"],
    )?;
    room.cwd = home.join("room-worktrees/probe");
    room.wait_for_team()?;
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
        binary: binary.clone(),
    };
    let panes = room.ready()?;
    // Warm every tab, the team's last, so each sidebar has been watched once and hands over
    // at the client's width.
    let mut tabs: Vec<_> = panes.tabs.iter().enumerate().collect();
    tabs.sort_by_key(|(_, tab)| tab.panes.iter().any(|pane| pane.kind == "agent"));
    for (index, tab) in tabs {
        room.look(&record.session, index, tab)?;
    }
    RoomRecord::write(root, &record)?;
    room.card(&record)
}

struct Room<'a> {
    sandbox: &'a HostSandbox,
    binary: &'a Path,
    mux: &'a str,
    path: OsString,
    cwd: PathBuf,
}

impl Room<'_> {
    fn card(&self, record: &RoomRecord) -> Result<String> {
        let snapshot = self.json("card snapshot", &["sidebar", "snapshot", "--json"])?;
        let renderers: Vec<Renderer> =
            self.json("card renderers", &["sidebar", "renderers", "--json"])?;
        // Read the roster last: a pane may have closed while the tabs were warming.
        let panes = self.json("card pane list", &["pane", "list", "--json"])?;
        render_card(
            &self.sandbox.root,
            self.binary,
            record,
            &panes,
            &renderers,
            &snapshot,
        )
    }

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

    /// Resolve the room's name without waiting for client-dependent topology.
    fn session(&self) -> Result<String> {
        let workspace: Workspace = self.json(
            "room session",
            &[
                "workspace",
                "resolve",
                self.cwd.to_str().context("room path is not UTF-8")?,
            ],
        )?;
        Ok(workspace.session_name)
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
        let mut command = self.command(program);
        command.args(args);
        // A fresh room issues focus requests of its own while it settles, and one of them can
        // overtake this visit's `rimz pane focus` before dispatch. That is the room working,
        // not a failure, so visit again rather than abort the room.
        let mut attempt = 1;
        loop {
            let result = command
                .output()
                .with_context(|| format!("visit tab: {command:?}"))?;
            if result.status.success() {
                return Ok(());
            }
            if let Some(delay) = look_retry_delay(self.mux, &result.stderr, attempt) {
                attempt += 1;
                std::thread::sleep(delay);
                continue;
            }
            bail!(
                "visit tab: {command:?} exited with {}: {}",
                result.status,
                String::from_utf8_lossy(&result.stderr)
            );
        }
    }

    /// The client attaches asynchronously, and Zellij materializes a new tab's layout panes
    /// only while one is attached; tmux needs no client, so only Zellij waits.
    fn wait_for_client(&self, session: &str, pane_pid: u32) -> Result<()> {
        if self.mux != "zellij" {
            return Ok(());
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(pid) = client_pid(pane_pid, session) {
                let attached = output(
                    "client list",
                    self.command("zellij")
                        .args(["--session", session, "action", "list-clients"]),
                )
                .is_ok_and(|clients| client_count(&clients) > 0);
                // Reject a bootstrap row or a client that Zellij dropped during the query.
                if attached && client_pid(pane_pid, session) == Some(pid) {
                    return Ok(());
                }
            }
            if Instant::now() >= deadline {
                bail!(
                    "no client attached to {session}; Zellij materializes a new tab's layout panes only for one"
                );
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn wait_for_team(&self) -> Result<()> {
        let team: Team = self.json("team roster", &["teams", "show", "forge#probe", "--json"])?;
        let team = team
            .instances
            .first()
            .context("launched forge#probe cohort is missing")?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let panes = self.json("team pane list", &["pane", "list", "--json"])?;
            let missing = missing_team_members(&panes, team);
            if missing.is_empty() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!(
                    "team panes timed out: missing agent panes in #{} for {}",
                    team.channel,
                    missing.join(", ")
                );
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn ready(&self) -> Result<Panes> {
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
                return Ok(panes);
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

pub(super) fn output(step: &str, command: &mut Command) -> Result<Vec<u8>> {
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

fn client_pid(pane_pid: u32, session: &str) -> Option<u32> {
    std::fs::read_dir("/proc")
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_string_lossy().parse::<u32>().ok())
        .find(|pid| {
            std::fs::read_to_string(format!("/proc/{pid}/comm"))
                .is_ok_and(|comm| comm.trim() == "zellij")
                && std::fs::read(format!("/proc/{pid}/cmdline"))
                    .is_ok_and(|cmdline| is_client_argv(&cmdline, session))
                && descends_from(*pid, pane_pid)
        })
}

fn descends_from(mut pid: u32, ancestor: u32) -> bool {
    while pid != 0 {
        if pid == ancestor {
            return true;
        }
        let Ok(status) = std::fs::read_to_string(format!("/proc/{pid}/status")) else {
            return false;
        };
        let Some(parent) = status
            .lines()
            .find_map(|line| line.strip_prefix("PPid:"))
            .and_then(|parent| parent.trim().parse().ok())
        else {
            return false;
        };
        pid = parent;
    }
    false
}

fn is_client_argv(cmdline: &[u8], session: &str) -> bool {
    let argv: Vec<_> = cmdline.split(|byte| *byte == 0).collect();
    argv.first().is_some_and(|program| {
        Path::new(OsStr::from_bytes(program)).file_name() == Some(OsStr::new("zellij"))
    }) && argv.get(1) == Some(&b"attach".as_slice())
        && argv.iter().skip(2).any(|arg| *arg == session.as_bytes())
        && !argv.contains(&b"--create-background".as_slice())
}

fn client_count(clients: &[u8]) -> usize {
    String::from_utf8_lossy(clients)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .skip(1)
        .count()
}

fn missing_team_members(panes: &Panes, team: &TeamInstance) -> Vec<String> {
    let tab_name = format!("#{}", team.channel);
    let projected: Vec<_> = panes
        .tabs
        .iter()
        .filter(|tab| tab.name.as_deref() == Some(&tab_name))
        .flat_map(|tab| &tab.panes)
        .filter(|pane| pane.kind == "agent")
        .filter_map(|pane| pane.agent.as_ref().map(|agent| agent.handle.as_str()))
        .collect();
    team.members
        .iter()
        .map(|member| format!("{}#{}", member.handle, team.channel))
        .filter(|handle| !projected.contains(&handle.as_str()))
        .collect()
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

pub(super) fn quote(value: &OsStr) -> String {
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

/// How many times a tab visit runs when a newer focus request supersedes it.
const LOOK_ATTEMPTS: usize = 3;

/// The error text of `FocusActionError::Superseded` in `crates/rimz/src/mux/focus_anchor.rs`,
/// which reaches this process only as the `rimz pane focus` child's stderr.
const FOCUS_SUPERSEDED: &str = "focus action was superseded before dispatch";

fn look_retry_delay(mux: &str, stderr: &[u8], attempt: usize) -> Option<Duration> {
    let stderr = String::from_utf8_lossy(stderr);
    if attempt < LOOK_ATTEMPTS && stderr.contains(FOCUS_SUPERSEDED) {
        return Some(Duration::from_millis(200));
    }
    // Zellij #5630: a discovery refusal can miss a live server before dispatching the action.
    if mux == "zellij" && attempt <= 5 && stderr.trim() == "There is no active session!" {
        return Some(Duration::from_millis(100));
    }
    None
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
                renderers
                    .iter()
                    .find(|renderer| renderer.pane_id.as_deref() == Some(&pane.pane_id))
                    .context("sidebar lacks live renderer")?;
                writeln!(
                    card,
                    "Sidebar: {}  Tab: {}",
                    pane.pane_id,
                    tab.name.as_deref().unwrap_or("-"),
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
                let pid = pane
                    .pid
                    .map_or_else(|| "-".to_owned(), |pid| pid.to_string());
                writeln!(card, "Role: {}  {}  pid {pid}", agent.handle, pane.pane_id)?;
                writeln!(
                    card,
                    "  As: target/debug/xtask sandbox in {} --as {} -- {} --{}",
                    quote(root.as_os_str()),
                    quote(OsStr::new(&agent.handle)),
                    quote(binary.as_os_str()),
                    record.mux
                )?;
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
    fn team_projection_waits_for_every_rostered_member_in_its_tab() {
        let team: Team = serde_json::from_str(
            r##"{"instances":[{"channel":"probe","members":[
                {"handle":"@coder"},{"handle":"@reviewer"},{"handle":"@auditor"}
            ]}]}"##,
        )
        .unwrap();
        let team = &team.instances[0];
        let stale: Panes = serde_json::from_str(
            r##"{"tabs":[{"name":"#probe","panes":[
                {"kind":"agent","pane_id":"zellij:terminal_7","agent":{"handle":"@coder#probe"}}
            ]}]}"##,
        )
        .unwrap();
        assert_eq!(
            missing_team_members(&stale, team),
            ["@reviewer#probe", "@auditor#probe"]
        );
        let panes: Panes = serde_json::from_str(
            r##"{"tabs":[{"name":"shell","panes":[
                {"kind":"agent","pane_id":"tmux:%1","agent":{"handle":"@reviewer#probe"}}
            ]},{"name":"#probe","panes":[
                {"kind":"agent","pane_id":"tmux:%2","agent":{"handle":"@coder#probe"}},
                {"kind":"process","pane_id":"tmux:%3","agent":{"handle":"@reviewer#probe"}},
                {"kind":"agent","pane_id":"tmux:%4","agent":{"handle":"@auditor#other"}}
            ]}]}"##,
        )
        .unwrap();
        assert_eq!(
            missing_team_members(&panes, team),
            ["@reviewer#probe", "@auditor#probe"]
        );
        let ready: Panes = serde_json::from_str(
            r##"{"tabs":[{"name":"#probe","panes":[
                {"kind":"agent","pane_id":"tmux:%2","agent":{"handle":"@coder#probe"}},
                {"kind":"agent","pane_id":"tmux:%3","agent":{"handle":"@reviewer#probe"}},
                {"kind":"agent","pane_id":"tmux:%4","agent":{"handle":"@auditor#probe"}}
            ]}]}"##,
        )
        .unwrap();
        assert!(missing_team_members(&ready, team).is_empty());
    }

    #[test]
    fn client_argv_accepts_both_managed_attach_shapes_not_bootstrap_or_other_sessions() {
        for cmdline in [
            &b"/usr/local/bin/zellij\0attach\0--create\0room-abcd\0options\0--mouse-mode\0true\0"[..],
            &b"zellij\0attach\0room-abcd\0options\0--mouse-mode\0true\0"[..],
        ] {
            assert!(is_client_argv(cmdline, "room-abcd"), "{cmdline:?}");
        }
        for cmdline in [
            &b"zellij\0attach\0--create-background\0room-abcd\0"[..],
            &b"zellij\0attach\0room-abcd-other\0options\0"[..],
            &b"zellij\0--session\0room-abcd\0action\0list-clients\0"[..],
            &b"zellij\0--server\0room-abcd\0"[..],
            &b"rimz\0attach\0room-abcd\0"[..],
            &b""[..],
        ] {
            assert!(!is_client_argv(cmdline, "room-abcd"), "{cmdline:?}");
        }
    }

    #[test]
    fn client_count_ignores_header_and_blank_lines() {
        assert_eq!(client_count(b""), 0);
        assert_eq!(
            client_count(b"CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n\n  \n"),
            0
        );
        assert_eq!(
            client_count(b"CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n1 terminal_1 /bin/sh\n"),
            1
        );
        assert_eq!(
            client_count(
                b"\nCLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n\n1 terminal_1 /bin/sh\n2 terminal_2 vim\n\n"
            ),
            2
        );
    }

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
        let panes = serde_json::from_str(r##"{"session":"private","tabs":[{"name":"shell","panes":[{"kind":"sidebar","pane_id":"tmux:%1"}]},{"name":"#probe","panes":[{"kind":"sidebar","pane_id":"tmux:%3"},{"kind":"agent","pane_id":"tmux:%4","pid":4242,"agent":{"handle":"@coder#probe"}}]}]}"##).unwrap();
        let renderers: Vec<Renderer> =
            serde_json::from_str(r#"[{"pane_id":"tmux:%3"},{"pane_id":"tmux:%1"}]"#).unwrap();
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
            binary: "/dev/rimz".into(),
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
        assert!(card.contains("Sidebar: tmux:%1  Tab: shell\n"));
        assert!(card.contains("Sidebar: tmux:%3  Tab: #probe\n"));
        assert!(card.contains("Stage: Build  Owner: coder"));
        assert!(card.contains("Role: @coder#probe  tmux:%4  pid 4242"));
        assert!(card.contains("  As: target/debug/xtask sandbox in '/sandbox' --as '@coder#probe' -- '/dev/rimz' --tmux"));
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

    fn with_ready_room(test: impl FnOnce(&Room<'_>, &RoomRecord)) {
        let sandbox = HostSandbox::for_manual_command().unwrap();
        let cwd = sandbox.env["HOME"].clone();
        let binary = cwd.join("rimz");
        std::fs::write(
            &binary,
            r#"#!/bin/sh
printf '%s\n' "$*" >> queries
case "$*" in
    '--tmux pane list --json') exec cat panes.json ;;
    '--tmux sidebar renderers --json') exec cat renderers.json ;;
    '--tmux sidebar snapshot --json') exec cat snapshot.json ;;
    *) exit 1 ;;
esac
"#,
        )
        .unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(
            cwd.join("panes.json"),
            r##"{"session":"private","tabs":[{"panes":[
                {"kind":"sidebar","pane_id":"tmux:%1"},
                {"kind":"sidebar","pane_id":"tmux:%3"},
                {"kind":"agent","pane_id":"tmux:%4","agent":{"handle":"@coder#probe"}},
                {"kind":"agent","pane_id":"tmux:%5","agent":{"handle":"@reviewer#probe"}}
            ]}]}"##,
        )
        .unwrap();
        std::fs::write(
            cwd.join("renderers.json"),
            r#"[{"pane_id":"tmux:%1"},{"pane_id":"tmux:%3"}]"#,
        )
        .unwrap();
        std::fs::write(
            cwd.join("snapshot.json"),
            r#"{"worktree_groups":[{"pipeline":{"stage":"Build","owner":"coder"}}]}"#,
        )
        .unwrap();
        let record = RoomRecord {
            mux: "tmux".into(),
            session: "private".into(),
            repo: cwd.clone(),
            worktree: cwd.clone(),
            stub_dir: cwd.clone(),
            binary: binary.clone(),
        };
        let room = Room {
            sandbox: &sandbox,
            binary: &binary,
            mux: "tmux",
            path: std::env::var_os("PATH").unwrap(),
            cwd,
        };
        room.ready().unwrap();
        std::fs::write(room.cwd.join("queries"), "").unwrap();
        test(&room, &record);
    }

    #[test]
    fn room_card_refreshes_after_readiness_despite_stale_renderer() {
        with_ready_room(|room, record| {
            std::fs::write(
                room.cwd.join("panes.json"),
                r##"{"session":"private","tabs":[{"panes":[
                    {"kind":"sidebar","pane_id":"tmux:%3"},
                    {"kind":"agent","pane_id":"tmux:%5","agent":{"handle":"@reviewer#probe"}}
                ]}]}"##,
            )
            .unwrap();
            std::fs::write(
                room.cwd.join("snapshot.json"),
                r#"{"worktree_groups":[{"pipeline":{"stage":"Build","owner":"reviewer"}}]}"#,
            )
            .unwrap();
            let card = room.card(record).unwrap();
            assert!(card.contains("Sidebar: tmux:%3"));
            assert!(card.contains("Role: @reviewer#probe"));
            assert!(card.contains("Stage: Build  Owner: reviewer"));
            assert!(!card.contains("tmux:%1"));
            assert!(!card.contains("tmux:%4"));
            assert!(!card.contains("@coder#probe"));
            let queries = std::fs::read_to_string(room.cwd.join("queries")).unwrap();
            assert_eq!(queries.lines().count(), 3);
            assert_eq!(queries.lines().last(), Some("--tmux pane list --json"));
        });
    }

    #[test]
    fn room_card_refuses_renderer_missing_after_readiness() {
        with_ready_room(|room, record| {
            std::fs::write(
                room.cwd.join("renderers.json"),
                r#"[{"pane_id":"tmux:%1"}]"#,
            )
            .unwrap();
            let result = room.card(record);
            assert!(
                result
                    .is_err_and(|error| error.to_string().contains("sidebar lacks live renderer"))
            );
        });
    }

    #[test]
    fn tab_visit_retries_only_known_refusals_with_bounded_delays() {
        let refusal = b"There is no active session!\n";
        for attempt in 1..=5 {
            assert_eq!(
                look_retry_delay("zellij", refusal, attempt),
                Some(Duration::from_millis(100))
            );
        }
        assert_eq!(look_retry_delay("zellij", refusal, 6), None);
        assert_eq!(look_retry_delay("tmux", refusal, 1), None);
        for stderr in [
            &b""[..],
            &b"session permission denied\n"[..],
            &b"There is no active session!\nanother error\n"[..],
        ] {
            assert_eq!(look_retry_delay("zellij", stderr, 1), None);
        }
        for attempt in 1..LOOK_ATTEMPTS {
            assert_eq!(
                look_retry_delay("tmux", FOCUS_SUPERSEDED.as_bytes(), attempt),
                Some(Duration::from_millis(200))
            );
        }
        assert_eq!(
            look_retry_delay("tmux", FOCUS_SUPERSEDED.as_bytes(), LOOK_ATTEMPTS),
            None
        );
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

    #[cfg(target_os = "linux")]
    fn failed_start(action: &str) -> (anyhow::Error, PathBuf) {
        let probe = tempfile::tempdir().unwrap();
        let binary = probe.path().join("rimz");
        std::fs::write(
            &binary,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$HOME\" > \"${{0%/*}}/sandbox-home\"\n{action}\nexit 1\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        let error = open(probe.path(), &binary, "tmux")
            .err()
            .expect("the preflight binary fails");
        let home = std::fs::read_to_string(probe.path().join("sandbox-home")).unwrap();
        let root = Path::new(home.trim()).parent().unwrap().to_path_buf();
        (error, root)
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn failed_room_start_keeps_its_root_and_reports_the_step_error() {
        let (error, root) = failed_start("");
        let root_exists = root.exists();
        let marker = std::fs::read_to_string(root.join("kept"));
        if root_exists {
            std::fs::remove_dir_all(&root).unwrap();
        }

        assert!(root_exists, "a failed room start must leave its root");
        assert!(root.is_absolute());
        let note = error.to_string();
        let first_line = note.lines().next().unwrap();
        assert!(first_line.contains(&root.display().to_string()), "{note}");
        assert!(
            first_line.contains(&format!("rm -rf {}", quote(root.as_os_str()))),
            "{note}"
        );
        assert!(
            error
                .chain()
                .skip(1)
                .any(|cause| cause.to_string().contains("testkit preflight")),
            "{error:?}"
        );
        let marker = marker.unwrap();
        assert!(marker.contains("testkit preflight"), "{marker}");
        assert!(!marker.contains("rm -rf"), "{marker}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn marker_write_failure_reports_the_reason_and_removes_the_root() {
        let (error, root) = failed_start("mkdir \"${HOME%/*}/kept\"");
        let root_exists = root.exists();
        if root_exists {
            std::fs::remove_dir_all(&root).unwrap();
        }

        assert!(
            !root_exists,
            "a failed marker write must not preserve the root"
        );
        let note = error.to_string();
        assert!(note.contains("could not keep root"), "{note}");
        assert!(note.contains(&root.display().to_string()), "{note}");
        assert!(note.contains("Is a directory"), "{note}");
        assert!(
            error
                .chain()
                .skip(1)
                .any(|cause| cause.to_string().contains("testkit preflight")),
            "{error:?}"
        );
    }
}
