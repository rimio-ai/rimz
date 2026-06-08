//! Non-agent process rows: command classification past launchers, sudo, Rimz's
//! supervised agent wrapper, and the agent-kind sniffing for wrapped commands.

use jiff::Timestamp;

use super::row::{ProcessCard, ProcessState, RowCard, SidebarRow};
use crate::feed::PaneRef;

/// Whether a pane no agent has bound carries enough identity to render a
/// process row. Foreground display wins, but a spawn command also admits the
/// pane so a single foreground race cannot erase a known pane.
pub(super) fn pane_command_is_known(pane: &PaneRef) -> bool {
    display_command(pane).is_some()
}

/// Worktree path for a pane row: prefer the mux-reported cwd when it is
/// non-empty, then fall back to Rimz's supervised agent wrapper manifest from
/// the spawn command and finally the foreground command. Used by both process
/// rows and the lazy-agent pane ladder so empty-cwd races do not diverge
/// between the two projections.
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
    // The full command earns a second line only when it adds something past the
    // primary label — an active pane whose command isn't already its whole name.
    let command_detail = command
        .filter(|_| state.is_busy() || elevated.is_some())
        .map(ToOwned::to_owned)
        .filter(|full| *full != name);
    let worktree_path = pane_worktree_path(pane).map(ToOwned::to_owned);
    let foreign_user = elevated.map(|agent| uid_marker(agent.uid));
    SidebarRow {
        id: pane.pane_id.to_string(),
        name,
        pane: Some(pane.clone()),
        worktree_path,
        worktree_branch: None,
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
/// rather than sitting idle. Classified by the program it runs (past any `sudo`):
/// bare shells and the interactive TUIs a user just sits in stay quiet;
/// everything else (a build, a test, a script) reads as active, so real work
/// never hides as idle chrome. An unknown command is active by default.
pub(crate) fn process_is_active(command: &str) -> bool {
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
/// through known wrappers to the real command: `npm` for `sudo npm install …`,
/// `codex` for `node /usr/bin/codex`, `codex` for `rimz agents exec codex`, and
/// `cargo` for `/usr/bin/cargo build`. The label a process row shows and the
/// token its agent-kind match keys off.
pub(crate) fn program_label(command: &str) -> String {
    basename(effective_program(command)).to_owned()
}

/// Whether a pane's foreground command is Rimz's own sidebar — chrome to filter
/// from rows, sibling counts, and view classification, never to render. One
/// predicate so the frame's own-view derivation and the daemon-view fold agree.
pub(crate) fn command_is_sidebar_chrome(command: &str) -> bool {
    program_label(command) == "rimz-sidebar"
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

fn effective_program_and_args(command: &str) -> Option<(&str, std::str::SplitWhitespace<'_>)> {
    let mut tokens = command.split_whitespace();
    let mut program = tokens.next()?;
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
    Some((program, tokens))
}

fn rimz_exec_kind<'a>(program: &str, mut tokens: std::str::SplitWhitespace<'a>) -> Option<&'a str> {
    if basename(program) != "rimz" {
        return None;
    }
    (tokens.next() == Some("agents") && tokens.next() == Some("exec"))
        .then(|| tokens.next())
        .flatten()
}

/// Worktree path carried by Rimz's own supervised agent wrapper, when the mux's
/// live pane read has not reported `cwd` yet. This is intentionally narrower
/// than command parsing in general: only the hidden `rimz agents exec <kind>
/// --worktree-path <path>` contract supplies path truth.
pub(crate) fn rimz_exec_worktree_path(command: &str) -> Option<&str> {
    let (program, tokens) = effective_program_and_args(command)?;
    rimz_exec_kind(program, tokens.clone())?;
    let mut tokens = tokens.skip(3);
    while let Some(token) = tokens.next() {
        if let Some(path) = token.strip_prefix("--worktree-path=") {
            return (!path.is_empty()).then_some(path);
        }
        if token == "--worktree-path" {
            return tokens.next().filter(|path| !path.is_empty());
        }
    }
    None
}

/// The program a command names — seeing past a leading `sudo` and its options,
/// Rimz's supervised `agents exec <kind>` wrapper, and, for a JS launcher
/// (`node`/`npx`), through to the script it runs. So `node /usr/bin/codex` is
/// the codex script, while `sudo npm install -g @openai/codex` is `npm` (an
/// install whose argument is a package, not a program being run). Falls back to
/// the whole command when nothing names a program (a bare `sudo`).
fn effective_program(command: &str) -> &str {
    effective_program_info(command).program
}

#[derive(Clone, Copy)]
struct EffectiveProgram<'a> {
    program: &'a str,
    from_launcher: bool,
}

fn effective_program_info(command: &str) -> EffectiveProgram<'_> {
    let Some((program, mut tokens)) = effective_program_and_args(command) else {
        return EffectiveProgram {
            program: command,
            from_launcher: false,
        };
    };
    if let Some(kind) = rimz_exec_kind(program, tokens.clone()) {
        return EffectiveProgram {
            program: kind,
            from_launcher: false,
        };
    }
    // A JS launcher runs the script named by its first non-flag argument, so the
    // agent is that script (`node /usr/bin/codex` → codex), not the launcher.
    if LAUNCHERS.contains(&basename(program))
        && let Some(script) = tokens.find(|token| !token.starts_with('-'))
    {
        return EffectiveProgram {
            program: script,
            from_launcher: true,
        };
    }
    EffectiveProgram {
        program,
        from_launcher: false,
    }
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
    command_agent_kind_with_comm(command, None)
}

