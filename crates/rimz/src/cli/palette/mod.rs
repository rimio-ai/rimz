//! Search the room's published cards and run explicit RimZ commands.

mod launch;
mod model;
mod ui;

#[cfg(test)]
mod tests;

use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Args, ValueEnum};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use rimz::config::MachineConfig;
use rimz::ids::{MuxClientId, MuxName, PaneId};
use rimz::mux::{ClientFocusOptions, ClientView, PopupRequest};
use rimz::sidebar::consumer::{PublishedSnapshotReader, published_frame_exists};
use rimz::tui::{MouseCapture, Screen, TerminalModeGuard};
use rimz::workspace::ResolvedWorkspace;

use super::{Ctx, GlobalFlags};
use launch::LaunchSetup;
use model::{Action, ChildOutcome, Effect, PaletteState, candidates};
use ui::{PaletteTheme, render};

const REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const EVENT_POLL: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum Mode {
    #[default]
    Cards,
    Commands,
    Worktrees,
    New,
}

impl Mode {
    fn from_query(query: &str) -> Self {
        match query.chars().next() {
            Some('>') => Self::Commands,
            Some('#') => Self::Worktrees,
            Some('+') => Self::New,
            _ => Self::Cards,
        }
    }

    fn prefix(self) -> &'static str {
        match self {
            Self::Cards => "",
            Self::Commands => ">",
            Self::Worktrees => "#",
            Self::New => "+",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Cards => "cards",
            Self::Commands => "commands",
            Self::Worktrees => "worktrees",
            Self::New => "new",
        }
    }
}

#[derive(Debug, Args)]
pub(super) struct PaletteArgs {
    /// Seed the query: cards, > commands, # worktrees, or + new agent.
    #[arg(long, value_enum, default_value = "cards")]
    mode: Mode,
    /// Address this room rather than the current directory's room.
    #[arg(long)]
    session_name: Option<String>,
    /// Card to target until the palette selection moves.
    #[arg(long)]
    from_pane: Option<PaneId>,
    /// Open in a centred multiplexer popup.
    #[arg(long)]
    popup: bool,
    /// Attached tmux client to open on (tmux only).
    #[arg(long, requires = "popup")]
    client: Option<String>,
}

pub(super) fn run(args: PaletteArgs, globals: &GlobalFlags) -> Result<()> {
    require_terminal(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        args.popup,
    )?;
    let ctx = match &args.session_name {
        Some(session) => {
            let record = rimz::room::session::workspace_record_for_session(session)?
                .with_context(|| super::no_room(format_args!("for session {session}")))?;
            Ctx::for_workspace(record.workspace_id, globals.mux)?
        }
        None => Ctx::open(globals)?,
    };
    let mux = match &args.session_name {
        Some(session) => super::render::room::present_mux_pick(
            rimz::room::session::pick_mux_for_session(session, globals.mux),
        )?,
        None => rimz::mux::auto_detect_backend(globals.mux.or(ctx.workspace.mux_hint))?,
    };
    let opener = args
        .from_pane
        .clone()
        .or_else(|| rimz::mux::own_pane_id(mux));
    if !args.popup {
        return run_terminal(&ctx, args.mode, mux, opener);
    }
    let backend = rimz::mux::backend_for(mux);
    let mut typing_pane = None;
    let client = match mux {
        MuxName::Tmux => {
            let read_view = || {
                backend.client_view(ClientFocusOptions {
                    session_name: Some(ctx.workspace.session_name.clone()),
                    ..Default::default()
                })
            };
            let view = if args.client.is_none() {
                read_view()?
            } else if cfg!(target_os = "linux") {
                read_view().unwrap_or_else(|error| {
                    tracing::debug!(
                        %error, "cannot read palette client's pane for typing exclusion"
                    );
                    ClientView::default()
                })
            } else {
                ClientView::default()
            };
            let client = popup_client(&ctx.workspace.session_name, args.client.as_deref(), &view)?;
            typing_pane = view
                .clients
                .iter()
                .find(|view| matches!(&view.client_id, MuxClientId::Tmux(name) if *name == client))
                .map(|view| view.pane_id.clone());
            Some(client)
        }
        MuxName::Zellij => {
            if args.client.is_some() {
                bail!("--client is only supported with --mux tmux");
            }
            None
        }
    };
    let request = PopupRequest {
        session_name: ctx.workspace.session_name.clone(),
        program: std::env::current_exe()?.to_string_lossy().into_owned(),
        args: popup_args(&args, &ctx.workspace.session_name, mux, opener.as_ref()),
        cwd: ctx.workspace.project_root.clone(),
        client,
        anchor_pane: opener,
    };
    if let Some(pane) = &typing_pane {
        rimz::mux::exclude_pane_input(ctx.store.runtime_paths(), pane);
    }
    let result = backend.open_popup(&request);
    if let Some(pane) = &typing_pane {
        rimz::mux::exclude_pane_input(ctx.store.runtime_paths(), pane);
    }
    result?;
    Ok(())
}

