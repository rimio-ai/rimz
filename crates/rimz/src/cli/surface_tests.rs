//! The binary's compatibility surface, pinned as a snapshot.
//!
//! RimZ ships as a binary. What users and their scripts depend on is the
//! command tree — subcommand names and aliases, flag names, arity, defaults,
//! and accepted values — not the Rust API of the `rimz` library crate, which
//! exists so the binary, tests, and benches can link the domain modules.
//! `cargo semver-checks` reads that library surface and is blind to this one,
//! so this snapshot is the gate that fails when the shipped contract moves.
//!
//! A diff here is the review prompt: an added command or optional flag is
//! additive, while a renamed or removed flag, a narrowed value set, or a
//! changed default breaks callers and belongs in `CHANGELOG.md`. Accept the
//! new snapshot deliberately (`cargo insta accept`) once the diff reads the
//! way the change intends.
//!
//! Help prose stays out on purpose: wording churn would bury the structural
//! diff that actually matters.

use super::*;

use clap::builder::PossibleValue;
use clap::{Arg, ArgAction, Command};

/// Render one command and its descendants as sorted, prose-free lines.
///
/// Sorting by name keeps the snapshot stable against declaration-order edits,
/// so a diff only appears when the surface itself moves.
fn render_command(cmd: &Command, path: &str, out: &mut String) {
    let mut header = path.to_owned();
    let mut aliases: Vec<&str> = cmd.get_all_aliases().collect();
    aliases.sort_unstable();
    if !aliases.is_empty() {
        header.push_str(&format!(" (aliases: {})", aliases.join(", ")));
    }
    if cmd.is_hide_set() {
        header.push_str(" [hidden]");
    }
    out.push_str(&header);
    out.push('\n');

    let mut args: Vec<&Arg> = cmd.get_arguments().collect();
    args.sort_by_key(|arg| arg_sort_key(arg));
    for arg in args {
        out.push_str("    ");
        out.push_str(&render_arg(arg));
        out.push('\n');
    }

    let mut subs: Vec<&Command> = cmd.get_subcommands().collect();
    subs.sort_by_key(|sub| sub.get_name().to_owned());
    for sub in subs {
        out.push('\n');
        render_command(sub, &format!("{path} {}", sub.get_name()), out);
    }
}

/// Positionals sort ahead of options so a command's shape reads argv-order.
fn arg_sort_key(arg: &Arg) -> (u8, String) {
    if arg.is_positional() {
        (0, arg.get_id().to_string())
    } else {
        (1, arg.get_id().to_string())
    }
}

fn render_arg(arg: &Arg) -> String {
    let num_args = arg.get_num_args().unwrap_or_else(|| (0..=0).into());
    let takes_value = num_args.max_values() > 0;

    let mut line = String::new();
    if arg.is_positional() {
        line.push_str(&format!("<{}>", arg.get_id()));
    } else {
        let mut names = Vec::new();
        if let Some(short) = arg.get_short() {
            names.push(format!("-{short}"));
        }
        if let Some(long) = arg.get_long() {
            names.push(format!("--{long}"));
        }
        line.push_str(&names.join(", "));
        // clap keeps a placeholder value name on boolean flags; printing it
        // would read as though `--json` accepted an argument.
        if takes_value && let Some(values) = arg.get_value_names() {
            let rendered: Vec<String> = values.iter().map(|name| format!("<{name}>")).collect();
            if !rendered.is_empty() {
                line.push(' ');
                line.push_str(&rendered.join(" "));
            }
        }
    }

    let mut notes = Vec::new();
    notes.push(format!("action={}", render_action(arg.get_action())));
    let max = if num_args.max_values() == usize::MAX {
        "inf".to_owned()
    } else {
        num_args.max_values().to_string()
    };
    notes.push(format!("takes={}..={max}", num_args.min_values()));
    if arg.is_required_set() {
        notes.push("required".to_owned());
    }
    if arg.is_global_set() {
        notes.push("global".to_owned());
    }
    if arg.is_hide_set() {
        notes.push("hidden".to_owned());
    }
    // A boolean flag's default is implied by its action; only a value-taking
    // arg has a default worth pinning, because changing it changes behaviour
    // for a caller who omits the flag.
    if takes_value {
        let defaults: Vec<String> = arg
            .get_default_values()
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect();
        if !defaults.is_empty() {
            notes.push(format!("default={}", defaults.join(",")));
        }
    }
    // Boolean flags carry an implicit `true|false` value set that says nothing
    // about the surface; only a real value enumeration is worth pinning.
    if !matches!(arg.get_action(), ArgAction::SetTrue | ArgAction::SetFalse) {
        let possible: Vec<String> = arg
            .get_possible_values()
            .iter()
            .map(PossibleValue::get_name)
            .map(ToOwned::to_owned)
            .collect();
        if !possible.is_empty() {
            notes.push(format!("values={}", possible.join("|")));
        }
    }
    if let Some(aliases) = arg.get_all_aliases() {
        let mut aliases: Vec<String> = aliases.iter().map(ToString::to_string).collect();
        aliases.sort();
        if !aliases.is_empty() {
            notes.push(format!("long-aliases={}", aliases.join(",")));
        }
    }

    format!("{line}  [{}]", notes.join(" "))
}

fn render_action(action: &ArgAction) -> &'static str {
    match action {
        ArgAction::Set => "set",
        ArgAction::Append => "append",
        ArgAction::SetTrue => "set-true",
        ArgAction::SetFalse => "set-false",
        ArgAction::Count => "count",
        ArgAction::Help => "help",
        ArgAction::HelpShort => "help-short",
        ArgAction::HelpLong => "help-long",
        ArgAction::Version => "version",
        _ => "other",
    }
}

/// The shipped command tree. Regenerate deliberately; see the module header.
#[test]
fn cli_surface_is_stable() {
    let mut cmd = <Cli as CommandFactory>::command();
    // `build` resolves derived arity, defaults, and propagated global flags, so
    // the snapshot records what argv parsing actually accepts rather than the
    // partially-populated builder state.
    cmd.build();
    let mut rendered = String::new();
    render_command(&cmd, "rimz", &mut rendered);
    insta::assert_snapshot!("cli_surface", rendered);
}
