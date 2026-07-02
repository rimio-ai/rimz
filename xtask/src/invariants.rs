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
    ensure_sidebar_enrich_projection_only(root, &files)?;
    ensure_sidebar_event_log_reads_through_rollup(root, &files)?;
    ensure_snapshot_json_writes_stay_in_produce(root, &files)?;
    ensure_diag_writes_stay_in_diag(root, &files)?;
    ensure_sidebar_enrich_folds_before_live_panes(root)?;
    ensure_card_admission_predicate(root)?;
    ensure_config_template_sections(root)?;
    ensure_sidebar_render_runtime_uses_snapshot_clock(root, &files)?;
    ensure_no_hardcoded_ui_colors(root, &files)?;
    ensure_no_hardcoded_glyphs(root, &files)?;
    ensure_presence_plugin_vendored(root)?;
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

/// Sidebar render code names color intent through a component token or a
/// semantic accessor, never a raw terminal color — so the four-layer theme
/// stays the one place hue is decided. The only `Color::` path render code may
/// write is the `Color::Reset` sentinel: the 16 named ANSI variants are intent,
/// and `Color::Indexed`/`Color::Rgb` constructors are already-resolved emit
/// values the theme pipeline mints, never hand-picked in render. The theme
/// module (which owns the Raw→Semantic→Component→emit pipeline and the depth
/// quantizer), the Alacritty parser, and the OKLab math are exempt, as are test
/// modules — tests legitimately assert carrier→slot mappings. See
/// docs/reference/theme.md and docs/contributing/rust-conventions.md.
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
         theme pipeline, not hand-picked in render — see docs/reference/theme.md\n{}",
        violations.join("\n")
    );
}

/// Render files that own or bridge the color pipeline (theme module, depth
/// quantizer, Alacritty parser, OKLab math), plus every test module.
fn ui_color_exempt(relative: &Path) -> bool {
    let file = relative.file_name().and_then(OsStr::to_str);
    matches!(
        file,
        Some("theme.rs" | "ansi.rs" | "scheme.rs" | "oklab.rs")
    ) || relative
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
        "sidebar render glyphs must route through theme.glyph(GlyphRole::…); defaults live in \
         render/theme/glyphs.rs and animation defaults live in render/animation.rs\n{}",
        violations.join("\n")
    );
}

fn ui_glyph_exempt(relative: &Path) -> bool {
    let file = relative.file_name().and_then(OsStr::to_str);
    (file == Some("glyphs.rs")
        && relative
            .components()
            .any(|component| component.as_os_str() == OsStr::new("theme")))
        || file == Some("animation.rs")
        || path_has_tests_component(relative)
        || file.is_some_and(|name| name == "tests.rs" || name.ends_with("_tests.rs"))
}

fn ui_glyph_violation_lines(text: &str) -> Vec<(usize, &str)> {
    const BANNED: [&str; 54] = [
        "◇", "↘", "↗", "◌", "◍", "◎", "↻", "⧉", "¤", "⇅", "∞", "━", "─", "╸", "▰", "▱", "▐", "▕",
        "▣", "▢", "▤", "◔", "◑", "◕", "◉", "▌", "▎", "🮇", "┤", "├", "⑂", "⇡", "⇣", "≡", "✓", "┄",
        "⌘", "⚠", "⇄", "●", "○", "⋯", "↕", "⏎", "␣", "✉", "↔", "⟳", "✕", "╭", "╮", "╰", "╯", "│",
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
    let required = [
        (
            root.join("crates/rimz/src/config/templates/config.template.toml"),
            &[
                "[remote_control]",
                "[accounts]",
                "[accounts.usage_limit_usd]",
                "[notifications]",
                "[sidebar]",
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