fn require_terminal(terminal: bool, popup: bool) -> Result<()> {
    if !terminal && !popup {
        bail!("rimz palette needs a terminal; run `rimz palette --popup`");
    }
    Ok(())
}

fn popup_client(session: &str, explicit: Option<&str>, view: &ClientView) -> Result<String> {
    if let Some(client) = explicit {
        return Ok(client.to_owned());
    }
    let mut clients = view
        .clients
        .iter()
        .filter_map(|client| match &client.client_id {
            MuxClientId::Tmux(name) => Some(name),
            MuxClientId::Zellij(_) => None,
        });
    let Some(client) = clients.next() else {
        bail!("no client is attached to session {session}; attach with `rimz attach`");
    };
    if clients.next().is_some() {
        bail!("several clients are attached to session {session}; pass --client <name>");
    }
    Ok(client.clone())
}

fn popup_args(
    args: &PaletteArgs,
    session: &str,
    mux: MuxName,
    opener: Option<&PaneId>,
) -> Vec<String> {
    let mut argv = vec![
        "palette".to_owned(),
        "--mode".to_owned(),
        args.mode.name().to_owned(),
        "--session-name".to_owned(),
        session.to_owned(),
        "--mux".to_owned(),
        mux.to_string(),
    ];
    if let Some(opener) = opener {
        argv.extend(["--from-pane".to_owned(), opener.to_string()]);
    }
    argv
}

fn run_terminal(ctx: &Ctx, mode: Mode, mux: MuxName, opener: Option<PaneId>) -> Result<()> {
    let mut guard = Some(TerminalModeGuard::enable(
        MouseCapture::Off,
        Screen::Alternate,
    )?);
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;
    let config = MachineConfig::load_lenient();
    let theme = PaletteTheme::resolve(&config.theme, rimz::tui::truecolor(), rimz::tui::no_color());
    let mut state = PaletteState::new(mode, opener, &ctx.workspace.session_name);
    let mut reader =
        PublishedSnapshotReader::new(ctx.runtime().clone(), &ctx.workspace.session_name, None);
    let mut launch_loaded = false;
    let mut next_refresh = Instant::now();
    loop {
        if !launch_loaded && state.mode() == Mode::New {
            state = state.with_launch(load_launch(ctx, mux));
            launch_loaded = true;
        }
        if Instant::now() >= next_refresh {
            refresh(&mut state, &mut reader, ctx);
            next_refresh = Instant::now() + REFRESH_INTERVAL;
        }
        terminal.draw(|frame| render(frame, &state, &theme))?;
        if !event::poll(EVENT_POLL)? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return Ok(()),
            KeyCode::Char('c') if control => return Ok(()),
            KeyCode::Up => state.move_selection(-1),
            KeyCode::Down => state.move_selection(1),
            KeyCode::Char('p') if control => state.move_selection(-1),
            KeyCode::Char('n') if control => state.move_selection(1),
            KeyCode::Backspace => state.backspace(),
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                state.type_char(character);
            }
            KeyCode::Enter if state.advance() => {
                let effect = state.selected_action().map(|action| action.effect);
                let show = effect == Some(Effect::Show);
                if show || effect == Some(Effect::Launch) {
                    drop(guard.take());
                }
                state.activate(|action| run_child(ctx, action));
                if show || (effect == Some(Effect::Launch) && state.notice.is_some()) {
                    if let Some(notice) = &state.notice {
                        writeln!(super::render::err(), "{notice}")?;
                    }
                    wait_for_key()?;
                }
                if state.closed {
                    return Ok(());
                }
            }
            _ => {}
        }
    }
}