pub(crate) fn command_agent_kind_with_comm(
    command: &str,
    comm: Option<&str>,
) -> Option<&'static str> {
    let program = effective_program_info(command);
    command_agent_kind_from_program(program).or_else(|| comm.and_then(command_agent_kind_from_comm))
}

fn command_agent_kind_from_program(program: EffectiveProgram<'_>) -> Option<&'static str> {
    let label = basename(program.program);
    crate::agents::known_kinds().find(|kind| {
        label == *kind
            || (program.from_launcher && agent_script_path_names_kind(program.program, kind))
    })
}

fn command_agent_kind_from_comm(comm: &str) -> Option<&'static str> {
    let comm = basename(comm.trim());
    let mut matches = crate::agents::known_kinds().filter(|kind| {
        crate::agents::descriptor_by_kind(kind).is_some_and(|descriptor| {
            descriptor.process_names.contains(&comm)
                // Launchers are precise only when their cmdline names the agent script.
                && (comm == descriptor.kind || !LAUNCHERS.contains(&comm))
        })
    });
    let kind = matches.next()?;
    matches.next().is_none().then_some(kind)
}

fn agent_script_path_names_kind(script: &str, kind: &str) -> bool {
    std::path::Path::new(script).components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|part| part == kind || part.strip_suffix("-code") == Some(kind))
    })
}

#[cfg(test)]
mod tests {

    use super::*;

    use crate::ledger::snapshot::testkit::*;

    #[test]
    fn pane_command_is_known_requires_a_nonempty_command() {
        // Foreground is the display source, but spawn identity is enough to
        // keep a known pane from disappearing during a raced foreground read.
        assert!(pane_command_is_known(&pane("%1", "zsh", "/repo/main")));
        let raced = crate::feed::PaneRef {
            command: None,
            spawn_command: Some("rimz agents exec codex --worktree-path /repo/main".to_owned()),
            ..pane("%1", "zsh", "/repo/main")
        };
        assert!(pane_command_is_known(&raced));
        let empty = crate::feed::PaneRef {
            command: Some(String::new()),
            spawn_command: Some(String::new()),
            ..pane("%1", "zsh", "/repo/main")
        };
        assert!(!pane_command_is_known(&empty));
    }

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
        assert_eq!(
            command_agent_kind("node /opt/claude/cli.js"),
            Some("claude")
        );
        assert_eq!(
            command_agent_kind("sudo node /opt/node_modules/@anthropic-ai/claude-code/cli.js"),
            Some("claude")
        );
        assert_eq!(command_agent_kind("node /tmp/claude-test/cli.js"), None);
        // A path-qualified program resolves to its basename.
        assert_eq!(program_label("/usr/bin/cargo build"), "cargo");
    }

    #[test]
    fn classifier_can_use_precise_proc_comm_as_a_fallback() {
        assert_eq!(
            command_agent_kind_with_comm("", Some("claude")),
            Some("claude")
        );
        assert_eq!(command_agent_kind_with_comm("node", Some("node")), None);
        assert_eq!(command_agent_kind_with_comm("zsh", Some("zsh")), None);
    }

    #[test]
    fn classifier_sees_past_rimz_supervised_agent_wrapper() {
        // `rimz agents --worktree` leaves Rimz's supervised wrapper as the pane's
        // root command while the real agent runs underneath it, so the sidebar
        // must classify the pane by the wrapped agent during the startup gap.
        let wrapped = "/home/me/.cargo/bin/rimz agents exec codex --worktree-path /repo/wt";
        assert_eq!(program_label(wrapped), "codex");
        assert_eq!(command_agent_kind(wrapped), Some("codex"));
        assert_eq!(rimz_exec_worktree_path(wrapped), Some("/repo/wt"));
        assert_eq!(
            command_agent_kind("sudo /home/me/.cargo/bin/rimz agents exec codex --prompt hi"),
            Some("codex")
        );
        assert_eq!(
            rimz_exec_worktree_path("/bin/rimz agents exec codex --worktree-path=/repo/wt"),
            Some("/repo/wt")
        );
        assert_eq!(command_agent_kind("rimz agents exec unknown"), None);
    }

    #[test]
    fn pane_agent_kind_tries_spawn_before_foreground() {
        let pane = crate::feed::PaneRef {
            command: Some("zsh".to_owned()),
            spawn_command: Some("rimz agents exec codex --worktree-path /repo/wt".to_owned()),
            ..pane("%1", "zsh", "/repo/wt")
        };

        assert_eq!(pane_agent_kind(&pane), Some("codex"));
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
            &crate::feed::PaneRef {
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
        pane.elevated_agent = Some(crate::feed::ElevatedAgent {
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
