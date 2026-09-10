//! Wrapper-aware command labels shared by process and agent consumers. Source spans preserve quoted shell commands and flattened process argv; shell scripts select the first non-setup command lexically, without evaluating shell syntax.

use std::ops::Range;

/// The base name of the program a command runs, seeing past a `sudo` wrapper
/// and through known wrappers to the real command: `npm` for `sudo npm install
/// …`, `codex` for `node /usr/bin/codex`, `opencode` for `bun
/// /usr/bin/opencode`, `codex` for `rimz agents exec codex`, and `cargo` for
/// `/usr/bin/cargo build`.
pub(crate) fn program_label(command: &str) -> String {
    basename(effective_program_info(command).program).to_owned()
}

/// Whether pane birth argv still names the live foreground root. A missing or
/// empty foreground is a reporting race; missing birth argv carries no identity.
pub(crate) fn spawn_command_names_live_root(
    command: Option<&str>,
    spawn_command: Option<&str>,
) -> bool {
    let Some(spawn_command) = spawn_command else {
        return false;
    };
    let Some(command) = command.filter(|command| !command.is_empty()) else {
        return true;
    };
    let spawn = effective_program_info(spawn_command);
    program_label(command) == basename(spawn.root_program)
}

/// The command with an absolute program path reduced to a basename: `/usr/bin/cargo
/// build` reads as `cargo build`, while relative paths like
/// `target/debug/xtask install-dev` stay verbatim as build-location context.
/// Arguments always stay verbatim. Sees past a `sudo` wrapper, so the wrapped
/// program's own path is the one considered.
pub(crate) fn command_program_basename(command: &str) -> String {
    let Some(parsed) = effective_program_and_args(command) else {
        return command.to_owned();
    };
    let program = parsed.program.text;
    if !std::path::Path::new(program).is_absolute() {
        return command.to_owned();
    }

    let base = basename(program);
    if base.len() == program.len() {
        return command.to_owned();
    }

    let Range { start, end } = parsed.program.span;
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

/// Raw executable identity for flattened process argv, without wrapper or script projection.
pub(crate) fn argv0_label(command: &str) -> &str {
    basename(command.split_whitespace().next().unwrap_or_default())
}

#[derive(Clone)]
struct Word<'a> {
    text: &'a str,
    span: Range<usize>,
    quoted: bool,
}

#[derive(Clone)]
enum Token<'a> {
    Word(Word<'a>),
    Separator,
}

struct ParsedCommand<'a> {
    program: Word<'a>,
    args: Vec<Word<'a>>,
}

enum Wrapper {
    Prefix {
        value_flags: &'static [&'static str],
        skip_operand: bool,
    },
    Env,
    Shell,
}

const WRAPPERS: &[(&str, Wrapper)] = &[
    (
        "sudo",
        Wrapper::Prefix {
            value_flags: &[
                "-u",
                "-g",
                "-h",
                "-p",
                "-C",
                "-U",
                "-r",
                "-t",
                "-T",
                "-R",
                "--user",
                "--group",
                "--host",
                "--prompt",
                "--close-from",
                "--other-user",
                "--role",
                "--type",
                "--command-timeout",
                "--chroot",
                "--chdir",
            ],
            skip_operand: false,
        },
    ),
    (
        "doas",
        Wrapper::Prefix {
            value_flags: &["-u", "-C"],
            skip_operand: false,
        },
    ),
    (
        "exec",
        Wrapper::Prefix {
            value_flags: &["-a"],
            skip_operand: false,
        },
    ),
    (
        "command",
        Wrapper::Prefix {
            value_flags: &[],
            skip_operand: false,
        },
    ),
    (
        "nohup",
        Wrapper::Prefix {
            value_flags: &[],
            skip_operand: false,
        },
    ),
    (
        "nice",
        Wrapper::Prefix {
            value_flags: &["-n", "--adjustment"],
            skip_operand: false,
        },
    ),
    (
        "time",
        Wrapper::Prefix {
            value_flags: &["-f", "--format", "-o", "--output"],
            skip_operand: false,
        },
    ),
    (
        "timeout",
        Wrapper::Prefix {
            value_flags: &["-s", "--signal", "-k", "--kill-after"],
            skip_operand: true,
        },
    ),
    (
        "stdbuf",
        Wrapper::Prefix {
            value_flags: &["-i", "-o", "-e", "--input", "--output", "--error"],
            skip_operand: false,
        },
    ),
    ("env", Wrapper::Env),
    ("sh", Wrapper::Shell),
    ("bash", Wrapper::Shell),
    ("zsh", Wrapper::Shell),
    ("dash", Wrapper::Shell),
    ("fish", Wrapper::Shell),
];