fn refresh(state: &mut PaletteState, reader: &mut PublishedSnapshotReader, ctx: &Ctx) {
    if !published_frame_exists(ctx.runtime(), &ctx.workspace.session_name) {
        state.refresh(Vec::new(), false);
        return;
    }
    match reader.read(ctx.store.paths()) {
        Ok(snapshot) => state.refresh(candidates(&snapshot), true),
        Err(error) => state.notice = Some(format!("reading the room: {error}")),
    }
}

/// Reads the `+` mode's inputs the first time the palette enters it, so other modes open without
/// loading definitions or listing worktrees.
fn load_launch(ctx: &Ctx, mux: MuxName) -> LaunchSetup {
    let unavailable = rimz::room::require_live_mux(Some(mux), &ctx.workspace)
        .err()
        .map(|error| error.to_string());
    match MachineConfig::load_with_agent_spec_sources() {
        Ok((machine, sources)) => launch_setup(
            &machine,
            &sources,
            &ctx.workspace.project_root,
            unavailable,
            false,
        ),
        Err(error) => LaunchSetup {
            unavailable: Some(format!("loading machine config: {error:#}")),
            ..LaunchSetup::default()
        },
    }
}

/// The `+` mode's inputs: the profiles `rimz agents profiles` lists, and the managed trees when
/// the room's root is a Git repository.
fn launch_setup(
    machine: &MachineConfig,
    sources: &rimz::config::AgentSpecSources,
    project_root: &Path,
    unavailable: Option<String>,
    new_worktree: bool,
) -> LaunchSetup {
    let reports = super::profile_report::available_profiles(
        &machine.agents.profiles,
        &machine.agents.commands,
        sources,
        rimz::config::effective::ProfileScope::Agents,
    );
    let (_, standalone) =
        super::profile_report::partition_team_profiles(reports, &machine.agents.teams);
    let profiles = standalone
        .into_iter()
        .map(|report| launch::Profile {
            name: report.name,
            model: report.model.unwrap_or_default(),
            description: report.description.unwrap_or_default(),
        })
        .collect();
    let trees = rimz::worktree::discover_owned(project_root)
        .ok()
        .map(|owned| {
            owned
                .into_iter()
                .map(|tree| launch::Tree {
                    name: tree.marker.name,
                    path: tree.marker.worktree_path,
                    branch: tree.branch.unwrap_or_default(),
                })
                .collect()
        });
    LaunchSetup {
        profiles,
        root: project_root.to_owned(),
        trees,
        unavailable,
        new_worktree,
    }
}

fn child_command(program: &Path, workspace: &ResolvedWorkspace, args: &[String]) -> Command {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(&workspace.project_root)
        .envs(rimz::workspace::pin_env(
            &workspace.workspace_id,
            &workspace.project_root,
        ));
    command
}

/// A launch runs in the palette's terminal so its prompts can be answered, and without the
/// palette's pane, which placement would otherwise replace.
fn launch_command(program: &Path, workspace: &ResolvedWorkspace, args: &[String]) -> Command {
    let mut command = child_command(program, workspace, args);
    command.env_remove("TMUX_PANE").env_remove("ZELLIJ_PANE_ID");
    command
}

fn run_child(ctx: &Ctx, action: &Action) -> ChildOutcome {
    let outcome = (|| -> Result<_> {
        let program = std::env::current_exe()?;
        let mut command = match action.effect {
            Effect::Launch => launch_command(&program, &ctx.workspace, &action.args),
            Effect::Act | Effect::Show => child_command(&program, &ctx.workspace, &action.args),
        };
        let (status, stderr) = match action.effect {
            Effect::Show | Effect::Launch => (command.status()?, String::new()),
            Effect::Act => {
                let output = command.stdin(Stdio::null()).output()?;
                (
                    output.status,
                    String::from_utf8_lossy(&output.stderr)
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .to_owned(),
                )
            }
        };
        Ok((status, stderr))
    })();
    match outcome {
        Ok((status, _)) if status.success() => ChildOutcome::Success,
        Ok((status, stderr)) => ChildOutcome::Failure(if stderr.is_empty() {
            status.to_string()
        } else {
            format!("{status}: {stderr}")
        }),
        Err(error) => ChildOutcome::Failure(error.to_string()),
    }
}

fn wait_for_key() -> Result<()> {
    writeln!(super::render::out(), "\nPress any key to close")?;
    let _guard = TerminalModeGuard::enable(MouseCapture::Off, Screen::Main)?;
    loop {
        if matches!(event::read()?, Event::Key(key) if key.kind != KeyEventKind::Release) {
            return Ok(());
        }
    }
}
