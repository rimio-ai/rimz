//! Zellij `MuxBackend` implementation.
//!
//! Every action subcommand runs `zellij action <verb> ...` against the
//! session inferred from the caller's `ZELLIJ_SESSION_NAME` env var (the
//! standard Zellij convention). `wake_sidebar` is the one outlier: it can
//! be invoked from a process that is not itself attached to Zellij — the
//! ledger wakeup walk — so it carries the session name explicitly via the
//! top-level `zellij --session <name>` flag.
//!
//! Caveats live in `docs/internals/multiplexers.md` under
//! "Zellij backend caveats" — namely that raw Zellij pane IDs are
//! integers, scoped per-session, and that the spike does not yet expose
//! tab-level operations beyond what's needed to identify a pane.

use std::env;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{
    CommandSpec, MuxBackend, MuxErr, PaneCapture, PaneListOptions, Result, SplitPaneOptions,
    ensure_pane_backend,
};
use crate::feed::PaneRef;
use crate::ids::{MuxName, PaneId, ViewKind};

/// Minimum Zellij version that ships the pipe-broadcast semantics Rimz
/// relies on (lazy-load suppression on `--name` without `--plugin`).
pub const MIN_ZELLIJ_VERSION: (u32, u32, u32) = (0, 41, 0);

/// Filename inside the XDG data directory where Rimz expects to find the
/// sidebar plugin. `doctor` warns when the file is missing and
/// `open_sidebar` returns `NotInstalled` so the user knows to install it.
const SIDEBAR_PLUGIN_FILENAME: &str = "rimz/sidebar.wasm";

/// Resolve the on-disk path Rimz looks at for the Zellij sidebar plugin.
/// Uses `$XDG_DATA_HOME` when set; falls back to `$HOME/.local/share`.
/// Mirrors the XDG resolution in [`crate::ledger::paths::state_home`].
pub fn sidebar_plugin_path() -> PathBuf {
    resolve_plugin_path(
        env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()),
        env::var_os("HOME").filter(|v| !v.is_empty()),
    )
}

/// Pure path resolution split out for unit testing — `unsafe_code` is
/// forbidden workspace-wide, so we can't mutate env to exercise
/// [`sidebar_plugin_path`] directly.
fn resolve_plugin_path(
    xdg_data_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> PathBuf {
    if let Some(value) = xdg_data_home {
        return PathBuf::from(value).join(SIDEBAR_PLUGIN_FILENAME);
    }
    if let Some(home) = home {
        return PathBuf::from(home)
            .join(".local/share")
            .join(SIDEBAR_PLUGIN_FILENAME);
    }
    PathBuf::from("/tmp").join(SIDEBAR_PLUGIN_FILENAME)
}

/// Bundle reported by `rimz doctor` when the active backend is Zellij.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ZellijCapabilities {
    pub binary_version: String,
    pub parsed_version: Option<(u32, u32, u32)>,
    pub meets_min_version: bool,
    pub plugin_path: PathBuf,
    pub plugin_present: bool,
}

/// Probe the installed Zellij and the on-disk sidebar plugin. Cheap: one
/// `zellij --version` call plus one `metadata` lookup.
pub fn capabilities() -> Result<ZellijCapabilities> {
    let raw = ZellijBackend.version()?;
    let parsed = parse_version(&raw);
    let plugin_path = sidebar_plugin_path();
    let plugin_present = plugin_path.is_file();
    Ok(ZellijCapabilities {
        meets_min_version: parsed.is_some_and(|v| v >= MIN_ZELLIJ_VERSION),
        binary_version: raw,
        parsed_version: parsed,
        plugin_path,
        plugin_present,
    })
}

