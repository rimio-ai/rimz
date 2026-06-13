use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::source_files::tracked_text_files;

fn is_agent_spend_parser_path(path: &Path, agents_root: &Path) -> bool {
    if !path.starts_with(agents_root) {
        return false;
    }
    if matches!(
        path.file_name().and_then(OsStr::to_str),
        Some("spend.rs" | "transcript_fs.rs")
    ) {
        return true;
    }
    let Ok(relative) = path.strip_prefix(agents_root) else {
        return false;
    };
    relative
        .components()
        .any(|component| component.as_os_str() == OsStr::new("spend"))
}

/// Architectural-invariant greps. Defense in depth: these are shallow string
/// matches, so an aliased import or a macro-generated name will bypass.
/// Treat them as a low-cost trip-wire that pairs with code review and the
/// type system; do not rely on them as the sole enforcement of any rule.
///
/// Each entry is `(needle, allow_predicate, message)`. The needle is split
/// across `concat!` calls so this file does not itself trip its own greps.
pub(crate) fn invariants(root: &Path) -> Result<()> {
    let files = tracked_text_files(root)?;
    ensure_banned_imports(root, &files)?;
    ensure_hook_stdio(root, &files)?;
    ensure_sidebar_renderer_boundaries(root, &files)?;
    ensure_spend_parser_boundaries(root, &files)?;
    ensure_sidebar_library_boundaries(root, &files)?;
    ensure_snapshot_json_writes_stay_in_produce(root, &files)?;
    ensure_diag_writes_stay_in_diag(root, &files)?;
    ensure_sidebar_enrich_folds_before_live_panes(root)?;
    ensure_card_admission_predicate(root)?;
    ensure_config_template_sections(root)?;
    ensure_sidebar_render_runtime_uses_snapshot_clock(root, &files)?;
    ensure_ledger_durability(root, &files)?;
    ensure_participant_identity(root, &files)?;
    ensure_no_core_pane_auto_use(root, &files)?;
    ensure_inline_tests_stay_small(&files)?;
    Ok(())
}

fn is_docs_or_xtask(root: &Path, path: &Path) -> bool {
    path.starts_with(root.join("docs"))
        || path.starts_with(root.join("xtask"))
        || path.extension().and_then(OsStr::to_str) == Some("md")
}

fn ensure_banned_imports(root: &Path, files: &[PathBuf]) -> Result<()> {
    for (needle, message) in [
        (
            concat!("chrono", "::"),
            "workspace crates must use jiff, not chrono",
        ),
        (
            concat!("bytes", "::"),
            "workspace crates must not import bytes",
        ),
        (
            concat!("tokio_util", "::"),
            "workspace crates must not import tokio_util",
        ),
    ] {
        ensure_no_match(files, needle, |path| is_docs_or_xtask(root, path), message)?;
    }
    Ok(())
}

fn ensure_hook_stdio(root: &Path, files: &[PathBuf]) -> Result<()> {
    ensure_no_match(
        files,
        concat!("Stdio", "::", "inherit"),
        |path| {
            path.starts_with(root.join("xtask"))
                || path.extension().and_then(OsStr::to_str) == Some("md")
        },
        "hook subprocess paths must not inherit stdio",
    )
}

fn ensure_sidebar_renderer_boundaries(root: &Path, files: &[PathBuf]) -> Result<()> {
    for needle in [
        "rimz::ledger::atomic",
        "crate::ledger::atomic",
        "rimz::ledger::writer",
        "crate::ledger::writer",
    ] {
        ensure_no_match(
            files,
            needle,
            |path| {
                !path.starts_with(root.join("crates/rimz/src/sidebar_pane"))
                    || path.starts_with(root.join("xtask"))
            },
            "sidebar renderer must not import ledger writer APIs",
        )?;
    }
    Ok(())
}

fn ensure_spend_parser_boundaries(root: &Path, files: &[PathBuf]) -> Result<()> {
    let agents_root = root.join("crates/rimz/src/agents");
    for needle in [
        concat!("::", "atomic"),
        concat!("atomic", "::"),
        concat!("::", "bridge"),
        concat!("bridge", "::"),
        concat!("::", "broker"),
        concat!("broker", "::"),
    ] {
        ensure_no_match(
            files,
            needle,
            |path| !is_agent_spend_parser_path(path, &agents_root),
            "adapter spend parsers are read-only: no ledger writes, bridge, or broker imports",
        )?;
    }
    Ok(())
}

