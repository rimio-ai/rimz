//! Non-agent process rows: command classification past launchers, sudo, Rimz's
//! supervised agent wrapper, and the agent-kind sniffing for wrapped commands.

use jiff::Timestamp;

use super::row::{ProcessCard, ProcessState, RowCard, SidebarRow};
use crate::pane::PaneRef;

mod classify;

pub use classify::command_agent_kind;
pub(crate) use classify::{
    command_agent_kind_with_comm, command_program_basename, program_label, rimz_exec_worktree_path,
};

/// Whether a pane no agent has bound carries enough identity to render a
/// process row. Foreground display wins, but a spawn command also admits the
/// pane so a single foreground race cannot erase a known pane.
pub(super) fn pane_command_is_known(pane: &PaneRef) -> bool {
    display_command(pane).is_some()
}

/// Worktree path for a pane row: prefer the mux-reported cwd when it is
/// non-empty, then fall back to Rimz's supervised agent wrapper manifest from
/// the spawn command and finally the foreground command. Used by both process
/// rows and the agent-pane ladder so empty-cwd races do not diverge between
/// the two projections.
pub(crate) fn pane_worktree_path(pane: &PaneRef) -> Option<&str> {
    pane.cwd
        .as_deref()
        .filter(|cwd| !cwd.is_empty())
        .or_else(|| {
            pane.spawn_command
                .as_deref()
                .and_then(rimz_exec_worktree_path)
        })
        .or_else(|| pane.command.as_deref().and_then(rimz_exec_worktree_path))
}

pub(super) fn row_from_process(pane: &PaneRef, now: Timestamp) -> SidebarRow {
    let command = display_command(pane);
    let elevated = pane.elevated_agent.as_ref();
    let program = elevated
        .map(|agent| agent.kind.as_str().to_owned())
        .or_else(|| command.map(program_label))
        .unwrap_or_else(|| "process".to_owned());
    let state = if elevated.is_some() {
        ProcessState::Idle
    } else if command.is_some_and(process_is_active) {
        ProcessState::Busy
    } else {
        ProcessState::Idle
    };
    // An active pane anchors its primary line on the shell that owns it (its root
    // process), so the line stays put as commands come and go while the live
    // command rides the second line. An idle pane keeps its foreground program as
    // its one label. Where `/proc` can't name the shell, fall back to the program.
    let name = if elevated.is_some() {
        program.clone()
    } else if state.is_busy() {
        pane.pane_pid
            .and_then(crate::proc::comm)
            .unwrap_or_else(|| program.clone())
    } else {
        program.clone()
    };
    // The command earns a second line only when it adds something past the
    // primary label — the program path trimmed to its basename, arguments
    // verbatim.
    let command_detail = command
        .filter(|_| state.is_busy() || elevated.is_some())
        .map(command_program_basename)
        .filter(|full| *full != name);
    let worktree_path = pane_worktree_path(pane).map(ToOwned::to_owned);
    let foreign_user = elevated.map(|agent| uid_marker(agent.uid));
    SidebarRow {
        id: pane.pane_id.to_string(),
        name,
        pane: Some(pane.clone()),
        worktree_path,
        worktree_branch: None,
        channel: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        last_activity: pane.pane_process_start.unwrap_or(now),
        card: RowCard::Process(ProcessCard {
            state,
            command_detail,
            foreign_user,
            rss_kb: None,
            cpu_pct: None,
            io_bps: None,
        }),
    }
}

pub fn pane_agent_kind(pane: &PaneRef) -> Option<&'static str> {
    pane.spawn_command
        .as_deref()
        .and_then(command_agent_kind)
        .or_else(|| pane.command.as_deref().and_then(command_agent_kind))
        .or_else(|| {
            pane.hosted_agent_kind
                .as_ref()
                .and_then(|kind| crate::agents::descriptor_by_kind(kind.as_str()))
                .map(|descriptor| descriptor.kind)
        })
}

fn display_command(pane: &PaneRef) -> Option<&str> {
    pane.command
        .as_deref()
        .filter(|command| !command.is_empty())
        .or_else(|| {
            pane.spawn_command
                .as_deref()
                .filter(|command| !command.is_empty())
        })
}

fn uid_marker(uid: u32) -> String {
    crate::proc::user_name(uid)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| format!("#{uid}"))
}