/// Parse `"zellij 0.41.2"` (and tolerant of leading/trailing whitespace).
/// Returns None when the shape is unexpected so `doctor` can render the
/// raw string verbatim.
fn parse_version(raw: &str) -> Option<(u32, u32, u32)> {
    let trimmed = raw.trim();
    let after_prefix = trimmed.strip_prefix("zellij ").unwrap_or(trimmed);
    let mut parts = after_prefix
        .split(|c: char| !c.is_ascii_digit() && c != '.')
        .next()?
        .split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

#[derive(Clone, Debug, Default)]
pub struct ZellijBackend;

impl ZellijBackend {
    fn list_panes_with_session(session: Option<&str>) -> Result<Vec<RawPane>> {
        let mut spec = CommandSpec::new("zellij");
        if let Some(name) = session {
            spec = spec.args(["--session".to_owned(), name.to_owned()]);
        }
        spec = spec.args(["action", "list-panes", "-j", "-a"]);
        let output = spec.run()?;
        serde_json::from_slice::<Vec<RawPane>>(&output.stdout).map_err(|e| MuxErr::Output {
            program: "zellij".to_owned(),
            reason: format!("parsing list-panes JSON: {e}"),
        })
    }
}

impl MuxBackend for ZellijBackend {
    fn name(&self) -> MuxName {
        MuxName::Zellij
    }

    fn ensure_session(&self, _name: &str) -> Result<()> {
        // Zellij creates the session lazily on `attach --create`. Layout-driven
        // precreate belongs behind the project trust gate.
        Ok(())
    }

    fn attach_command(&self, name: &str) -> CommandSpec {
        CommandSpec::new("zellij").args(["attach", "--create", name])
    }

    fn detach(&self, _name: &str) -> Result<()> {
        CommandSpec::new("zellij")
            .args(["action", "detach"])
            .run()
            .map(|_| ())
    }

    fn list_sessions(&self) -> Result<Vec<String>> {
        let output = CommandSpec::new("zellij").arg("list-sessions").run()?;
        // Output lines look like `name [Created Ns ago]`; the bare name
        // appears as a leading whitespace-separated token. Strip ANSI escapes
        // defensively in case `list-sessions` colorizes its output.
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(strip_ansi)
            .map(|line| line.split_whitespace().next().unwrap_or(&line).to_owned())
            .filter(|line| !line.is_empty())
            .collect())
    }

    fn list_panes(&self, opts: PaneListOptions) -> Result<Vec<PaneRef>> {
        let raws = Self::list_panes_with_session(opts.session_name.as_deref())?;
        let session_name = opts.session_name.unwrap_or_default();
        Ok(raws
            .into_iter()
            .filter(|p| !p.is_plugin && !p.is_suppressed)
            .map(|p| PaneRef {
                pane_id: PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", p.id)),
                session_name: session_name.clone(),
                view_id: Some(format!("tab_{}", p.tab_id)),
                view_kind: Some(ViewKind::Tab),
                pane_process_start: None,
            })
            .collect())
    }

    fn split_pane(&self, opts: SplitPaneOptions) -> Result<()> {
        let mut spec = CommandSpec::new("zellij").args(["action", "new-pane"]);
        if let Some(target) = opts.target_pane_id {
            ensure_pane_backend(&target, MuxName::Zellij)?;
            // Zellij's CLI opens relative to the current focus and does not
            // expose a target-pane flag for `new-pane`.
        }
        if let Some(cwd) = opts.cwd {
            spec = spec.args(["--cwd".to_owned(), cwd]);
        }
        if let Some(command) = opts.command
            && let Some((program, args)) = command.split_first()
        {
            spec = spec
                .args(["--".to_owned(), program.clone()])
                .args(args.iter().cloned());
        }
        spec.run().map(|_| ())
    }

    fn focus_pane(&self, pane: &PaneId) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Zellij)?;
        // Zellij 0.41+: `focus-pane-id <raw>`. The earlier `focus-pane-with-id`
        // name was removed; the stub that referenced it never reached a
        // running binary.
        CommandSpec::new("zellij")
            .args(["action", "focus-pane-id", pane.raw()])
            .run()
            .map(|_| ())
    }

    fn capture_pane(&self, pane: &PaneId, lines: Option<u16>, ansi: bool) -> Result<PaneCapture> {
        ensure_pane_backend(pane, MuxName::Zellij)?;
        let mut spec = CommandSpec::new("zellij").args(["action", "dump-screen"]);
        if ansi {
            spec = spec.arg("-a");
        }
        if lines.is_some() {
            // The `-f`/`--full` flag dumps the entire scrollback. Zellij does
            // not expose a "last N lines" cap at the CLI level, so any non-None
            // request maps onto "include scrollback"; the caller can post-trim.
            spec = spec.arg("-f");
        }
        spec = spec.args(["-p".to_owned(), pane.raw().to_owned()]);
        let output = spec.run()?;
        let raw_text = String::from_utf8_lossy(&output.stdout).into_owned();
        let (raw_text, lines) = trim_capture(raw_text, lines);
        Ok(PaneCapture {
            pane_id: pane.clone(),
            raw_text,
            lines,
        })
    }

    fn send_keys(&self, pane: &PaneId, text: &str) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Zellij)?;
        CommandSpec::new("zellij")
            .args(["action", "write-chars", "--pane-id", pane.raw(), text])
            .run()
            .map(|_| ())
    }

    fn open_sidebar(&self, session_name: &str, _width: u16) -> Result<()> {
        let plugin = sidebar_plugin_path();
        if !plugin.is_file() {
            return Err(MuxErr::NotInstalled {
                program: "rimz-sidebar.wasm".to_owned(),
            });
        }
        let url = format!("file:{}", plugin.display());
        CommandSpec::new("zellij")
            .args([
                "--session".to_owned(),
                session_name.to_owned(),
                "action".to_owned(),
                "launch-or-focus-plugin".to_owned(),
                "--floating".to_owned(),
                "false".to_owned(),
                url,
            ])
            .run()
            .map(|_| ())
    }

    fn wake_sidebar(&self, session_name: &str, bytes: &[u8]) -> Result<()> {
        // Per-instance socket fanout is the channel of record. The broadcast
        // `zellij pipe` here is a latency optimization on top.
        //
        // The ledger wakeup walk may set `RIMZ_ZELLIJ_BIN` to point at a test
        // shim binary (see `tests/fixtures/zellij-trace`); honor it so the
        // wiring is testable end-to-end without a live Zellij plugin.
        let program = env::var("RIMZ_ZELLIJ_BIN").unwrap_or_else(|_| "zellij".to_owned());
        let payload = String::from_utf8_lossy(bytes).to_string();
        CommandSpec::new(program)
            .args([
                "--session",
                session_name,
                "pipe",
                "--name",
                "rimz::feed",
                "--",
                &payload,
            ])
            .run()
            .map(|_| ())
    }

    fn version(&self) -> Result<String> {
        let output = CommandSpec::new("zellij")
            .arg("--version")
            .to_command()
            .output()
            .map_err(|err| match err.kind() {
                std::io::ErrorKind::NotFound => MuxErr::NotInstalled {
                    program: "zellij".to_owned(),
                },
                _ => MuxErr::Io(err),
            })?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }
}

