//! Terminal edge for the caller room's live loop panel.

use std::io::IsTerminal;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use rimz::tui::{MouseCapture, Screen, TerminalModeGuard};

use super::*;

pub(super) fn watch(args: WatchArgs, globals: &GlobalFlags) -> Result<()> {
    if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
        bail!("`rimz loop watch` needs a terminal; use `rimz loop list` for one snapshot");
    }
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    if !render::room_open(&workspace.project_root)
        && crate::cli::pinned_room_root().as_ref() != Some(&workspace.project_root)
    {
        bail!(
            "`rimz loop watch` shows an open room's loops; start one with `rimz start`, or use `rimz loop list`"
        );
    }
    let mut follower = rimz::reload::RoomBuildFollower::new(&workspace.workspace_id);
    let mut input = TerminalModeGuard::enable(MouseCapture::Off, Screen::Alternate)?;
    let mut held_stats = run_log::HeldStats::new(&rimz::disk::paths::logs_dir());
    loop {
        if follower.poll_if_due(Instant::now()).is_some() {
            drop(input);
            if let Some(target) = follower.recheck() {
                let error = rimz::reload::exec_current_args(&target);
                tracing::warn!(%error, "loop panel exec failed; keeping current build");
            }
            input = TerminalModeGuard::enable(MouseCapture::Off, Screen::Alternate)?;
        }
        repaint_watch(&workspace, args.hold, &mut held_stats)?;
        match event::poll(Duration::from_secs(1)) {
            Ok(true) => match event::read() {
                Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Char('q') if !args.hold => return Ok(()),
                    KeyCode::Char('c') | KeyCode::Char('C')
                        if key.modifiers.contains(KeyModifiers::CONTROL) && !args.hold =>
                    {
                        return Ok(());
                    }
                    _ => {}
                },
                Ok(_) => {}
                Err(err) => return Err(err.into()),
            },
            Ok(false) => {}
            Err(err) => return Err(err.into()),
        }
    }
}

fn repaint_watch(
    workspace: &rimz::ResolvedWorkspace,
    hold: bool,
    held_stats: &mut run_log::HeldStats,
) -> Result<()> {
    let mut frame = Vec::new();
    list::render_watch_frame(&mut frame, workspace, hold, held_stats)?;
    if frame.last() == Some(&b'\n') {
        frame.pop();
        if frame.last() == Some(&b'\r') {
            frame.pop();
        }
    }
    let mut stdout = std::io::stdout();
    rimz::tui::replace_frame(&mut stdout, &frame)?;
    Ok(())
}