fn ensure_sidebar_library_boundaries(root: &Path, files: &[PathBuf]) -> Result<()> {
    let sidebar_root = root.join("crates/rimz/src/sidebar");
    for needle in [
        concat!("ledger", "::", "writer"),
        concat!("feed_", "store"),
        concat!("::", "bridge"),
        concat!("bridge", "::"),
        concat!("::", "broker"),
        concat!("broker", "::"),
    ] {
        ensure_no_match(
            files,
            needle,
            |path| !path.starts_with(&sidebar_root),
            "crates/rimz/src/sidebar is read-only on the ledger: no writer, feed-store, bridge, or broker imports",
        )?;
    }
    Ok(())
}

fn ensure_ledger_durability(root: &Path, files: &[PathBuf]) -> Result<()> {
    for needle in [concat!(".sync_", "all("), concat!(".sync_", "data(")] {
        ensure_no_match(
            files,
            needle,
            |path| {
                path.ends_with("crates/rimz/src/ledger/atomic.rs") || is_docs_or_xtask(root, path)
            },
            "fsync syscalls live in ledger/atomic.rs alone — route through its helpers",
        )?;
    }
    Ok(())
}

fn ensure_participant_identity(root: &Path, files: &[PathBuf]) -> Result<()> {
    let cli_root = root.join("crates/rimz/src/cli");
    ensure_no_match(
        files,
        concat!("WorkspaceResolver::", "resolve("),
        |path| {
            let participant_cli = path.starts_with(&cli_root)
                && matches!(
                    path.file_name().and_then(OsStr::to_str),
                    Some(
                        "hooks.rs"
                            | "event.rs"
                            | "statusline.rs"
                            | "feed.rs"
                            | "pane.rs"
                            | "sidebar.rs"
                    )
                );
            !participant_cli
        },
        "participant surfaces resolve identity through the session pin — use resolve_participant",
    )
}

fn ensure_no_match(
    files: &[PathBuf],
    needle: &str,
    allow: impl Fn(&Path) -> bool,
    message: &str,
) -> Result<()> {
    let mut violations = Vec::new();
    for path in files {
        if allow(path) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for (idx, line) in text.lines().enumerate() {
            if line.contains(needle) {
                violations.push(format!("{}:{}: {}", path.display(), idx + 1, line.trim()));
            }
        }
    }
    if violations.is_empty() {
        return Ok(());
    }
    bail!("{message}\n{}", violations.join("\n"));
}

fn ensure_sidebar_render_runtime_uses_snapshot_clock(root: &Path, files: &[PathBuf]) -> Result<()> {
    let render_root = root.join("crates/rimz/src/sidebar_pane/render");
    let mut violations = Vec::new();
    for path in files {
        if !path.starts_with(&render_root)
            || path.extension().and_then(OsStr::to_str) != Some("rs")
            || path
                .strip_prefix(&render_root)
                .ok()
                .is_some_and(path_has_tests_component)
            || path
                .file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| name == "tests.rs" || name.ends_with("_tests.rs"))
        {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let mut in_tests = false;
        for (idx, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("mod tests") {
                in_tests = true;
            }
            if in_tests {
                continue;
            }
            if line.contains(concat!("Timestamp", "::", "now()"))
                && !(path.ends_with("crates/rimz/src/sidebar_pane/render/mod.rs")
                    && line.contains(concat!("since: Timestamp", "::", "now()")))
            {
                violations.push(format!("{}:{}: {}", path.display(), idx + 1, line.trim()));
            }
        }
    }
    if violations.is_empty() {
        return Ok(());
    }
    bail!(
        "sidebar render runtime must use the snapshot clock; pass snapshot.now/current frame time instead of Timestamp::now()\n{}",
        violations.join("\n")
    );
}

fn path_has_tests_component(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == OsStr::new("tests"))
}

