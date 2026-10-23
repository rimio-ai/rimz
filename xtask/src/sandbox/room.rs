//! Bring up a disposable staged room, print its navigation card, and hold it.

use std::collections::BTreeSet;
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
            // An id listed for the whole window is an attached client, not a bootstrap or action
            // row, and the same own attach process alive on both sides of it makes that client
            // ours.
            if let Some(pid) = client_pid(pane_pid, session)
                && self.client_stays_listed(session)
                && client_pid(pane_pid, session) == Some(pid)
            {
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

    /// Whether one client id stays listed across `CLIENT_CONFIRM_WINDOW`, the proof
    /// `ZellijBackend::confirm_attached_client` asks of an attached client.
    fn client_stays_listed(&self, session: &str) -> bool {
        let list = || {
            output(
                "client list",
                self.command("zellij")
                    .args(["--session", session, "action", "list-clients"]),
            )
            .map(|clients| client_ids(&clients))
            .unwrap_or_default()
        };
        let mut replies = vec![list()];
        let deadline = Instant::now() + CLIENT_CONFIRM_WINDOW;
        while one_client_in_every(&replies) {
            if Instant::now() >= deadline {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
            replies.push(list());
        }
        false
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

/// Zellij keeps a background-create bootstrap client and a one-shot action client listed for
/// less than this, so only an id listed across it is an attached client.
const CLIENT_CONFIRM_WINDOW: Duration = Duration::from_millis(750);

fn client_ids(clients: &[u8]) -> BTreeSet<u32> {
    String::from_utf8_lossy(clients)
        .lines()
        .filter_map(|line| line.split_whitespace().next()?.parse().ok())
        .collect()
}

fn one_client_in_every(replies: &[BTreeSet<u32>]) -> bool {
    let Some((first, rest)) = replies.split_first() else {
        return false;
    };
    first
        .iter()
        .any(|id| rest.iter().all(|reply| reply.contains(id)))
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
mod tests;
