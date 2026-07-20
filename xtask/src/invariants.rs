use std::ffi::OsStr;
use std::fs;
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
    if relative == Path::new("adapters/plugin/probes.rs") {
        return true;
    }
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
    ensure_normalized_agent_process_decisions(root, &files)?;
    ensure_private_agent_adapter_boundary(root, &files)?;
    ensure_adapter_kinds_stay_typed(root, &files)?;
    ensure_sidebar_renderer_boundaries(root, &files)?;
    ensure_spend_parser_boundaries(root, &files)?;
    ensure_spending_walker_ownership(root, &files)?;
    ensure_agents_do_not_depend_on_sidebar(root, &files)?;
    ensure_sidebar_library_boundaries(root, &files)?;
    ensure_sidebar_enrich_projection_only(root, &files)?;
    ensure_no_zellij_runtime_list_panes(root, &files)?;
    ensure_sidebar_event_log_reads_through_rollup(root, &files)?;
    ensure_snapshot_json_writes_stay_in_produce(root, &files)?;
    ensure_snapshot_projection_stays_quiet(root, &files)?;
    ensure_diag_writes_stay_in_diag(root, &files)?;
    ensure_sidebar_enrich_folds_before_live_panes(root)?;
    ensure_card_admission_predicate(root)?;
    ensure_config_template_sections(root)?;
    ensure_sidebar_render_runtime_uses_snapshot_clock(root, &files)?;
    ensure_no_hardcoded_ui_colors(root, &files)?;
    ensure_cli_color_provenance(root, &files)?;
    ensure_brand_resolution_single_home(root, &files)?;
    ensure_no_hardcoded_glyphs(root, &files)?;
    ensure_presence_plugin_vendored(root)?;
    ensure_store_durability(root, &files)?;
    ensure_participant_identity(root, &files)?;
    ensure_no_core_pane_auto_use(root, &files)?;
    ensure_managed_tmux_endpoint(root, &files)?;
    ensure_inline_tests_stay_small(&files)?;
    Ok(())
}