fn ensure_sidebar_enrich_folds_before_live_panes(root: &Path) -> Result<()> {
    let path = root.join("crates/rimz/src/sidebar/enrich.rs");
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let Some(live_fold) = text
        .find("with_admitted_live_panes(")
        .or_else(|| text.find("with_admitted_live_panes_and_diagnostics("))
        .or_else(|| text.find("with_live_panes("))
    else {
        bail!("sidebar enrich spine must fold a live pane frame through with_live_panes");
    };
    let after_live = &text[live_fold..];
    let mut violations = Vec::new();
    for needle in [
        ".with_project_root(",
        ".with_worktree_roots(",
        ".with_root_class(",
        ".with_agent_context(",
        ".with_subagent_context(",
        ".with_agent_activity(",
        ".drop_dead_agents_with(",
        ".drop_dead_daemon_sessions(",
        ".reap_stale_sessions(",
    ] {
        if let Some(offset) = after_live.find(needle) {
            let line = text[..live_fold + offset]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count()
                + 1;
            violations.push(format!("{}:{}: {}", path.display(), line, needle));
        }
    }
    if violations.is_empty() {
        return Ok(());
    }
    bail!(
        "sidebar enrich rollup/context/liveness folds must stay before with_live_panes\n{}",
        violations.join("\n")
    );
}

fn ensure_card_admission_predicate(root: &Path) -> Result<()> {
    let path = root.join("crates/rimz/src/ledger/snapshot/view/live.rs");
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let Some(live_fold) = text.find("pub fn with_live_panes(") else {
        bail!("sidebar live-pane fold must stay in view/live.rs");
    };
    let after = &text[live_fold..];
    let Some(groups_fold) = after.find("self.worktree_groups =") else {
        bail!("with_live_panes must build worktree groups after card admission");
    };
    let body = &after[..groups_fold];
    if !body.contains("pane_admits_card(pane, exclude).admits()") {
        bail!("with_live_panes must filter rows through pane_admits_card");
    }
    let mut violations = Vec::new();
    for needle in [
        "command_is_sidebar_chrome",
        "pane_is_host",
        "pane.pane_id !=",
        "pane.pane_id ==",
    ] {
        if let Some(offset) = body.find(needle) {
            let line = text[..live_fold + offset]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count()
                + 1;
            violations.push(format!("{}:{}: {}", path.display(), line, needle));
        }
    }
    if violations.is_empty() {
        return Ok(());
    }
    bail!(
        "with_live_panes card-admission filtering must stay behind pane_admits_card\n{}",
        violations.join("\n")
    );
}

fn ensure_config_template_sections(root: &Path) -> Result<()> {
    let path = root.join("crates/rimz/src/config.template.toml");
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let required = [
        "[worktree]",
        "[agents]",
        "[agents.aliases]",
        "[agents.layouts]",
        "[remote_control]",
        "[accounts]",
        "[accounts.usage_limit_usd]",
        "[notifications]",
        "[sidebar]",
        "[sidebar.context]",
        "[sidebar.budget]",
        "[sidebar.attention]",
        "[sidebar.theme]",
        "[sidebar.providers]",
        "[zellij]",
        "[tmux]",
        "[resume]",
    ];
    let missing: Vec<&str> = required
        .into_iter()
        .filter(|section| !text.contains(section))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    bail!(
        "config template is missing required sections: {}",
        missing.join(", ")
    );
}

fn ensure_no_core_pane_auto_use(root: &Path, files: &[PathBuf]) -> Result<()> {
    let allowed_prefixes = [
        root.join("crates/rimz/src/cli/pane.rs"),
        root.join("crates/rimz/src/mux"),
        root.join("crates/rimz/tests"),
        root.join("docs"),
        root.join("xtask"),
    ];
    for needle in [
        concat!("capture", "_pane("),
        concat!("send", "_keys("),
        concat!("send", "_key("),
    ] {
        ensure_no_match(
            files,
            needle,
            |path| {
                allowed_prefixes
                    .iter()
                    .any(|prefix| path.starts_with(prefix))
            },
            "core paths must not auto-use pane capture/send primitives",
        )?;
    }
    Ok(())
}