/// Whether a `process` pane is doing genuine work — worth the running spinner —
/// rather than sitting idle. Classified by the program it runs (past any
/// `sudo`): bare shells and the interactive TUIs a user just sits in stay
/// quiet; everything else reads as active, so real work never hides as idle
/// chrome. An unknown command is active by default.
pub(crate) fn process_is_active(command: &str) -> bool {
    // A known agent kind (claude/codex) or the shared `node` host is a transient
    // pre-enrichment state that becomes a proper agent row — never animate it as
    // a process.
    if command_agent_kind(command).is_some() {
        return false;
    }
    const IDLE: &[&str] = &[
        // Shells — a bare prompt is presence, not work.
        "zsh", "bash", "fish", "sh", "dash", "ksh", "csh", "tcsh", "nu", "pwsh", "xonsh",
        // The shared agent host before hook enrichment claims the pane.
        "node", // Interactive TUIs the user lives in, not work in flight.
        "vim", "nvim", "vi", "nano", "emacs", "helix", "hx", "less", "more", "most", "man", "top",
        "htop", "btop", "btm", "atop", "lazygit", "gitui", "tig", "k9s",
    ];
    !IDLE.contains(&program_label(command).as_str())
}

/// Whether a pane's foreground command is Rimz's own sidebar — chrome to filter
/// from rows, sibling counts, and view classification, never to render. One
/// predicate so the frame's own-view derivation and the daemon-view fold agree.
pub(crate) fn command_is_sidebar_chrome(command: &str) -> bool {
    program_label(command) == crate::pane::SIDEBAR_CHROME_TITLE
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::ledger::snapshot::testkit::*;

    #[test]
    fn process_activity_follows_the_real_program() {
        // A sudo-wrapped build is real work; an agent host and bare shells are not.
        assert!(process_is_active("sudo npm install -g @openai/codex"));
        assert!(process_is_active("cargo build --release"));
        assert!(!process_is_active("codex"));
        assert!(!process_is_active("sudo codex"));
        assert!(!process_is_active("zsh"));
        assert!(!process_is_active("nvim src/main.rs"));
    }

    #[test]
    fn process_row_carries_the_full_command_only_when_active() {
        // Active: line 2 shows the command; line 1 falls back to the program when
        // `/proc` can't name the owning shell (no pid in tests).
        let active = row_from_process(
            &pane("%1", "sudo npm install -g @openai/codex", "/repo"),
            Timestamp::now(),
        );
        assert!(active.is_process());
        assert_eq!(active.name, "npm");
        assert_eq!(active.process_state(), Some(ProcessState::Busy));
        assert_eq!(
            active
                .as_process()
                .and_then(|process| process.command_detail.as_deref()),
            Some("sudo npm install -g @openai/codex")
        );

        let path_active = row_from_process(
            &pane("%5", "target/debug/xtask install-dev", "/repo"),
            Timestamp::now(),
        );
        assert_eq!(path_active.name, "xtask");
        assert_eq!(
            path_active
                .as_process()
                .and_then(|process| process.command_detail.as_deref()),
            Some("xtask install-dev")
        );

        // Idle shell: one clean line, no detail.
        let idle = row_from_process(&pane("%2", "zsh", "/repo"), Timestamp::now());
        assert_eq!(idle.name, "zsh");
        assert_eq!(idle.process_state(), Some(ProcessState::Idle));
        assert_eq!(
            idle.as_process()
                .and_then(|process| process.command_detail.as_ref()),
            None
        );

        // An active command already equal to its label adds no redundant line.
        let bare = row_from_process(&pane("%3", "cargo", "/repo"), Timestamp::now());
        assert_eq!(bare.process_state(), Some(ProcessState::Busy));
        assert_eq!(
            bare.as_process()
                .and_then(|process| process.command_detail.as_ref()),
            None
        );

        let spawn_only = row_from_process(
            &crate::pane::PaneRef {
                command: None,
                spawn_command: Some("rimz agents exec codex --worktree-path /repo".to_owned()),
                ..pane("%4", "zsh", "/repo")
            },
            Timestamp::now(),
        );
        assert_eq!(spawn_only.name, "codex");
    }

    #[test]
    fn elevated_agent_marker_relabels_only_the_process_row() {
        let mut pane = pane("%4", "sudo su", "/repo");
        pane.elevated_agent = Some(crate::pane::ElevatedAgent {
            kind: crate::ids::AgentKind::new_unchecked("claude"),
            uid: 0,
        });

        let row = row_from_process(&pane, Timestamp::now());

        assert!(row.is_process());
        assert_eq!(row.name, "claude");
        assert_eq!(row.process_state(), Some(ProcessState::Idle));
        let process = row.as_process().expect("process card");
        assert!(
            process
                .foreign_user
                .as_deref()
                .is_some_and(|marker| marker == "root" || marker == "#0"),
            "root should render by name when the platform can resolve it: {process:?}",
        );
        assert_eq!(process.command_detail.as_deref(), Some("sudo su"));
        assert_eq!(
            row.pane.as_ref().and_then(|pane| pane.command.as_deref()),
            Some("sudo su"),
            "the relabel never rewrites pane.command"
        );
    }
}
