//! RimZ `allowed-tools` rules in agy's permission grammar, merged into the launch's one
//! `--settings` document as `permissions.allow`. agy's matching was observed on agy 1.5.14:
//! `command(P)` matches P word by word with any trailing arguments, a file target covers a
//! directory's descendants and expands no glob or `~`, and a rule string agy cannot read is
//! accepted and matches nothing, so a rule with no translation is left out, never passed on.

use std::path::Path;

use crate::agents::launch_settings::{merge_settings, union_settings_array};
use crate::agents::skills::{LaunchSettingsArtifact, LaunchSettingsErr};
use crate::config::ToolRule;

/// One rule as an agy `action(target)` string, or `None` where agy has no equivalent. A
/// specifier holding a parenthesis is untranslated, since agy reads a target only up to its
/// closing one.
pub(super) fn translate(rule: &ToolRule) -> Option<String> {
    let rule = rule.to_string();
    let (name, specifier) = match rule.split_once('(') {
        Some((name, rest)) => (name, rest.strip_suffix(')')),
        None => (rule.as_str(), None),
    };
    if specifier.is_some_and(|specifier| specifier.contains(['(', ')'])) {
        return None;
    }
    match (name, specifier) {
        ("Bash", None | Some("*")) => Some("command(*)".to_owned()),
        ("Bash", Some(prefix)) => {
            let prefix = prefix
                .strip_suffix(":*")
                .or_else(|| prefix.strip_suffix(" *"))
                .unwrap_or(prefix)
                .trim();
            // agy reads a `regex:` target as a pattern, not a word prefix.
            (!prefix.is_empty() && !prefix.contains('*') && !prefix.starts_with("regex:"))
                .then(|| format!("command({prefix})"))
        }
        ("Read", None) => Some("read_file(*)".to_owned()),
        ("Edit" | "Write", None) => Some("write_file(*)".to_owned()),
        ("Read", Some(target)) => file_target(target).map(|target| format!("read_file({target})")),
        ("Edit" | "Write", Some(target)) => {
            file_target(target).map(|target| format!("write_file({target})"))
        }
        ("WebFetch", None) => Some("read_url(*)".to_owned()),
        ("WebFetch", Some(specifier)) => specifier
            .strip_prefix("domain:")
            .filter(|host| *host == "*" || is_hostname(host))
            .map(|host| format!("read_url({host})")),
        (name, None) => mcp_target(name).map(|target| format!("mcp({target})")),
        _ => None,
    }
}

/// A Claude path specifier as an agy file target: `//abs` is absolute, any other path is
/// relative to the workspace, and a trailing `/**` is the directory itself. A target that
/// still starts with `/` once its prefix is gone (`.//x`, `///x`) is untranslated: agy
/// would read it as absolute, or as a `//` path that matches nothing.
fn file_target(target: &str) -> Option<String> {
    let target = target.strip_suffix("/**").unwrap_or(target);
    if target.starts_with('~') || target.contains(['*', '?', '[', '{']) {
        return None;
    }
    let (absolute, target) = match target.strip_prefix("//") {
        Some(absolute) => (true, absolute),
        None => (
            false,
            target
                .strip_prefix("./")
                .or_else(|| target.strip_prefix('/'))
                .unwrap_or(target),
        ),
    };
    if target.is_empty() || target.starts_with('/') {
        return None;
    }
    Some(if absolute {
        format!("/{target}")
    } else {
        target.to_owned()
    })
}

/// agy's `read_url` target is a hostname: a port, a scheme, a path, or a wildcard matches
/// nothing there.
fn is_hostname(host: &str) -> bool {
    !host.is_empty()
        && host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

/// `mcp__S__T` as `S/T`, and `mcp__S` or `mcp__S__*` as `S/*`. S is the server name agy
/// knows: the name the agent file and the account's own config give it.
fn mcp_target(name: &str) -> Option<String> {
    let rest = name.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__").unwrap_or((rest, "*"));
    if server.is_empty() || server.contains('*') || tool.is_empty() {
        return None;
    }
    (tool == "*" || !tool.contains('*')).then(|| format!("{server}/{tool}"))
}

pub(super) fn render(
    rules: &[ToolRule],
    (cwd, artifact_dir): (&Path, &Path),
    extra_args: &mut Vec<String>,
    artifact: &mut Option<LaunchSettingsArtifact>,
) -> Result<(), LaunchSettingsErr> {
    let allow: Vec<String> = rules.iter().filter_map(translate).collect();
    if allow.is_empty() {
        return Ok(());
    }
    *artifact = merge_settings(
        cwd,
        artifact_dir,
        extra_args,
        artifact.as_ref(),
        false,
        |object| union_settings_array(object, "permissions", "allow", allow),
    )
    .map_err(|error| match error {
        LaunchSettingsErr::Settings { path, reason } => LaunchSettingsErr::Settings {
            path,
            reason: format!(
                "{reason}; correct that key, or remove the definition's allowed-tools list"
            ),
        },
        error => error,
    })?;
    Ok(())
}
