use crate::agents::program_names_kind;

/// The base name of the program a command runs, seeing past a `sudo` wrapper
/// and through known wrappers to the real command: `npm` for `sudo npm install
/// …`, `codex` for `node /usr/bin/codex`, `opencode` for `bun
/// /usr/bin/opencode`, `codex` for `rimz agents exec codex`, and `cargo` for
/// `/usr/bin/cargo build`.
pub(crate) fn program_label(command: &str) -> String {
    basename(effective_program(command)).to_owned()
}

/// The command with its program token reduced to a basename: the leading path
/// trimmed so `target/debug/xtask install-dev` reads as `xtask install-dev`,
/// while arguments stay verbatim. Sees past a `sudo` wrapper, so the wrapped
/// program's own path is the one trimmed.
pub(crate) fn command_program_basename(command: &str) -> String {
    let Some((program, _)) = effective_program_and_args(command) else {
        return command.to_owned();
    };
    let base = basename(program);
    if base.len() == program.len() {
        return command.to_owned();
    }

    // `program` is a `split_whitespace` token from `command`, so its pointer
    // delta is the byte offset to splice at.
    let start = program.as_ptr() as usize - command.as_ptr() as usize;
    let end = start + program.len();
    let mut out = String::with_capacity(command.len() - (program.len() - base.len()));
    out.push_str(&command[..start]);
    out.push_str(base);
    out.push_str(&command[end..]);
    out
}

/// The file name of a path-or-bare token (`codex` from `/usr/bin/codex`), or
/// the token itself when it has no path component.
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
/// (`node`/`npx`/`bun`), through to the script it runs.
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

/// Single-letter `sudo` options that consume the following token as their
/// value, so it is skipped rather than mistaken for the wrapped program.
const SUDO_VALUE_FLAGS: &[&str] = &["u", "g", "h", "p", "C", "U", "r", "t", "T", "R"];

/// JS launchers whose agent identity is the script they run, not the launcher
/// binary — so `node …/codex` reads as codex and `bun …/opencode` reads as
/// opencode.
const LAUNCHERS: &[&str] = &["node", "nodejs", "npx", "bun"];

/// The agent kind a command launches, matched against the program it runs
/// (past any `sudo`, and through a `node`/`npx`/`bun` launcher to its script)
/// — never an install target.
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
        program_names_kind(label, kind)
            || (program.from_launcher && agent_script_path_names_kind(program.program, kind))
    })
}

fn command_agent_kind_from_comm(comm: &str) -> Option<&'static str> {
    let comm = basename(comm.trim());
    let mut matches = crate::agents::known_kinds().filter(|kind| program_names_kind(comm, kind));
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
        assert_eq!(
            command_agent_kind("codex-aarch64-apple-darwin"),
            Some("codex")
        );
        assert_eq!(
            command_agent_kind("/usr/local/bin/codex-x86_64-apple-darwin"),
            Some("codex")
        );
        assert_eq!(
            program_label("codex-aarch64-apple-darwin"),
            "codex-aarch64-apple-darwin"
        );
        assert_eq!(command_agent_kind("claude"), Some("claude"));
        // A JS launcher runs its script, so `node …/codex` is codex — the script
        // is the program, unlike npm's install *target*.
        assert_eq!(command_agent_kind("node /usr/bin/codex"), Some("codex"));
        assert_eq!(
            command_agent_kind("node --inspect /usr/bin/codex"),
            Some("codex")
        );
        assert_eq!(program_label("node /usr/bin/codex"), "codex");
        assert_eq!(
            command_agent_kind_with_comm("bun /usr/bin/opencode", Some("bun")),
            Some("opencode")
        );
        assert_eq!(program_label("bun /usr/bin/opencode"), "opencode");
        // A bare launcher with no script is just the host.
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
    fn command_program_basename_trims_only_the_program_token() {
        assert_eq!(
            command_program_basename("target/debug/xtask install-dev"),
            "xtask install-dev"
        );
        assert_eq!(
            command_program_basename("/usr/bin/cargo build"),
            "cargo build"
        );
        assert_eq!(
            command_program_basename("cargo build --release"),
            "cargo build --release"
        );
        assert_eq!(
            command_program_basename("sudo /usr/bin/cargo build"),
            "sudo cargo build"
        );
        assert_eq!(
            command_program_basename("sudo -E -u root /usr/bin/npm i -g @openai/codex"),
            "sudo -E -u root npm i -g @openai/codex"
        );
        assert_eq!(
            command_program_basename("cargo run --manifest-path /a/b/Cargo.toml"),
            "cargo run --manifest-path /a/b/Cargo.toml"
        );
        assert_eq!(
            command_program_basename("xtask install-dev"),
            "xtask install-dev"
        );
        assert_eq!(command_program_basename(""), "");
    }

    #[test]
    fn classifier_can_use_precise_proc_comm_as_a_fallback() {
        assert_eq!(
            command_agent_kind_with_comm("", Some("claude")),
            Some("claude")
        );
        assert_eq!(
            command_agent_kind_with_comm("", Some("codex-aarch64-a")),
            Some("codex")
        );
        assert_eq!(command_agent_kind_with_comm("node", Some("node")), None);
        assert_eq!(command_agent_kind_with_comm("bun", Some("bun")), None);
        assert_eq!(
            command_agent_kind_with_comm("bun run dev", Some("bun")),
            None
        );
        assert_eq!(command_agent_kind_with_comm("zsh", Some("zsh")), None);
    }

    #[test]
    fn classifier_sees_past_rimz_supervised_agent_wrapper() {
        // `rimz agents --worktree` leaves Rimz's supervised wrapper as the
        // pane's root command while the real agent runs underneath it, so the
        // sidebar must classify the pane by the wrapped agent during the
        // startup gap.
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
}
