//! Non-agent process rows: command classification past launchers and
//! sudo, and the agent-kind sniffing for wrapped commands.

use jiff::Timestamp;

use super::view::{SidebarRow, SidebarRowKind};
use crate::agents::lifecycle::TurnPhase;
use crate::feed::PaneRef;

pub(super) fn row_from_process(pane: &PaneRef) -> SidebarRow {
    let command = pane
        .command
        .as_deref()
        .filter(|command| !command.is_empty());
    let program = command
        .map(program_label)
        .unwrap_or_else(|| "process".to_owned());
    let active = command.is_some_and(process_is_active);
    // An active pane anchors its primary line on the shell that owns it (its root
    // process), so the line stays put as commands come and go while the live
    // command rides the second line. An idle pane keeps its foreground program as
    // its one label. Where `/proc` can't name the shell, fall back to the program.
    let name = if active {
        pane.pane_pid
            .and_then(crate::proc::comm)
            .unwrap_or_else(|| program.clone())
    } else {
        program.clone()
    };
    // The full command earns a second line only when it adds something past the
    // primary label — an active pane whose command isn't already its whole name.
    let command_detail = command
        .filter(|_| active)
        .map(ToOwned::to_owned)
        .filter(|full| *full != name);
    SidebarRow {
        row_kind: SidebarRowKind::Process,
        id: pane.pane_id.to_string(),
        name,
        status: None,
        phase: TurnPhase::Idle,
        pane: Some(pane.clone()),
        request_id: None,
        surface: None,
        task: None,
        prompt: None,
        model: None,
        effort: None,
        context_pct: None,
        context_window: None,
        total_tokens: None,
        todo_done: None,
        todo_total: None,
        context: None,
        worktree_path: pane.cwd.clone(),
        worktree_branch: None,
        last_activity: pane.pane_process_start.unwrap_or_else(Timestamp::now),
        resolver: None,
        options: Vec::new(),
        sub_agents: Vec::new(),
        process_active: active,
        command_detail,
        compacting: false,
        turn_error_label: None,
        rss_kb: pane.rss_kb,
        cpu_pct: pane.cpu_pct,
        io_bps: pane.io_bps,
    }
}

/// Whether a `process` pane is doing genuine work — worth the running spinner —
/// rather than sitting idle. Classified by the program it runs (past any `sudo`):
/// bare shells and the interactive TUIs a user just sits in stay quiet;
/// everything else (a build, a test, a script) reads as active, so real work
/// never hides as idle chrome. An unknown command is active by default.
fn process_is_active(command: &str) -> bool {
    // A known agent kind (claude/codex) or the shared `node` host is a transient
    // pre-enrichment state that becomes a proper agent row — never animate it as a
    // process. Shells and the interactive TUIs a user just sits in stay quiet too;
    // everything else (a build, a test, a script) reads as active work.
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

/// The base name of the program a command runs, seeing past a `sudo` wrapper and
/// through a JS launcher to its script: `npm` for `sudo npm install …`, `codex`
/// for `node /usr/bin/codex`, `cargo` for `/usr/bin/cargo build`. The label a
/// process row shows and the token its agent-kind match keys off.
pub(super) fn program_label(command: &str) -> String {
    basename(effective_program(command)).to_owned()
}

/// The file name of a path-or-bare token (`codex` from `/usr/bin/codex`), or the
/// token itself when it has no path component.
fn basename(token: &str) -> &str {
    std::path::Path::new(token)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(token)
}

/// The program a command names — seeing past a leading `sudo` and its options,
/// and, for a JS launcher (`node`/`npx`), through to the script it runs. So
/// `node /usr/bin/codex` is the codex script, while `sudo npm install -g
/// @openai/codex` is `npm` (an install whose argument is a package, not a program
/// being run). Falls back to the whole command when nothing names a program (a
/// bare `sudo`).
fn effective_program(command: &str) -> &str {
    let mut tokens = command.split_whitespace();
    let Some(mut program) = tokens.next() else {
        return command;
    };
    // Step past a `sudo` wrapper and its options to the wrapped program.
    if basename(program) == "sudo" {
        while let Some(token) = tokens.next() {
            if let Some(flag) = token.strip_prefix('-') {
                // `-u user`, `-g group`, … carry their value in the next token.
                if SUDO_VALUE_FLAGS.contains(&flag) {
                    tokens.next();
                }
            } else if token.contains('=') {
                // `sudo VAR=val cmd` — environment assignments precede the program.
            } else {
                program = token;
                break;
            }
        }
    }
    // A JS launcher runs the script named by its first non-flag argument, so the
    // agent is that script (`node /usr/bin/codex` → codex), not the launcher.
    if LAUNCHERS.contains(&basename(program))
        && let Some(script) = tokens.find(|token| !token.starts_with('-'))
    {
        return script;
    }
    program
}

/// Single-letter `sudo` options that consume the following token as their value,
/// so it is skipped rather than mistaken for the wrapped program.
const SUDO_VALUE_FLAGS: &[&str] = &["u", "g", "h", "p", "C", "U", "r", "t", "T", "R"];

/// JS launchers whose agent identity is the script they run, not the launcher
/// binary — so `node …/codex` reads as codex. A package manager like `npm` is not
/// here: `npm install -g @openai/codex` installs a package, it does not run one.
const LAUNCHERS: &[&str] = &["node", "nodejs", "npx"];

/// The agent kind a command launches, matched against the program it runs (past
/// any `sudo`, and through a `node`/`npx` launcher to its script) — never an
/// install target, so `sudo npm install -g @openai/codex` is an npm process while
/// `codex`, `sudo codex`, and `node /usr/bin/codex` are codex.
pub fn command_agent_kind(command: &str) -> Option<&'static str> {
    let program = program_label(command);
    crate::agents::KNOWN_AGENTS
        .iter()
        .copied()
        .find(|agent| program == *agent)
}