fn tokenize(source: &str, range: Range<usize>) -> Option<Vec<Token<'_>>> {
    let mut tokens = Vec::new();
    let mut chars = source[range.clone()].char_indices().peekable();
    while let Some((offset, ch)) = chars.next() {
        if ch.is_whitespace() && ch != '\n' {
            continue;
        }
        if matches!(ch, '&' | '|' | ';' | '\n') {
            if matches!(ch, '&' | '|') && chars.peek().is_some_and(|(_, next)| *next == ch) {
                chars.next();
            }
            tokens.push(Token::Separator);
            continue;
        }
        let start = range.start + offset;
        let mut end = start + ch.len_utf8();
        let mut quote = None;
        let mut escaped = false;
        let mut enclosing_close = None;
        let mut current = ch;
        loop {
            if escaped {
                escaped = false;
            } else if current == '\\' && quote != Some('\'') {
                escaped = true;
            } else if quote == Some(current) {
                quote = None;
                enclosing_close.get_or_insert(end);
            } else if quote.is_none() && matches!(current, '\'' | '"') {
                quote = Some(current);
            }
            let Some(&(next_offset, next)) = chars.peek() else {
                break;
            };
            if quote.is_none()
                && !escaped
                && (next.is_whitespace() || matches!(next, '&' | '|' | ';'))
            {
                break;
            }
            chars.next();
            end = range.start + next_offset + next.len_utf8();
            current = next;
        }
        if quote.is_some() || escaped {
            return None;
        }
        let quoted = matches!(ch, '\'' | '"') && enclosing_close == Some(end);
        let span = if quoted {
            start + 1..end - 1
        } else {
            start..end
        };
        tokens.push(Token::Word(Word {
            text: &source[span.clone()],
            span,
            quoted,
        }));
    }
    Some(tokens)
}

fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn effective_program_and_args(command: &str) -> Option<ParsedCommand<'_>> {
    let Some(tokens) = tokenize(command, 0..command.len()) else {
        let text = command.split_whitespace().next()?;
        let start = command.len() - command.trim_start().len();
        return Some(ParsedCommand {
            program: Word {
                text,
                span: start..start + text.len(),
                quoted: false,
            },
            args: Vec::new(),
        });
    };
    unwrap_command(command, &tokens, 0)
}

