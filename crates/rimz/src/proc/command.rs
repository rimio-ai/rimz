//! Pure shell/process command parsing shared outside process consumers.

/// The base name of the program a command runs, seeing past a `sudo` wrapper
/// and through known wrappers to the real command: `npm` for `sudo npm install
/// …`, `codex` for `node /usr/bin/codex`, `opencode` for `bun
/// /usr/bin/opencode`, `codex` for `rimz agents exec codex`, and `cargo` for
/// `/usr/bin/cargo build`.
pub(crate) fn program_label(command: &str) -> String {
    basename(effective_program(command)).to_owned()
}

/// The program at the root of a command, seeing past `sudo` but not through
/// RimZ or JavaScript launchers.
pub(crate) fn root_program_label(command: &str) -> Option<&str> {
    effective_program_and_args(command).map(|(program, _)| basename(program))
}

/// The command with an absolute program path reduced to a basename: `/usr/bin/cargo
/// build` reads as `cargo build`, while relative paths like
/// `target/debug/xtask install-dev` stay verbatim as build-location context.
/// Arguments always stay verbatim. Sees past a `sudo` wrapper, so the wrapped
/// program's own path is the one considered.
pub(crate) fn command_program_basename(command: &str) -> String {
    let Some((program, _)) = effective_program_and_args(command) else {
        return command.to_owned();
    };
    if !std::path::Path::new(program).is_absolute() {
        return command.to_owned();
    }

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
pub(crate) fn basename(token: &str) -> &str {
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

/// Worktree path carried by RimZ's own supervised agent wrapper, when the mux's
/// live pane read has not reported `cwd` yet. This is intentionally narrower
/// than command parsing in general: only the hidden `rimz agents exec <kind>
/// --worktree-path <path>` envelope supplies path truth; opaque wrapper state
/// after that envelope is deliberately ignored.
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
/// RimZ's supervised `agents exec <kind>` wrapper, and, for a JS launcher
/// (`node`/`npx`/`bun`), through to the script it runs.
fn effective_program(command: &str) -> &str {
    effective_program_info(command).program
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EffectiveProgram<'a> {
    pub(crate) program: &'a str,
    pub(crate) from_launcher: bool,
}

pub(crate) fn effective_program_info(command: &str) -> EffectiveProgram<'_> {
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

pub(crate) fn is_launcher(program: &str) -> bool {
    LAUNCHERS.contains(&basename(program))
}

pub(crate) fn agent_script_path_names_kind(script: &str, kind: &str) -> bool {
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
    fn parser_sees_past_sudo_and_javascript_launchers() {
        assert_eq!(program_label("sudo npm install -g @openai/codex"), "npm");
        assert_eq!(
            program_label("codex-aarch64-apple-darwin"),
            "codex-aarch64-apple-darwin"
        );
        assert_eq!(
            effective_program_info("node --inspect /usr/bin/codex"),
            EffectiveProgram {
                program: "/usr/bin/codex",
                from_launcher: true,
            }
        );
        assert_eq!(program_label("node /usr/bin/codex"), "codex");
        assert_eq!(
            effective_program_info("bun /usr/bin/opencode").program,
            "/usr/bin/opencode"
        );
        assert_eq!(program_label("bun /usr/bin/opencode"), "opencode");
        assert_eq!(
            program_label("sudo -E -u root npm i -g @openai/codex"),
            "npm"
        );
        assert_eq!(program_label("/usr/bin/cargo build"), "cargo");
    }

    #[test]
    fn parser_identifies_qwen_node_bundle_script() {
        assert_eq!(
            effective_program_info(
                "/home/u/.local/lib/qwen-code/node/bin/node --expose-gc /home/u/.local/lib/qwen-code/lib/cli.js"
            )
            .program,
            "/home/u/.local/lib/qwen-code/lib/cli.js"
        );
    }

    #[test]
    fn command_program_basename_trims_only_the_program_token() {
        assert_eq!(
            command_program_basename("target/debug/xtask install-dev"),
            "target/debug/xtask install-dev"
        );
        assert_eq!(
            command_program_basename("./target/debug/xtask install-dev"),
            "./target/debug/xtask install-dev"
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
    fn parser_sees_past_rimz_supervised_agent_wrapper() {
        // `rimz agents --worktree` leaves RimZ's supervised wrapper as the
        // pane's root command while the real agent runs underneath it, so the
        // sidebar must classify the pane by the wrapped agent during the
        // startup gap.
        let wrapped = "/home/me/.cargo/bin/rimz agents exec codex --worktree-path /repo/wt";
        assert_eq!(program_label(wrapped), "codex");
        assert_eq!(rimz_exec_worktree_path(wrapped), Some("/repo/wt"));
        assert_eq!(
            effective_program_info(
                "sudo /home/me/.cargo/bin/rimz agents exec codex --request opaque-state"
            )
            .program,
            "codex"
        );
        assert_eq!(
            rimz_exec_worktree_path("/bin/rimz agents exec codex --worktree-path=/repo/wt"),
            Some("/repo/wt")
        );
        assert_eq!(
            rimz_exec_worktree_path(
                "/bin/rimz agents exec codex --worktree-path /repo/wt --request arbitrary-later-state"
            ),
            Some("/repo/wt")
        );
    }
}