#[cfg(test)]
mod tests {

    use super::*;

    use crate::ledger::snapshot::testkit::*;

    #[test]
    fn classifier_sees_past_sudo_to_the_real_program() {
        // The bug: a `codex` token buried in install args misread the pane as a
        // codex agent. Classification keys off the program, never the arguments.
        assert_eq!(
            command_agent_kind("sudo npm install -g @openai/codex"),
            None
        );
        assert_eq!(program_label("sudo npm install -g @openai/codex"), "npm");
        // A real agent under sudo is still that agent; a bare invocation too.
        assert_eq!(command_agent_kind("sudo codex"), Some("codex"));
        assert_eq!(command_agent_kind("codex --foo"), Some("codex"));
        assert_eq!(command_agent_kind("claude"), Some("claude"));
        // A JS launcher runs its script, so `node …/codex` is codex — the script
        // is the program, unlike npm's install *target*.
        assert_eq!(command_agent_kind("node /usr/bin/codex"), Some("codex"));
        assert_eq!(
            command_agent_kind("node --inspect /usr/bin/codex"),
            Some("codex")
        );
        assert_eq!(program_label("node /usr/bin/codex"), "codex");
        // A bare launcher with no script is just the host (handled as idle `node`).
        assert_eq!(command_agent_kind("node"), None);
        // sudo options, including a value-taking `-u user`, skip to the program.
        assert_eq!(
            program_label("sudo -E -u root npm i -g @openai/codex"),
            "npm"
        );
        assert_eq!(
            command_agent_kind("sudo -u root npm i -g @openai/codex"),
            None
        );
        assert_eq!(
            command_agent_kind("sudo node /usr/bin/codex"),
            Some("codex")
        );
        // A path-qualified program resolves to its basename.
        assert_eq!(program_label("/usr/bin/cargo build"), "cargo");
    }

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
        // Active: line 2 shows the full command; line 1 falls back to the program
        // when `/proc` can't name the owning shell (no pid in tests).
        let active = row_from_process(&pane("%1", "sudo npm install -g @openai/codex", "/repo"));
        assert_eq!(active.row_kind, SidebarRowKind::Process);
        assert_eq!(active.name, "npm");
        assert!(active.process_active);
        assert_eq!(
            active.command_detail.as_deref(),
            Some("sudo npm install -g @openai/codex")
        );

        // Idle shell: one clean line, no detail.
        let idle = row_from_process(&pane("%2", "zsh", "/repo"));
        assert_eq!(idle.name, "zsh");
        assert!(!idle.process_active);
        assert_eq!(idle.command_detail, None);

        // An active command already equal to its label adds no redundant line.
        let bare = row_from_process(&pane("%3", "cargo", "/repo"));
        assert!(bare.process_active);
        assert_eq!(bare.command_detail, None);
    }
}