/// Every managed tmux command addresses the RimZ-owned socket.
///
/// A bare `tmux` argv inherits the user's default server, where RimZ owns no
/// session and where its server-global options and root key bindings would
/// land on the user's own rooms. `mux::tmux::managed_cmd` is the seam for
/// readers outside the backend.
fn ensure_managed_tmux_endpoint(root: &Path, files: &[PathBuf]) -> Result<()> {
    let src_root = root.join("crates/rimz/src");
    // `tmux.rs` builds the managed argv; `presence.rs` spawns the control
    // client with its own explicit `-S`; `uninstall.rs` deliberately asks the
    // ambient server "which session am I in", which must follow `$TMUX`.
    let exempt = [
        src_root.join("mux/tmux.rs"),
        src_root.join("mux/tmux/presence.rs"),
        src_root.join("cli/uninstall.rs"),
    ];
    let mut violations = Vec::new();
    for path in files {
        if !path.starts_with(&src_root)
            || exempt.iter().any(|allowed| path == allowed)
            || is_test_source_path(root, path)
        {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for (idx, line) in lines_outside_inline_tests(&text) {
            if line.contains(r#"CommandSpec::new("tmux")"#)
                || line.contains(r#"Command::new("tmux")"#)
            {
                violations.push(format!("{}:{}: {}", path.display(), idx, line.trim()));
            }
        }
    }
    if violations.is_empty() {
        return Ok(());
    }
    bail!(
        "tmux commands must address the RimZ-owned server; use `mux::tmux::managed_cmd()` \
         instead of a bare `tmux` argv:\n{}",
        violations.join("\n")
    );
}

fn ensure_private_agent_adapter_boundary(root: &Path, files: &[PathBuf]) -> Result<()> {
    let crate_root = root.join("crates/rimz");
    let agents_root = crate_root.join("src/agents");
    let provider_names = [
        "amp",
        "antigravity",
        "claude",
        "codex",
        "copilot",
        "cursor",
        "droid",
        "grok",
        "kimi",
        "kiro",
        "opencode",
        "pi",
        "qwen",
    ];
    let concrete_types = [
        "AmpAdapter",
        "AntigravityAdapter",
        "ClaudeAdapter",
        "CodexAdapter",
        "CopilotAdapter",
        "CursorAdapter",
        "DroidAdapter",
        "GrokAdapter",
        "KimiAdapter",
        "KiroAdapter",
        "OpencodeAdapter",
        "PiAdapter",
        "QwenAdapter",
    ];
    let removed_api = [
        concat!("Agent", "Adapter"),
        concat!("Agent", "Descriptor"),
        concat!("Decoded", "Hook"),
        concat!("Integration", "Coverage"),
        concat!("Lifecycle", "Coverage"),
    ];
    let mut offenders = Vec::new();
    for path in files.iter().filter(|path| {
        path.starts_with(&crate_root)
            && path.extension().and_then(OsStr::to_str) == Some("rs")
            && !path.starts_with(&agents_root)
    }) {
        let source = fs::read_to_string(path)
            .with_context(|| format!("read invariant source {}", path.display()))?;
        let code = code_without_comments(&source);
        let private_path = code.contains(concat!("agents", "::adapters"))
            || provider_names
                .iter()
                .any(|provider| code.contains(&format!("agents::{provider}::")));
        let concrete_type = concrete_types.iter().any(|name| code.contains(name));
        let removed = removed_api.iter().any(|name| code.contains(name));
        if private_path || concrete_type || removed {
            offenders.push(path.display().to_string());
        }
    }
    if !offenders.is_empty() {
        bail!(
            "agent consumers must resolve AgentDefinition and use provider-neutral services\n{}",
            offenders.join("\n")
        );
    }
    Ok(())
}

fn ensure_adapter_kinds_stay_typed(root: &Path, files: &[PathBuf]) -> Result<()> {
    let cli_root = root.join("crates/rimz/src/cli");
    let mut offenders = Vec::new();
    for path in files.iter().filter(|path| {
        *path == &cli_root.join("supervised.rs")
            || *path == &cli_root.join("supervised/run.rs")
            || path.starts_with(cli_root.join("hooks/lifecycle"))
    }) {
        let source = fs::read_to_string(path)
            .with_context(|| format!("read invariant source {}", path.display()))?;
        let compact = code_without_comments(&source)
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        let mut remainder = compact.as_str();
        let needle = concat!("AgentKind", "::new_unchecked(");
        while let Some((_, suffix)) = remainder.split_once(needle) {
            let statement = suffix
                .split_once(';')
                .map_or(suffix, |(statement, _)| statement);
            if statement.contains(".spec().kind") {
                offenders.push(path.display().to_string());
                break;
            }
            remainder = suffix;
        }
    }
    if !offenders.is_empty() {
        bail!(
            "agent definitions expose typed kind_id(); keep unchecked kinds at open-set wire boundaries\n{}",
            offenders.join("\n")
        );
    }
    Ok(())
}

fn ensure_agents_do_not_depend_on_sidebar(root: &Path, files: &[PathBuf]) -> Result<()> {
    let agents = root.join("crates/rimz/src/agents");
    let needle = concat!("crate", "::sidebar");
    let mut offenders = Vec::new();
    for path in files
        .iter()
        .filter(|path| path.starts_with(&agents) && !is_test_source_path(root, path))
    {
        let source = fs::read_to_string(path)
            .with_context(|| format!("read invariant source {}", path.display()))?;
        if code_without_comments(&source).contains(needle) {
            offenders.push(path.display().to_string());
        }
    }
    if !offenders.is_empty() {
        bail!(
            "agent domain must not depend on sidebar code; project inputs flow into agents::spending\n{}",
            offenders.join("\n")
        );
    }
    Ok(())
}

fn code_without_comments(source: &str) -> String {
    let mut code = String::with_capacity(source.len());
    let mut block_comment = false;
    for line in source.lines() {
        let mut rest = line;
        loop {
            if block_comment {
                let Some((_, after)) = rest.split_once("*/") else {
                    break;
                };
                block_comment = false;
                rest = after;
                continue;
            }
            let line_comment = rest.find("//");
            let block_start = rest.find("/*");
            match (line_comment, block_start) {
                (Some(line_at), Some(block_at)) if block_at < line_at => {
                    code.push_str(&rest[..block_at]);
                    rest = &rest[block_at + 2..];
                    block_comment = true;
                }
                (Some(line_at), _) => {
                    code.push_str(&rest[..line_at]);
                    break;
                }
                (None, Some(block_at)) => {
                    code.push_str(&rest[..block_at]);
                    rest = &rest[block_at + 2..];
                    block_comment = true;
                }
                (None, None) => {
                    code.push_str(rest);
                    break;
                }
            }
        }
        code.push('\n');
    }
    code
}

fn ensure_normalized_agent_process_decisions(root: &Path, files: &[PathBuf]) -> Result<()> {
    let consumers = [
        root.join("crates/rimz/src/cli/hooks.rs"),
        root.join("crates/rimz/src/cli/hooks/owner.rs"),
        root.join("crates/rimz/src/proc/pane_probe.rs"),
    ];
    for provider in ["claude", "codex", "droid"] {
        let needle = format!("agents::{provider}::");
        ensure_no_match(
            files,
            &needle,
            |path| !consumers.iter().any(|consumer| consumer == path),
            "generic hook and process consumers must use normalized adapter or registry decisions",
        )?;
    }
    Ok(())
}

fn ensure_spending_walker_ownership(root: &Path, files: &[PathBuf]) -> Result<()> {
    let sidebar = root.join("crates/rimz/src/sidebar");
    let sidebar_pane = root.join("crates/rimz/src/sidebar_pane");
    let held_stats = root.join("crates/rimz/src/cli/stats/hold.rs");
    ensure_no_match(
        files,
        concat!("SpendingWalker", "::new"),
        |path| {
            let sidebar_test = path.starts_with(&sidebar)
                && path.file_name().and_then(OsStr::to_str) == Some("tests.rs");
            (!path.starts_with(&sidebar) || sidebar_test)
                && !path.starts_with(&sidebar_pane)
                && path != held_stats.as_path()
        },
        "sidebar data/render planes and held-stats code must use the elected spending service",
    )
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
        "rimz::store::atomic",
        "crate::store::atomic",
        "rimz::store::writer",
        "crate::store::writer",
    ] {
        ensure_no_match(
            files,
            needle,
            |path| {
                !path.starts_with(root.join("crates/rimz/src/sidebar_pane"))
                    || path.starts_with(root.join("xtask"))
            },
            "sidebar renderer must not import store writer APIs",
        )?;
    }
    Ok(())
}

fn ensure_spend_parser_boundaries(root: &Path, files: &[PathBuf]) -> Result<()> {
    let agents_root = root.join("crates/rimz/src/agents");
    for needle in [
        concat!("::", "atomic"),
        concat!("atomic", "::"),
        concat!("::", "run_wake"),
        concat!("run_wake", "::"),
        concat!("::", "broker"),
        concat!("broker", "::"),
    ] {
        ensure_no_match(
            files,
            needle,
            |path| !is_agent_spend_parser_path(path, &agents_root),
            "adapter spend parsers are read-only: no store writes, run-wake, or broker imports",
        )?;
    }
    Ok(())
}

fn ensure_sidebar_library_boundaries(root: &Path, files: &[PathBuf]) -> Result<()> {
    let sidebar_root = root.join("crates/rimz/src/sidebar");
    for needle in [
        concat!("store", "::", "writer"),
        concat!("::", "run_wake"),
        concat!("run_wake", "::"),
        concat!("::", "broker"),
        concat!("broker", "::"),
    ] {
        ensure_no_match(
            files,
            needle,
            |path| !path.starts_with(&sidebar_root),
            "crates/rimz/src/sidebar is read-only on the store: no writer, run-wake, or broker imports",
        )?;
    }
    Ok(())
}

fn ensure_sidebar_enrich_projection_only(root: &Path, files: &[PathBuf]) -> Result<()> {
    for needle in [
        concat!("Command", "::", "new"),
        concat!("std", "::", "process"),
        "child_process",
    ] {
        ensure_no_match(
            files,
            needle,
            |path| !is_sidebar_enrich_source(root, path),
            "sidebar enrich is projection-only: subprocess lanes live in crates/rimz/src/sidebar/refresh/",
        )?;
    }
    Ok(())
}

fn ensure_no_zellij_runtime_list_panes(root: &Path, files: &[PathBuf]) -> Result<()> {
    let needle = concat!("list", "-panes");
    let mut violations = Vec::new();
    for path in files {
        if path.extension().and_then(OsStr::to_str) != Some("rs")
            || is_test_source_path(root, path)
            || !is_zellij_or_sidebar_runtime_source(root, path)
        {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for (idx, line) in text.lines().enumerate() {
            if line.contains(needle)
                && !is_allowed_authoritative_zellij_pane_query(root, path, line)
            {
                violations.push(format!("{}:{}: {}", path.display(), idx + 1, line.trim()));
            }
        }
    }
    if violations.is_empty() {
        return Ok(());
    }
    bail!(
        "Zellij runtime must use presence-plugin topology; only the stale-topology confirmation path may query server panes\n{}",
        violations.join("\n")
    )
}

fn is_allowed_authoritative_zellij_pane_query(root: &Path, path: &Path, line: &str) -> bool {
    path == root.join("crates/rimz/src/mux/zellij/backend.rs")
        && line.contains("--all")
        && line.contains("--json")
}

fn is_zellij_or_sidebar_runtime_source(root: &Path, path: &Path) -> bool {
    path == root.join("crates/rimz/src/mux/zellij.rs")
        || path.starts_with(root.join("crates/rimz/src/mux/zellij"))
        || path.starts_with(root.join("crates/rimz/src/sidebar"))
}

fn is_sidebar_enrich_source(root: &Path, path: &Path) -> bool {
    path == root.join("crates/rimz/src/sidebar/enrich.rs")
        || path.starts_with(root.join("crates/rimz/src/sidebar/enrich"))
}

fn ensure_sidebar_event_log_reads_through_rollup(root: &Path, files: &[PathBuf]) -> Result<()> {
    for needle in [
        concat!("event_log", "::", "read_all"),
        concat!("event_log", "::", "read_from_offset"),
    ] {
        ensure_no_match(
            files,
            needle,
            |path| !is_sidebar_runtime_source(root, path) || is_test_source_path(root, path),
            "sidebar event-log reads must fold through RollupCursor, not read events.log directly",
        )?;
    }
    Ok(())
}

fn is_sidebar_runtime_source(root: &Path, path: &Path) -> bool {
    path.starts_with(root.join("crates/rimz/src/sidebar"))
        || path.starts_with(root.join("crates/rimz/src/sidebar_pane"))
}

fn is_test_source_path(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    relative
        .components()
        .any(|component| component.as_os_str() == OsStr::new("tests"))
        || relative
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name == "tests.rs" || name.ends_with("_tests.rs"))
}

fn ensure_store_durability(root: &Path, files: &[PathBuf]) -> Result<()> {
    for needle in [concat!(".sync_", "all("), concat!(".sync_", "data(")] {
        ensure_no_match(
            files,
            needle,
            |path| {
                path.ends_with("crates/rimz/src/store/atomic.rs") || is_docs_or_xtask(root, path)
            },
            "fsync syscalls live in store/atomic.rs alone — route through its helpers",
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
                    Some("hooks.rs" | "statusline.rs" | "pane.rs" | "sidebar.rs")
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

/// Sidebar render code names color intent through a component token or a
/// semantic accessor, never a raw terminal color — so the four-layer theme
/// stays the one place hue is decided. The only `Color::` path render code may
/// write is the `Color::Reset` sentinel: the 16 named ANSI variants are intent,
/// and `Color::Indexed`/`Color::Rgb` constructors are already-resolved emit
/// values the theme pipeline mints, never hand-picked in render. The render-side
/// theme module (which resolves `Tone` into ratatui carriers) and the ANSI depth
/// quantizer are exempt, as are test modules — tests legitimately assert
/// carrier→slot mappings. See docs/internals/theme.md and
/// docs/contributing/rust-conventions.md.
fn ensure_no_hardcoded_ui_colors(root: &Path, files: &[PathBuf]) -> Result<()> {
    let render_root = root.join("crates/rimz/src/sidebar_pane/render");
    let mut violations = Vec::new();
    for path in files {
        if path.extension().and_then(OsStr::to_str) != Some("rs") || !path.starts_with(&render_root)
        {
            continue;
        }
        let Ok(relative) = path.strip_prefix(&render_root) else {
            continue;
        };
        if ui_color_exempt(relative) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for (idx, line) in ui_color_violation_lines(&text) {
            violations.push(format!("{}:{}: {}", path.display(), idx + 1, line.trim()));
        }
    }
    if violations.is_empty() {
        return Ok(());
    }
    bail!(
        "sidebar render must name color through a component token (theme.component(Component::…)) \
         or a semantic accessor (theme.good/warn/caution/alarm, body/muted/faint/rule), never a \
         Color variant; only Color::Reset may be named — Color::Indexed/Rgb are minted by the \
         theme pipeline, not hand-picked in render — see docs/internals/theme.md\n{}",
        violations.join("\n")
    );
}

/// Render files that connect the color pipeline to the screen: the render-side
/// theme module and its component tokens, which turn a `Tone` into a ratatui
/// carrier, and `ansi.rs`, which quantizes that carrier down to an indexed
/// terminal. Test modules are exempt too. Scheme parsing and the OKLab math live
/// in the shared theme core, outside this scan entirely.
fn ui_color_exempt(relative: &Path) -> bool {
    let file = relative.file_name().and_then(OsStr::to_str);
    matches!(file, Some("theme.rs" | "ansi.rs"))
        || relative
            .components()
            .any(|component| component.as_os_str() == OsStr::new("theme"))
        || path_has_tests_component(relative)
        || file.is_some_and(|name| name == "tests.rs" || name.ends_with("_tests.rs"))
}

/// The banned `Color::` carrier lines outside any inline `mod tests` block. The
/// only `Color::` path render code may write is the `Color::Reset` sentinel: the
/// 16 named ANSI variants are intent, and `Color::Indexed`/`Color::Rgb`
/// constructors are already-resolved emit values that only the theme pipeline
/// mints. Needles are `concat!`-split so this file never trips its own grep.
fn ui_color_violation_lines(text: &str) -> Vec<(usize, &str)> {
    const BANNED: [&str; 18] = [
        concat!("Color", "::", "Red"),
        concat!("Color", "::", "Green"),
        concat!("Color", "::", "Yellow"),
        concat!("Color", "::", "Blue"),
        concat!("Color", "::", "Magenta"),
        concat!("Color", "::", "Cyan"),
        concat!("Color", "::", "Gray"),
        concat!("Color", "::", "DarkGray"),
        concat!("Color", "::", "LightRed"),
        concat!("Color", "::", "LightGreen"),
        concat!("Color", "::", "LightYellow"),
        concat!("Color", "::", "LightBlue"),
        concat!("Color", "::", "LightMagenta"),
        concat!("Color", "::", "LightCyan"),
        concat!("Color", "::", "White"),
        concat!("Color", "::", "Black"),
        concat!("Color", "::", "Indexed"),
        concat!("Color", "::", "Rgb"),
    ];
    let mut hits = Vec::new();
    let mut in_tests = false;
    for (idx, line) in text.lines().enumerate() {
        if line.trim_start().starts_with("mod tests") {
            in_tests = true;
        }
        if in_tests {
            continue;
        }
        if BANNED
            .iter()
            .any(|needle| names_color_variant(line, needle))
        {
            hits.push((idx, line));
        }
    }
    hits
}

fn ensure_cli_color_provenance(root: &Path, files: &[PathBuf]) -> Result<()> {
    let cli_root = root.join("crates/rimz/src/cli");
    let palette = cli_root.join("render/palette.rs");
    let mut violations = Vec::new();
    for path in files {
        if path.extension().and_then(OsStr::to_str) != Some("rs")
            || !path.starts_with(&cli_root)
            || path == &palette
            || is_test_source_path(root, path)
        {
            continue;
        }
        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };
        for (idx, line) in cli_color_violation_lines(&text) {
            violations.push(format!("{}:{}: {}", path.display(), idx + 1, line.trim()));
        }
    }
    if violations.is_empty() {
        return Ok(());
    }
    bail!(
        "CLI colors resolve through cli::render::palette accessors; construct no anstyle colors elsewhere\n{}",
        violations.join("\n")
    )
}

fn cli_color_violation_lines(text: &str) -> Vec<(usize, &str)> {
    const BANNED: [&str; 4] = [
        concat!("anstyle", "::", "Color"),
        concat!("Rgb", "Color("),
        concat!("Ansi256", "Color("),
        concat!("AnsiColor", "::"),
    ];
    lines_outside_inline_tests(text)
        .filter(|(_, line)| BANNED.iter().any(|needle| line.contains(needle)))
        .collect()
}

fn ensure_brand_resolution_single_home(root: &Path, files: &[PathBuf]) -> Result<()> {
    let definition = root.join("crates/rimz/src/agents/definition.rs");
    let provider = root.join("crates/rimz/src/theme/provider.rs");
    let plugin_definition = root.join("crates/rimz/src/agents/adapters/plugin/mod.rs");
    let sidebar_fixture = root.join("crates/rimz/src/cli/sidebar/fixture.rs");
    let needle = concat!(".brand", ".color");
    let mut violations = Vec::new();
    for path in files {
        if path.extension().and_then(OsStr::to_str) != Some("rs")
            || path == &definition
            || path == &provider
            || path == &plugin_definition
            || path == &sidebar_fixture
            || is_test_source_path(root, path)
        {
            continue;
        }
        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };
        for (idx, line) in lines_outside_inline_tests(&text) {
            if line.contains(needle) {
                violations.push(format!("{}:{}: {}", path.display(), idx + 1, line.trim()));
            }
        }
    }
    if violations.is_empty() {
        return Ok(());
    }
    bail!(
        "provider brand identity resolves through theme::resolve_provider_identity\n{}",
        violations.join("\n")
    )
}

fn lines_outside_inline_tests(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut in_tests = false;
    text.lines().enumerate().filter(move |(_, line)| {
        if line.trim_start().starts_with("mod tests") {
            in_tests = true;
        }
        !in_tests
    })
}

/// True when `line` writes `needle` (a `Color::<Variant>` path) as its own
/// identifier, not as the tail of a longer one like `ThemeColor::Indexed`.
fn names_color_variant(line: &str, needle: &str) -> bool {
    let mut from = 0;
    while let Some(rel) = line[from..].find(needle) {
        let at = from + rel;
        let glued_to_prefix = line[..at]
            .chars()
            .next_back()
            .is_some_and(|ch| ch.is_alphanumeric() || ch == '_');
        if !glued_to_prefix {
            return true;
        }
        from = at + needle.len();
    }
    false
}

fn ensure_no_hardcoded_glyphs(root: &Path, files: &[PathBuf]) -> Result<()> {
    let render_root = root.join("crates/rimz/src/sidebar_pane/render");
    let mut violations = Vec::new();
    for path in files {
        if path.extension().and_then(OsStr::to_str) != Some("rs") || !path.starts_with(&render_root)
        {
            continue;
        }
        let Ok(relative) = path.strip_prefix(&render_root) else {
            continue;
        };
        if ui_glyph_exempt(relative) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for (idx, line) in ui_glyph_violation_lines(&text) {
            violations.push(format!("{}:{}: {}", path.display(), idx + 1, line.trim()));
        }
    }
    if violations.is_empty() {
        return Ok(());
    }
    bail!(
        "sidebar render glyphs must route through theme.glyph(GlyphRole::…); the shipped catalog \
         lives in crates/rimz/src/theme/glyphs.rs and animation defaults live in \
         render/animation.rs\n{}",
        violations.join("\n")
    );
}

/// Render files allowed to write glyph literals: the animation module, which
/// owns the spinner frame sequences, plus every test module — tests legitimately
/// assert rendered shapes. The shipped catalog lives in the shared theme core
/// (`crates/rimz/src/theme/glyphs.rs`), outside this scan entirely.
fn ui_glyph_exempt(relative: &Path) -> bool {
    let file = relative.file_name().and_then(OsStr::to_str);
    file == Some("animation.rs")
        || path_has_tests_component(relative)
        || file.is_some_and(|name| name == "tests.rs" || name.ends_with("_tests.rs"))
}

fn ui_glyph_violation_lines(text: &str) -> Vec<(usize, &str)> {
    const BANNED: [&str; 55] = [
        "◇", "↘", "↗", "◌", "◍", "◎", "↻", "⧉", "¤", "⇅", "∞", "━", "─", "╸", "╺", "▰", "▱", "▐",
        "▕", "▣", "▢", "▤", "◔", "◑", "◕", "◉", "▌", "▎", "🮇", "┤", "├", "⑂", "⇡", "⇣", "≡", "✓",
        "┄", "⌘", "⚠", "⇄", "●", "○", "⋯", "↕", "⏎", "␣", "✉", "↔", "⟳", "✕", "╭", "╮", "╰", "╯",
        "│",
    ];
    let mut hits = Vec::new();
    let mut in_tests = false;
    for (idx, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("mod tests") {
            in_tests = true;
        }
        if in_tests
            || trimmed.starts_with("//")
            || trimmed.starts_with("///")
            || trimmed.starts_with("//!")
        {
            continue;
        }
        if BANNED.iter().any(|needle| line.contains(needle)) {
            hits.push((idx, line));
        }
    }
    hits
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
    let path = root.join("crates/rimz/src/store/snapshot/view/live.rs");
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
    let required = [
        (
            root.join("crates/rimz/src/config/templates/config.template.toml"),
            &[
                "[remote_control]",
                "[accounts]",
                "[accounts.usage_limit_usd]",
                "[notifications]",
                "[sidebar]",
                "[mux]",
                "[zellij]",
                "[tmux]",
                "[resume]",
            ][..],
        ),
        (
            root.join("crates/rimz/src/config/templates/theme.template.toml"),
            &[
                "[theme]",
                "[theme.display]",
                "[theme.display.context_meter]",
                "[theme.display.budget_bar]",
                "[theme.display.budget_bar.burn_rate]",
                "[theme.display.highlight_steps]",
                "[theme.pets]",
                "[theme.animations]",
                "[theme.glyphs]",
                "[theme.glyphs.unicode.status]",
                "[theme.glyphs.nerd_font.status]",
                "[theme.providers]",
                "[colors.primary]",
            ][..],
        ),
        (
            root.join("crates/rimz/src/config/templates/agents.template.toml"),
            &[
                "[agents]",
                "[agents.worktree]",
                "[agents.attention]",
                "[agents.profiles]",
                "[agents.commands]",
                "[agents.teams]",
            ][..],
        ),
        (
            root.join("crates/rimz/src/config/templates/loop.template.toml"),
            &["[tasks]"][..],
        ),
    ];
    let mut missing = Vec::new();
    for (path, sections) in required {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        missing.extend(
            sections
                .iter()
                .filter(|section| !text.contains(**section))
                .map(|section| format!("{}: {section}", path.display())),
        );
    }
    if missing.is_empty() {
        return Ok(());
    }
    bail!(
        "config templates are missing required sections: {}",
        missing.join(", ")
    );
}

fn ensure_presence_plugin_vendored(root: &Path) -> Result<()> {
    let wasm_path = crate::build::vendored_plugin_path(root);
    let bytes = match fs::read(&wasm_path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            bail!(
                "{} is missing; run `cargo xtask plugin-refresh`",
                wasm_path.display()
            )
        }
        Err(err) => return Err(err).with_context(|| format!("reading {}", wasm_path.display())),
    };
    if bytes.is_empty() {
        bail!(
            "{} is empty; run `cargo xtask plugin-refresh`",
            wasm_path.display()
        );
    }
    if !crate::build::is_wasm_module(&bytes) {
        bail!(
            "{} is not a wasm module; run `cargo xtask plugin-refresh`",
            wasm_path.display()
        );
    }

    let srchash_path = crate::build::vendored_srchash_path(root);
    let recorded = match fs::read_to_string(&srchash_path) {
        Ok(recorded) => recorded,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            bail!(
                "{} is missing; run `cargo xtask plugin-refresh`",
                srchash_path.display()
            )
        }
        Err(err) => {
            return Err(err).with_context(|| format!("reading {}", srchash_path.display()));
        }
    };
    let expected = crate::build::presence_plugin_source_digest(root)?;
    if recorded != expected {
        bail!(
            "{} is stale for crates/rimz-presence-zellij; run `cargo xtask plugin-refresh`",
            srchash_path.display()
        );
    }

    let manifest_path = root.join("crates/rimz/Cargo.toml");
    let manifest_toml = fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    ensure_include_covers_build_inputs(&manifest_toml)?;
    Ok(())
}

const PACKAGED_BUILD_INPUTS: &[&str] = &[
    "/presence/rimz-presence-zellij.wasm",
    "/pricing/litellm-pricing.json",
];

/// The crate `include` allowlist packages only its entries, so build.rs inputs
/// must be force-listed or the published crate drops them and falls back at
/// build time.
fn ensure_include_covers_build_inputs(manifest_toml: &str) -> Result<()> {
    let manifest: toml::Value =
        toml::from_str(manifest_toml).context("parsing crates/rimz/Cargo.toml")?;
    let include: Vec<&str> = manifest
        .get("package")
        .and_then(|package| package.get("include"))
        .and_then(toml::Value::as_array)
        .map(|entries| entries.iter().filter_map(toml::Value::as_str).collect())
        .unwrap_or_default();

    for required in PACKAGED_BUILD_INPUTS {
        if !include.iter().any(|entry| entry == required) {
            bail!(
                "crates/rimz/Cargo.toml [package].include is missing {required}; \
                 add it or the published crate drops this build input"
            );
        }
    }
    Ok(())
}

fn ensure_no_core_pane_auto_use(root: &Path, files: &[PathBuf]) -> Result<()> {
    let allowed_prefixes = [
        root.join("crates/rimz/src/cli/pane.rs"),
        root.join("crates/rimz/src/mux"),
        root.join("crates/rimz/tests"),
        root.join("docs"),
        root.join("xtask"),
    ];
    let agents_show_command = "crates/rimz/src/cli/agents_cmd/show.rs";
    let run_failure_capture = root.join("crates/rimz/src/cli/supervised/pane.rs");
    let codex_turn_death_confirmation = "crates/rimz/src/sidebar/refresh/sessions.rs";
    for needle in [
        concat!("capture", "_pane("),
        concat!("send", "_keys("),
        concat!("send", "_key("),
    ] {
        let mut violations = Vec::new();
        for path in files {
            if allowed_prefixes
                .iter()
                .any(|prefix| path.starts_with(prefix))
            {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            let lines: Vec<&str> = text.lines().collect();
            for (idx, line) in lines.iter().enumerate() {
                if !line.contains(needle) {
                    continue;
                }
                // `rimz agents show --capture` is an explicit user-facing pane
                // read, wired to the same primitive as `rimz pane capture`.
                if needle == concat!("capture", "_pane(")
                    && path.to_string_lossy().ends_with(agents_show_command)
                    && idx > 0
                    && lines[idx - 1].trim() == "// rimz-invariant: explicit-agent-show-capture"
                {
                    continue;
                }
                // Supervised runs explicitly capture their own transient pane
                // tail before cleanup when a run fails.
                if needle == concat!("capture", "_pane(")
                    && path == run_failure_capture.as_path()
                    && idx > 0
                    && lines[idx - 1].trim() == "// rimz-invariant: run-failure-capture"
                {
                    continue;
                }
                // Codex capacity kills expose their warning only in the pane;
                // the producer reads a bounded tail to refine the transcript
                // shape marker's label.
                if needle == concat!("capture", "_pane(")
                    && path
                        .to_string_lossy()
                        .ends_with(codex_turn_death_confirmation)
                    && idx > 0
                    && lines[idx - 1].trim() == "// rimz-invariant: codex-turn-death-confirmation"
                {
                    continue;
                }
                violations.push(format!("{}:{}: {}", path.display(), idx + 1, line.trim()));
            }
        }
        if !violations.is_empty() {
            bail!(
                "core paths must not auto-use pane capture/send primitives\n{}",
                violations.join("\n")
            );
        }
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

fn ensure_snapshot_projection_stays_quiet(root: &Path, files: &[PathBuf]) -> Result<()> {
    let snapshot_root = root.join("crates/rimz/src/store/snapshot");
    for needle in ["warn!(", "error!("] {
        ensure_no_match(
            files,
            needle,
            |path| !path.starts_with(&snapshot_root) || is_test_source_path(root, path),
            "store/snapshot projection re-folds per frame: diagnostics stay debug!-level (warn!/error! there floods the off-box channel per fold)",
        )?;
    }
    Ok(())
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