fn unwrap_command<'a>(
    source: &'a str,
    tokens: &[Token<'a>],
    depth: usize,
) -> Option<ParsedCommand<'a>> {
    let start = tokens
        .iter()
        .position(|token| !matches!(token, Token::Word(word) if is_assignment(word.text)))?;
    let tokens = &tokens[start..];
    let words: Vec<_> = tokens
        .iter()
        .take_while(|token| matches!(token, Token::Word(_)))
        .filter_map(|token| match token {
            Token::Word(word) => Some(word.clone()),
            Token::Separator => None,
        })
        .collect();
    let program = words.first()?.clone();
    let fallback = || ParsedCommand {
        program: program.clone(),
        args: words[1..].to_vec(),
    };
    if depth >= 32 {
        return Some(fallback());
    }
    let Some((_, wrapper)) = WRAPPERS
        .iter()
        .find(|(name, _)| *name == basename(program.text))
    else {
        return Some(fallback());
    };
    let mut index = 1;
    match wrapper {
        Wrapper::Shell => {
            while let Some(word) = words.get(index) {
                if word.text == "--" || !word.text.starts_with(['-', '+']) {
                    break;
                }
                if word.text.starts_with('-')
                    && !word.text.starts_with("--")
                    && word.text.contains('c')
                {
                    let Some(script) = words.get(index + 1) else {
                        break;
                    };
                    let parsed = if script.quoted {
                        tokenize(source, script.span.clone())
                            .and_then(|script| first_script_command(source, &script, depth + 1))
                    } else {
                        first_script_command(source, &tokens[index + 1..], depth + 1)
                    };
                    return Some(parsed.unwrap_or_else(fallback));
                }
                index += 1;
            }
            return Some(fallback());
        }
        Wrapper::Prefix {
            value_flags,
            skip_operand,
        } => {
            while let Some(word) = words.get(index) {
                if word.text == "--" {
                    index += 1;
                    break;
                }
                if !word.text.starts_with('-') {
                    break;
                }
                index += 1 + usize::from(value_flags.contains(&word.text));
            }
            index += usize::from(*skip_operand);
        }
        Wrapper::Env => {
            while let Some(word) = words.get(index) {
                if word.text == "--" {
                    index += 1;
                    break;
                }
                if is_assignment(word.text) {
                    index += 1;
                    continue;
                }
                if !word.text.starts_with('-') {
                    break;
                }
                if matches!(word.text, "-S" | "--split-string") {
                    index += 1;
                    if let Some(script) = words.get(index).filter(|word| word.quoted) {
                        let parsed = tokenize(source, script.span.clone()).and_then(|mut split| {
                            split.extend_from_slice(&tokens[index + 1..]);
                            unwrap_command(source, &split, depth + 1)
                        });
                        return Some(parsed.unwrap_or_else(fallback));
                    }
                    break;
                }
                index += 1 + usize::from(matches!(word.text, "-u" | "--unset" | "-C" | "--chdir"));
            }
        }
    }
    if index >= words.len() {
        return Some(fallback());
    }
    Some(unwrap_command(source, &tokens[index..], depth + 1).unwrap_or_else(fallback))
}

fn first_script_command<'a>(
    source: &'a str,
    tokens: &[Token<'a>],
    depth: usize,
) -> Option<ParsedCommand<'a>> {
    let mut start = 0;
    while start < tokens.len() {
        if let Some(parsed) = unwrap_command(source, &tokens[start..], depth)
            && !matches!(
                parsed.program.text,
                "cd" | "export" | "set" | "source" | "."
            )
        {
            if !parsed.program.quoted
                && (matches!(
                    parsed.program.text,
                    "if" | "while" | "until" | "for" | "case" | "select" | "function"
                ) || parsed
                    .program
                    .text
                    .starts_with(['(', '{', '>', '<', '!', '|']))
            {
                return None;
            }
            return Some(parsed);
        }
        start += tokens[start..]
            .iter()
            .position(|token| matches!(token, Token::Separator))?
            + 1;
    }
    None
}

fn rimz_exec_kind<'a>(parsed: &ParsedCommand<'a>) -> Option<&'a str> {
    if basename(parsed.program.text) != "rimz" {
        return None;
    }
    let mut tokens = parsed.args.iter().map(|word| word.text);
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
    let parsed = effective_program_and_args(command)?;
    rimz_exec_kind(&parsed)?;
    let mut tokens = parsed.args.iter().map(|word| word.text).skip(3);
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EffectiveProgram<'a> {
    pub(crate) program: &'a str,
    pub(crate) root_program: &'a str,
}