/// Subset of fields `zellij action list-panes -j -a` emits. We deserialize
/// only what we route into `PaneRef`; serde silently ignores everything else.
#[derive(Debug, Deserialize)]
struct RawPane {
    id: u64,
    is_plugin: bool,
    #[serde(default)]
    is_suppressed: bool,
    tab_id: u64,
}

/// Defensive ANSI strip for `list-sessions` output. Zellij ships a colored
/// banner in newer versions; the parser only cares about the bare name.
///
/// Handles the CSI subset (`ESC [ params final`) Zellij emits. The
/// introducer `[` lives at 0x5b which overlaps the final-byte range
/// (0x40..=0x7e), so we must consume the introducer first and only then
/// scan for the final byte.
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if let Some('[') = chars.next() {
                for ch in chars.by_ref() {
                    if matches!(ch, '\x40'..='\x7e') {
                        break;
                    }
                }
            }
            // Non-CSI escape (single byte after ESC) or end-of-string: nothing
            // to skip; the next iteration resumes the scan.
        } else {
            out.push(c);
        }
    }
    out
}

fn trim_capture(raw_text: String, max_lines: Option<u16>) -> (String, Vec<String>) {
    let mut lines: Vec<String> = raw_text.lines().map(str::to_owned).collect();
    if let Some(max_lines) = max_lines {
        let keep = max_lines as usize;
        if keep == 0 {
            lines.clear();
        } else if lines.len() > keep {
            lines = lines.split_off(lines.len() - keep);
        }
    }

    let mut trimmed = lines.join("\n");
    if raw_text.ends_with('\n') && !trimmed.is_empty() {
        trimmed.push('\n');
    }
    (trimmed, lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parser_accepts_three_dot_form() {
        assert_eq!(parse_version("zellij 0.41.2"), Some((0, 41, 2)));
        assert_eq!(parse_version("  zellij 1.2.3  \n"), Some((1, 2, 3)));
        assert_eq!(parse_version("zellij 0.44"), Some((0, 44, 0)));
        assert_eq!(parse_version("garbage"), None);
    }

    #[test]
    fn min_version_threshold_holds() {
        assert!((0, 41, 0) >= MIN_ZELLIJ_VERSION);
        assert!((0, 44, 3) >= MIN_ZELLIJ_VERSION);
        assert!((0, 40, 9) < MIN_ZELLIJ_VERSION);
    }

    #[test]
    fn raw_pane_deserializes_minimal_shape() {
        let json = r#"[
          {"id": 0, "is_plugin": false, "is_suppressed": false, "tab_id": 0},
          {"id": 2, "is_plugin": true,  "is_suppressed": false, "tab_id": 0}
        ]"#;
        let parsed: Vec<RawPane> = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.len(), 2);
        assert!(!parsed[0].is_plugin);
        assert!(parsed[1].is_plugin);
    }

    #[test]
    fn ansi_strip_drops_color_codes() {
        let stripped = strip_ansi("\x1b[32mfoo\x1b[0m bar");
        assert_eq!(stripped, "foo bar");
    }

    #[test]
    fn capture_trim_keeps_last_requested_lines() {
        let (raw, lines) = trim_capture("a\nb\nc\nd\n".to_owned(), Some(2));
        assert_eq!(lines, vec!["c", "d"]);
        assert_eq!(raw, "c\nd\n");
    }

    #[test]
    fn resolve_plugin_path_prefers_xdg_data_home() {
        assert_eq!(
            resolve_plugin_path(Some("/tmp/xdg-data".into()), Some("/ignored".into())),
            PathBuf::from("/tmp/xdg-data/rimz/sidebar.wasm"),
        );
    }

    #[test]
    fn resolve_plugin_path_falls_back_to_home() {
        assert_eq!(
            resolve_plugin_path(None, Some("/home/marv".into())),
            PathBuf::from("/home/marv/.local/share/rimz/sidebar.wasm"),
        );
    }

    #[test]
    fn resolve_plugin_path_last_resort_is_tmp() {
        assert_eq!(
            resolve_plugin_path(None, None),
            PathBuf::from("/tmp/rimz/sidebar.wasm"),
        );
    }
}