fn ensure_snapshot_json_writes_stay_in_produce(root: &Path, files: &[PathBuf]) -> Result<()> {
    let producer_root = root.join("crates/rimz/src/sidebar/produce");
    let source_root = root.join("crates/rimz/src");
    let snapshot_file = concat!("snapshot", ".json");
    let write_helper = concat!("write_temp_then_", "rename_cache");
    let mut violations = Vec::new();

    for path in files {
        // Unit-test modules write snapshot.json fixtures legitimately; exempt
        // both the inline-sized sibling (`tests.rs`) and the grown form a large
        // suite splits into (a `tests/` directory of concern modules).
        let in_test_module = path.file_name().and_then(OsStr::to_str) == Some("tests.rs")
            || path.components().any(|part| part.as_os_str() == "tests");
        if path.extension().and_then(OsStr::to_str) != Some("rs")
            || !path.starts_with(&source_root)
            || path.starts_with(&producer_root)
            || in_test_module
        {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for block in function_blocks(&text) {
            if block.body.contains(snapshot_file)
                && (block.body.contains(write_helper)
                    || block.body.contains("std::fs::write")
                    || block.body.contains("fs::write"))
            {
                violations.push(format!(
                    "{}:{}: {}",
                    path.display(),
                    block.line,
                    block.signature.trim()
                ));
            }
        }
    }

    if violations.is_empty() {
        return Ok(());
    }
    bail!(
        "published pane-frame writes belong in sidebar::produce; realtime events must not patch snapshot.json\n{}",
        violations.join("\n")
    );
}

fn ensure_diag_writes_stay_in_diag(root: &Path, files: &[PathBuf]) -> Result<()> {
    let diag_module = root.join("crates/rimz/src/diag.rs");
    for needle in [concat!("diag.log", ".jsonl"), concat!("diag", "-frames")] {
        ensure_no_match(
            files,
            needle,
            |path| path == diag_module.as_path() || is_docs_or_xtask(root, path),
            "diagnostic log paths belong in crates/rimz/src/diag.rs",
        )?;
    }
    Ok(())
}

struct FunctionBlock<'a> {
    line: usize,
    signature: &'a str,
    body: String,
}

fn function_blocks(text: &str) -> Vec<FunctionBlock<'_>> {
    let mut blocks = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut idx = 0;
    while idx < lines.len() {
        let line = lines[idx];
        if !line.contains("fn ") {
            idx += 1;
            continue;
        }

        let start = idx;
        let mut body = String::new();
        let mut depth = 0_i32;
        let mut saw_open = false;
        while idx < lines.len() {
            let current = lines[idx];
            body.push_str(current);
            body.push('\n');
            for ch in current.chars() {
                match ch {
                    '{' => {
                        depth += 1;
                        saw_open = true;
                    }
                    '}' if saw_open => depth -= 1,
                    _ => {}
                }
            }
            idx += 1;
            if saw_open && depth <= 0 {
                break;
            }
        }
        blocks.push(FunctionBlock {
            line: start + 1,
            signature: line,
            body,
        });
    }
    blocks
}

/// An inline `mod tests { … }` past this many lines moves to a sibling
/// `tests.rs` (`#[cfg(test)] mod tests;`) per
/// docs/contributing/rust-conventions.md#tests.
const INLINE_TESTS_MAX_LINES: usize = 500;

fn ensure_inline_tests_stay_small(files: &[PathBuf]) -> Result<()> {
    let mut violations = Vec::new();
    for path in files {
        if path.extension().and_then(OsStr::to_str) != Some("rs") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let lines: Vec<&str> = text.lines().collect();
        // Exact-line match so needles in strings never self-trip; the house
        // shape keeps the tests module last, so its span runs to EOF.
        let Some(start) = lines.iter().position(|line| *line == "mod tests {") else {
            continue;
        };
        let span = lines.len() - start;
        if span > INLINE_TESTS_MAX_LINES {
            violations.push(format!(
                "{}:{}: inline tests module spans {span} lines",
                path.display(),
                start + 1,
            ));
        }
    }
    if violations.is_empty() {
        return Ok(());
    }
    bail!(
        "inline tests modules past {INLINE_TESTS_MAX_LINES} lines move to a sibling tests.rs — see docs/contributing/rust-conventions.md#tests\n{}",
        violations.join("\n")
    );
}

#[cfg(test)]
mod tests;