pub(crate) fn effective_program_info(command: &str) -> EffectiveProgram<'_> {
    let Some(parsed) = effective_program_and_args(command) else {
        let program = command.split_whitespace().next().unwrap_or(command);
        return EffectiveProgram {
            program,
            root_program: program,
        };
    };
    let program = parsed.program.text;
    if let Some(kind) = rimz_exec_kind(&parsed) {
        return EffectiveProgram {
            program: kind,
            root_program: program,
        };
    }
    // A JS launcher runs the script named by its first non-flag argument, so the
    // agent is that script (`node /usr/bin/codex` → codex), not the launcher.
    if LAUNCHERS.contains(&basename(program))
        && let Some(script) = parsed.args.iter().find(|word| !word.text.starts_with('-'))
    {
        return EffectiveProgram {
            program: script.text,
            root_program: program,
        };
    }
    EffectiveProgram {
        program,
        root_program: program,
    }
}

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
    fn raw_program_identity_does_not_project_wrappers_or_scripts() {
        for (command, raw) in [
            ("/usr/bin/ttyd -p 8200", "ttyd"),
            ("sh -c ttyd -p 8200", "sh"),
            ("env X=1 ttyd -p 8200", "env"),
            ("node /usr/bin/codex", "node"),
            ("", ""),
        ] {
            assert_eq!(argv0_label(command), raw);
        }
    }

    #[test]
    fn parser_unwraps_prefix_wrappers() {
        for command in [
            "env RUST_LOG=debug cargo build",
            "env -i -u HOME -C '/repo dir' A=b cargo build",
            "env -uHOME -C/repo -- A=b cargo build",
            "sudo -E -u root A=b cargo build",
            "sudo --user root cargo build",
            "doas -u root cargo build",
            "exec -a worker cargo build",
            "command -p -- cargo build",
            "nohup cargo build",
            "nice -n 5 cargo build",
            "nice -n5 cargo build",
            "nice --adjustment=5 cargo build",
            "time -o timings cargo build",
            "timeout --foreground -s TERM -k 2s 10s cargo build",
            "timeout --signal=TERM --kill-after=2s 10s cargo build",
            "stdbuf -oL -e 0 --input=0 cargo build",
            "sudo -u root env X=1 nohup nice -n 5 timeout 10s command exec cargo build",
        ] {
            assert_eq!(
                effective_program_info(command),
                EffectiveProgram {
                    program: "cargo",
                    root_program: "cargo"
                },
                "{command}"
            );
        }
        for command in [
            "env -S 'node /usr/bin/codex'",
            "env -S node /usr/bin/codex",
            "/usr/bin/env node /usr/bin/codex",
        ] {
            assert_eq!(
                effective_program_info(command),
                EffectiveProgram {
                    program: "/usr/bin/codex",
                    root_program: "node"
                },
                "{command}"
            );
            assert_eq!(program_label(command), "codex", "{command}");
        }
        assert_eq!(program_label("env ./a=b arg"), "a=b");
    }

    #[test]
    fn parser_selects_shell_script_command() {
        for (command, label) in [
            ("sh -c cargo build && echo ok", "cargo"),
            ("sh -c 'cargo build && echo ok'", "cargo"),
            ("sh -c 'cd /repo && cargo build'", "cargo"),
            ("bash -c \"cd x; make\"", "make"),
            (
                "sh -c 'export A=1; set -e; source setup; . setup; A=1 B=2; exec cd x; command cargo build'",
                "cargo",
            ),
            ("sh -c 'cd \"a; b && c\"; A=\"x y\" cargo build'", "cargo"),
            ("sh -c 'cd a\\;b; cargo build'", "cargo"),
            ("sh -c 'echo ok; cargo build'", "echo"),
            ("sh -c 'codex' extra", "codex"),
            ("sh script.sh -c cargo", "sh"),
            ("sh -- -c cargo", "sh"),
            ("bash", "bash"),
            ("bash -l", "bash"),
            ("sh -c ''", "sh"),
            ("sh -c 'cd x; export A=1; A=2'", "sh"),
            ("sh -c '(cd /repo && cargo build)'", "sh"),
            ("sh -c '{ cargo build; }'", "sh"),
            ("sh -c 'exec > /tmp/log; cargo build'", "sh"),
            ("sh -c '! cargo build'", "sh"),
            ("sh -c 'while true; do sleep 1; done'", "sh"),
            ("sh -c 'if [ -f x ]; then cargo build; fi'", "sh"),
            ("sh -c '\"while\" arg'", "while"),
        ] {
            assert_eq!(program_label(command), label, "{command}");
        }
        for shell in ["sh", "bash", "zsh", "dash", "fish"] {
            assert_eq!(
                program_label(&format!("{shell} -ec 'cargo build'")),
                "cargo"
            );
        }
        for separator in ["&&", "||", ";", "|", "\n"] {
            assert_eq!(
                program_label(&format!("sh -c 'cd x{separator}cargo build'")),
                "cargo"
            );
        }
        for command in [
            "bash -lc \"exec node /usr/bin/codex\"",
            "sh -c 'env X=1 bash -lc \"exec node /usr/bin/codex\"'",
        ] {
            assert_eq!(
                effective_program_info(command),
                EffectiveProgram {
                    program: "/usr/bin/codex",
                    root_program: "node"
                },
                "{command}"
            );
        }
    }

    #[test]
    fn basename_splicing_preserves_source() {
        for (command, shortened) in [
            ("env A=b /usr/bin/cargo build", "env A=b cargo build"),
            ("sh -c '/usr/bin/cargo build'", "sh -c 'cargo build'"),
            (
                "sh -c 'cd /usr/bin/cargo && /usr/bin/cargo build'",
                "sh -c 'cd /usr/bin/cargo && cargo build'",
            ),
            (
                "env NOTE=🦀 /usr/bin/cargo build",
                "env NOTE=🦀 cargo build",
            ),
            (
                "sh -c 'target/debug/xtask install-dev'",
                "sh -c 'target/debug/xtask install-dev'",
            ),
            ("env X=1 node /usr/bin/codex", "env X=1 node /usr/bin/codex"),
            (
                "env -S '\"/usr/bin/cargo\" build'",
                "env -S '\"cargo\" build'",
            ),
        ] {
            assert_eq!(command_program_basename(command), shortened, "{command}");
        }
    }

    #[test]
    fn worktree_path_survives_wrappers() {
        for (command, path) in [
            (
                "env X=1 rimz agents exec codex --worktree-path /repo",
                Some("/repo"),
            ),
            (
                "sh -c 'cd /tmp && rimz agents exec codex --worktree-path=/repo'",
                Some("/repo"),
            ),
            (
                "sh -c rimz agents exec codex --worktree-path /repo && echo ok",
                Some("/repo"),
            ),
            (
                "sh -c 'rimz agents exec codex --worktree-path \"/repo dir\"'",
                Some("/repo dir"),
            ),
            ("sh -c 'rimz agents exec codex --worktree-path'", None),
            ("sh -c 'rimz agents exec codex --worktree-path='", None),
            ("sh -c 'rimz agents exec codex --worktree-path \"\"'", None),
            (
                "sh -c 'rimz agents exec codex; echo --worktree-path /wrong'",
                None,
            ),
        ] {
            assert_eq!(rimz_exec_worktree_path(command), path, "{command}");
        }
    }

    #[test]
    fn parser_keeps_incomplete_wrappers() {
        for (command, label) in [
            ("env -u", "env"),
            ("sudo -u root", "sudo"),
            ("timeout 10s", "timeout"),
            ("sh -c", "sh"),
            ("sh -c 'cargo build", "sh"),
            ("", ""),
            ("A=1; cargo build", "A=1;"),
            ("; cargo build", ";"),
        ] {
            assert_eq!(program_label(command), label, "{command}");
        }
        assert_eq!(
            program_label(&format!("{}cargo build", "env ".repeat(40))),
            "env"
        );
    }

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
                root_program: "node",
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
    fn spawn_identity_requires_its_root_to_remain_live() {
        let spawn = Some("/bin/rimz agents exec claude --worktree-path /repo");

        assert!(spawn_command_names_live_root(None, spawn));
        assert!(spawn_command_names_live_root(Some(""), spawn));
        assert!(spawn_command_names_live_root(Some("rimz"), spawn));
        assert!(!spawn_command_names_live_root(Some("zsh"), spawn));
        assert!(!spawn_command_names_live_root(Some("rimz"), None));
        for (spawn, live) in [
            ("env X=1 sh -c '/bin/rimz agents exec codex'", "rimz"),
            ("env X=1 node /usr/bin/codex", "node"),
            ("sh -c 'codex'", "codex"),
        ] {
            assert!(spawn_command_names_live_root(Some(live), Some(spawn)));
            assert!(!spawn_command_names_live_root(Some("sh"), Some(spawn)));
            assert!(!spawn_command_names_live_root(Some("zsh"), Some(spawn)));
        }
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
