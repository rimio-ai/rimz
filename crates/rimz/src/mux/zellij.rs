//! Zellij `MuxBackend` implementation.
//!
//! Interactive actions run `zellij action <verb> ...` against the session
//! inferred from the caller's `ZELLIJ_SESSION_NAME` env var. Operations that
//! may run before the user attaches, such as native sidebar launch and wakeup
//! fanout, carry the session name explicitly via `zellij --session <name>`.
//!
//! Caveats live in `docs/internals/multiplexers.md` under
//! "Zellij backend caveats" — namely that raw Zellij pane IDs are
//! integers, scoped per-session, and that the spike does not yet expose
//! tab-level operations beyond what's needed to identify a pane.

use std::env;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::{
    CommandSpec, MuxBackend, MuxErr, PaneCapture, PaneListOptions, Result, SessionOptions,
    SidebarPaneOptions, SplitPaneOptions, ensure_pane_backend,
};
use crate::feed::PaneRef;
use crate::ids::{MuxName, PaneId, ViewKind};

/// Minimum Zellij version that ships the pipe-broadcast semantics Rimz uses
/// as a best-effort wakeup optimization.
pub const MIN_ZELLIJ_VERSION: (u32, u32, u32) = (0, 41, 0);

/// Bundle reported by `rimz doctor` when the active backend is Zellij.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ZellijCapabilities {
    pub binary_version: String,
    pub parsed_version: Option<(u32, u32, u32)>,
    pub meets_min_version: bool,
}

/// Probe the installed Zellij. Cheap: one `zellij --version` call.
pub fn capabilities() -> Result<ZellijCapabilities> {
    let raw = ZellijBackend.version()?;
    let parsed = parse_version(&raw);
    Ok(ZellijCapabilities {
        meets_min_version: parsed.is_some_and(|v| v >= MIN_ZELLIJ_VERSION),
        binary_version: raw,
        parsed_version: parsed,
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

    fn ensure_session(&self, _opts: &SessionOptions) -> Result<()> {
        // Zellij creates sessions lazily, and `open_sidebar` owns first birth
        // by rendering the session from a layout (Zellij applies a layout only
        // at session creation). There is nothing to pre-create here.
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

    fn open_sidebar(&self, opts: &SidebarPaneOptions) -> Result<()> {
        // The session is born from a layout exactly once. If it already
        // exists it already carries its sidebar — Zellij applies a layout
        // only at session birth, so we never re-inject. Touch the layout
        // once, at creation; the user owns every resize and split afterward.
        if self
            .list_sessions()?
            .iter()
            .any(|session| session == &opts.session_name)
        {
            return Ok(());
        }
        create_session_with_sidebar(opts)
    }

    fn wake_sidebar(&self, session_name: &str, bytes: &[u8]) -> Result<()> {
        // Per-instance socket fanout is the channel of record. The broadcast
        // `zellij pipe` here is a latency optimization on top.
        //
        // The ledger wakeup walk may set `RIMZ_ZELLIJ_BIN` to point at a test
        // shim binary (see `tests/fixtures/zellij-trace`); honor it so the
        // wiring is testable end-to-end.
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

fn terminal_pane_count(panes: &[RawPane]) -> usize {
    panes
        .iter()
        .filter(|pane| !pane.is_plugin && !pane.is_suppressed)
        .count()
}

/// Create the background session from a layout that puts the `rimz-sidebar`
/// pane on the left and focuses the user's terminal on the right. The layout
/// doubles as the default tab template, so new tabs are born with a sidebar
/// too. The sidebar pane is `close_on_exit`, so when its own process exits the
/// pane closes — see the self-close loop in `crates/rimz-sidebar`.
///
/// Zellij parses `--default-layout` asynchronously, after the
/// `--create-background` client returns, so the temp layout file must outlive
/// the call. We hold it through a bounded wait for the sidebar pane to appear,
/// then let it drop.
fn create_session_with_sidebar(opts: &SidebarPaneOptions) -> Result<()> {
    let layout = TempLayoutFile::new(render_sidebar_layout(opts)?)?;
    let spec = CommandSpec::new("zellij").args([
        "attach".to_owned(),
        "--create-background".to_owned(),
        opts.session_name.clone(),
        "options".to_owned(),
        "--default-cwd".to_owned(),
        opts.cwd.to_string_lossy().into_owned(),
        "--default-layout".to_owned(),
        layout.path().to_string_lossy().into_owned(),
    ]);
    let mut command = spec.to_command();
    command.current_dir(&opts.cwd);
    let output = command.output().map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => MuxErr::NotInstalled {
            program: spec.program.clone(),
        },
        _ => MuxErr::Io(err),
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let lower = stderr.to_ascii_lowercase();
        // A racing `rimz` may have created the session first; treat that as
        // success rather than re-injecting.
        if !(lower.contains("already exists")
            || (lower.contains("session") && lower.contains("exists")))
        {
            return Err(MuxErr::Command {
                program: spec.program,
                args: spec.args.join(" "),
                stderr,
            });
        }
    }
    wait_for_sidebar_layout(&opts.session_name);
    drop(layout);
    Ok(())
}

/// Block until Zellij has materialized the sidebar + terminal panes the layout
/// describes, so the caller's temp layout file stays on disk long enough to be
/// read. Bounded and best-effort: a slow or failed materialization just means
/// an earlier drop of the file.
fn wait_for_sidebar_layout(session_name: &str) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if let Ok(panes) = ZellijBackend::list_panes_with_session(Some(session_name))
            && terminal_pane_count(&panes) >= 2
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
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

struct TempLayoutFile {
    path: PathBuf,
}

impl TempLayoutFile {
    fn new(contents: String) -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "rimz-zellij-layout-{}-{}.kdl",
            std::process::id(),
            uuid::Uuid::now_v7().simple(),
        ));
        std::fs::write(&path, contents)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempLayoutFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn render_sidebar_layout(opts: &SidebarPaneOptions) -> Result<String> {
    let rimz_bin = kdl_string(&opts.rimz_bin.to_string_lossy())?;
    let workspace_id = kdl_string(opts.workspace_id.as_str())?;
    let session_name = kdl_string(&opts.session_name)?;
    let size = kdl_string(&format!("{}%", opts.width_percent.clamp(10, 90)))?;
    // The sidebar lives in the `default_tab_template`, so every tab — the
    // explicit first one and any the user opens later — is born with it. The
    // explicit first `tab` focuses the terminal child; the working cwd comes
    // from the session's `--default-cwd`, so panes need no `cwd` of their own.
    Ok(format!(
        r#"layout {{
    default_tab_template split_direction="vertical" {{
        pane size={size} name="rimz-sidebar" {{
            command {rimz_bin}
            args "sidebar" "serve" "--mux" "zellij" "--workspace-id" {workspace_id} "--session-name" {session_name}
            close_on_exit true
        }}
        children
    }}
    tab name="rimz" {{
        pane focus=true
    }}
}}
"#,
    ))
}

fn kdl_string(value: &str) -> Result<String> {
    serde_json::to_string(value).map_err(|err| MuxErr::Output {
        program: "zellij".to_owned(),
        reason: format!("escaping layout string: {err}"),
    })
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
}
