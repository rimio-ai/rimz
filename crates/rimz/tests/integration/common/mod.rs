//! Shared harness for integration tests. Real tempdir, real store files —
//! no in-memory stubs.
//!
//! Two entry points, one module each:
//! - [`Env`] (`env`) drives the `rimz` binary out of process (the CLI tier):
//!   host state and both mux namespaces scoped to tempdirs, the workspace
//!   resolved from the project root, and repeated hook round-trip helpers.
//! - [`Harness`] (`harness`) opens a real [`rimz::Store`] in process (the
//!   library tier) for tests that drive store APIs directly.
//!
//! `payloads` holds the agent hook-payload fixtures and environment probes
//! shared across tiers; `codex` holds the hook and project trust setup a real
//! Codex launch needs.

mod codex;
mod command;
mod env;
pub(crate) mod git;
mod harness;
mod payloads;
#[cfg(unix)]
pub(crate) mod room;
mod shim;
mod skip;
#[cfg(unix)]
pub(crate) use shim::{
    gh_fork_head, gh_same_repo_head, tea_fork_head, tea_same_repo_head, write_gh_pr_head_shim,
    write_tea_pr_head_shim,
};
#[cfg(unix)]
pub(crate) mod ssh_trace;
pub(crate) mod wait;
mod zellij;

use std::sync::{Arc, Mutex};
#[cfg(unix)]
use std::time::Duration;

pub use codex::{trust_codex_hooks, trust_codex_preflight_hooks, trust_codex_project};
pub use command::{COMMAND_TIMEOUT, CommandTimeoutExt, ScrubSessionEnvExt};
pub use env::{Env, af_unix_bind_sandboxed, canonical, tmux_pane};
pub use harness::Harness;
pub use payloads::{
    claude_pre_tool_use_payload, codex_permission_payload, codex_pre_tool_use_payload,
    lifecycle_event, permission_payload, pi_tool_call_payload,
};
#[cfg(unix)]
pub(crate) use shim::codex_appserver_stub;
#[cfg(unix)]
pub use shim::{cargo_bin, zellij_trace_shim};
pub use shim::{hermetic_providers, provider_bin};
#[cfg(unix)]
pub use shim::{
    path_with_front, write_env_dump_shim, write_failing_agent_shim, write_fake_bash_shell,
    write_fake_login_shell, write_hook_firing_agent, write_long_lived_agent_shim, write_path_shim,
};
pub use skip::{AF_UNIX_SANDBOXED, skip};
pub use zellij::ZellijNamespace;

pub(crate) const ENV_PROBE_TIMEOUT_MS_ENV: &str = "RIMZ_TEST_ENV_PROBE_TIMEOUT_MS";

#[expect(
    clippy::print_stdout,
    reason = "live memory measurements are test output"
)]
pub(crate) fn assert_wrapper_memory_budget(label: &str, wrapper: u32, budget_mib: u64) {
    let rollup = std::fs::read_to_string(format!("/proc/{wrapper}/smaps_rollup"))
        .expect("wrapper smaps_rollup");
    let field = |key: &str| -> u64 {
        rollup
            .lines()
            .find_map(|line| {
                line.strip_prefix(key)
                    .and_then(|value| value.split_whitespace().next())
                    .and_then(|value| value.parse().ok())
            })
            .expect("smaps_rollup field")
    };
    let private_kib = field("Pss_Anon:");
    let huge_kib = field("AnonHugePages:");
    let outside_huge_kib = private_kib.saturating_sub(huge_kib);
    let enabled =
        std::fs::read_to_string("/sys/kernel/mm/transparent_hugepage/enabled").unwrap_or_default();
    let mode = enabled
        .split_whitespace()
        .find_map(|mode| mode.strip_prefix('[')?.strip_suffix(']'))
        .unwrap_or("unavailable");
    println!(
        "{label}: {private_kib} KiB Pss_Anon; {} KiB Pss; {outside_huge_kib} KiB outside huge \
         pages; AnonHugePages {huge_kib} KiB; THP {mode}",
        field("Pss:")
    );
    let huge_budget_mib = 16;
    if outside_huge_kib > budget_mib * 1024 || huge_kib > huge_budget_mib * 1024 {
        println!(
            "{label} mappings (load {}):\n{}",
            std::fs::read_to_string("/proc/loadavg")
                .unwrap_or_default()
                .trim(),
            std::fs::read_to_string(format!("/proc/{wrapper}/smaps")).expect("wrapper mappings")
        );
    }
    assert!(
        outside_huge_kib <= budget_mib * 1024,
        "{label} {outside_huge_kib} KiB outside huge pages exceeds {budget_mib} MiB; \
         private {private_kib} KiB; AnonHugePages {huge_kib} KiB; THP {mode}"
    );
    assert!(
        huge_kib <= huge_budget_mib * 1024,
        "{label} AnonHugePages {huge_kib} KiB exceeds {huge_budget_mib} MiB; \
         private {private_kib} KiB; outside huge pages {outside_huge_kib} KiB; THP {mode}"
    );
}

pub(crate) fn append_frame(
    env: &Env,
    frame: rimz::store::ingress::HookIngress,
) -> (rimz::store::ingress::HookIngress, u64) {
    let store = env.store();
    let lock =
        rimz::disk::lock::SharedAppendLock::acquire(&store.paths().hook_ingress_lock).unwrap();
    let end = rimz::store::ingress::append(store.paths(), &frame, &lock).unwrap();
    (frame, end)
}

pub fn trust_claude_project(env: &Env, root: &std::path::Path) {
    let path = env.home_root.join(".claude.json");
    let mut config: serde_json::Value = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).expect("Claude config"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(error) => panic!("read Claude config: {error}"),
    };
    config["projects"][root.to_str().unwrap()]["hasTrustDialogAccepted"] = true.into();
    std::fs::write(path, serde_json::to_vec(&config).unwrap()).expect("write Claude trust");
}

pub fn write_definition(
    env: &Env,
    namespace: &str,
    name: &str,
    frontmatter: &str,
    body: &str,
) -> std::path::PathBuf {
    if matches!(namespace, "agents" | "subagents") {
        let fields: serde_json::Value =
            serde_saphyr::from_str(frontmatter).expect("definition YAML");
        let kind = fields["agent"]
            .as_str()
            .filter(|kind| rimz::agents::find_definition(kind).is_some())
            .or_else(|| {
                fields["model"]
                    .as_str()
                    .and_then(rimz::agents::definition_model_kind)
            });
        let replaces_system_prompt = |kind: &str| {
            rimz::agents::find_definition(kind).is_some_and(|definition| {
                definition
                    .spec()
                    .launch
                    .preset_arg_matcher(rimz::agents::PresetField::SystemPromptFile)
                    .is_some()
            })
        };
        if let Some(kind) = kind.filter(|kind| replaces_system_prompt(kind)) {
            write_kind_base(env, kind);
        }
    }
    let directory = env.agents_home().join(namespace);
    std::fs::create_dir_all(&directory).expect("create definitions directory");
    let path = directory.join(format!("{name}.md"));
    std::fs::write(&path, format!("---\n{frontmatter}\n---\n{body}\n")).expect("write definition");
    path
}

pub fn write_kind_base(env: &Env, kind: &str) {
    if !env.agents_home().join(format!("agents/{kind}.md")).exists() {
        write_definition(
            env,
            "agents",
            kind,
            &format!("description: {kind} base"),
            &format!("{kind} base."),
        );
    }
}

#[cfg(unix)]
pub fn daemon_test_guard() -> rimz::disk::lock::WorkspaceLock {
    // Nextest gives every test its own process, so an in-process mutex leaves
    // the machine-wide listener and ttyd's ephemeral stock-index listener
    // contended. `cargo xtask test` gives the whole run one shared TMPDIR,
    // making this lock run-wide and isolated across runs.
    let path = std::env::temp_dir().join("rimz-web-daemon-tests.lock");
    rimz::disk::lock::WorkspaceLock::acquire_with_timeout(&path, Duration::from_secs(120))
        .unwrap_or_else(|err| panic!("acquire web daemon test lock {}: {err}", path.display()))
}

pub fn exec_args(env: &Env, request: &rimz::harness::launch::ExecRequest) -> Vec<String> {
    rimz::harness::launch::exec_argv(std::path::Path::new("rimz"), &env.runtime_paths(), request)
        .expect("encode hidden exec request")
        .into_iter()
        .skip(1)
        .collect()
}

/// The `loop show` refusal for a name with `count` retained runs, from `error: ` on. The age is
/// read back from `stderr`: a run the producer stamped just now can cross a `rel_age` boundary
/// between the subprocess and the assertion.
pub fn retained_runs_refusal(stderr: &str, name: &str, count: usize) -> String {
    let age = stderr
        .split_once(", last ")
        .and_then(|(_, rest)| rest.split_once(": `"))
        .map(|(age, _)| age)
        .unwrap_or_else(|| panic!("no retained-runs hint in {stderr:?}"));
    let noun = if count == 1 { "run" } else { "runs" };
    format!(
        "error: no loop task named `{name}`; see `rimz loop list`\n  {count} retained {noun}, last {age}: `rimz loop logs {name}`\n"
    )
}

const OUTPUT_TAIL_BYTES: usize = 64 * 1024;
const MAX_OSC_TITLE_BYTES: usize = 64 * 1024;

#[derive(Default)]
pub(crate) struct PtyRecorder {
    output: Arc<Mutex<PtyOutput>>,
}

#[derive(Default)]
struct PtyOutput {
    tail: Vec<u8>,
    titles: Vec<String>,
    marker: Option<OutputMarker>,
    screen: Option<vt100::Parser>,
}

impl PtyRecorder {
    pub(crate) fn with_screen(rows: u16, cols: u16) -> Self {
        Self {
            output: Arc::new(Mutex::new(PtyOutput {
                screen: Some(vt100::Parser::new(
                    rows.try_into().unwrap(),
                    cols.try_into().unwrap(),
                    0,
                )),
                ..Default::default()
            })),
        }
    }

    pub(crate) fn screen(&self) -> String {
        self.output
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .screen
            .as_ref()
            .expect("sized PTY recorder")
            .screen()
            .contents()
    }

    pub(crate) fn watch_output(&self, needle: &[u8]) {
        self.output
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .marker = Some(OutputMarker::new(needle));
    }

    pub(crate) fn output_seen(&self) -> bool {
        self.output
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .marker
            .as_ref()
            .is_some_and(|marker| marker.seen)
    }

    pub(crate) fn read_from(
        &self,
        mut reader: impl std::io::Read + Send + 'static,
    ) -> std::thread::JoinHandle<()> {
        let output = Arc::clone(&self.output);
        std::thread::spawn(move || {
            // A respawn keeps completed titles, but cannot complete the old PTY's partial OSC.
            let mut scanner = OscTitleScanner::default();
            let mut buffer = [0u8; 4096];
            loop {
                let read = match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => return,
                    Ok(read) => read,
                };
                let titles = scanner.feed(&buffer[..read]);
                let mut output = output
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(screen) = &mut output.screen {
                    screen.process(&buffer[..read]);
                }
                output.titles.extend(titles);
                if let Some(marker) = &mut output.marker {
                    marker.feed(&buffer[..read]);
                }
                let tail = &mut output.tail;
                tail.extend_from_slice(&buffer[..read]);
                let excess = tail.len().saturating_sub(OUTPUT_TAIL_BYTES);
                if excess > 0 {
                    tail.drain(..excess);
                }
            }
        })
    }

    pub(crate) fn output_bytes(&self) -> Vec<u8> {
        self.output
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .tail
            .clone()
    }

    pub(crate) fn titles(&self) -> Vec<String> {
        self.output
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .titles
            .clone()
    }
}

struct OutputMarker {
    needle: Vec<u8>,
    carry: Vec<u8>,
    seen: bool,
}

impl OutputMarker {
    fn new(needle: &[u8]) -> Self {
        assert!(!needle.is_empty(), "output marker must not be empty");
        Self {
            needle: needle.to_vec(),
            carry: Vec::new(),
            seen: false,
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        if self.seen {
            return;
        }
        self.carry.extend_from_slice(bytes);
        if self
            .carry
            .windows(self.needle.len())
            .any(|window| window == self.needle)
        {
            self.seen = true;
            self.carry.clear();
            return;
        }
        let excess = self.carry.len().saturating_sub(self.needle.len() - 1);
        self.carry.drain(..excess);
    }
}

#[derive(Default)]
struct OscTitleScanner {
    state: OscTitleState,
    payload: Vec<u8>,
}

#[derive(Clone, Copy, Default)]
enum OscTitleState {
    #[default]
    Ground,
    Escape,
    Osc,
    Separator,
    Payload,
    PayloadEscape,
    Discard,
    DiscardEscape,
}

impl OscTitleScanner {
    fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        use OscTitleState::*;

        let mut titles = Vec::new();
        for &byte in bytes {
            self.state = match (self.state, byte) {
                (Escape, b']') => Osc,
                (Osc, b'0' | b'1' | b'2') => Separator,
                (Separator, b';') => Payload,
                (Payload | PayloadEscape, 0x07) | (PayloadEscape, b'\\') => {
                    if byte == b'\\' {
                        self.payload.pop();
                    }
                    titles.push(String::from_utf8_lossy(&self.payload).into_owned());
                    self.payload.clear();
                    Ground
                }
                (Payload | PayloadEscape, 0x1b) => {
                    self.payload.push(byte);
                    PayloadEscape
                }
                (Payload | PayloadEscape, _) => {
                    self.payload.push(byte);
                    Payload
                }
                (Discard | DiscardEscape, 0x07) | (DiscardEscape, b'\\') => Ground,
                (Discard | DiscardEscape, 0x1b) => DiscardEscape,
                (Discard | DiscardEscape, _) => Discard,
                (_, 0x1b) => Escape,
                _ => Ground,
            };
            // Oversized sequences are ignored through their terminator, without growing carry.
            if self.payload.len() > MAX_OSC_TITLE_BYTES {
                self.payload.clear();
                self.state = if byte == 0x1b { DiscardEscape } else { Discard };
            }
        }
        titles
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_OSC_TITLE_BYTES, OUTPUT_TAIL_BYTES, OscTitleScanner, OutputMarker, PtyRecorder,
    };

    #[test]
    fn output_marker_is_independent_of_read_boundaries() {
        let stream = b"noiseTITLE_DONEtail";
        let marker_end = b"noiseTITLE_DONE".len();
        let mut marker = OutputMarker::new(b"TITLE_DONE");
        for (index, byte) in stream.iter().enumerate() {
            marker.feed(std::slice::from_ref(byte));
            assert_eq!(
                marker.seen,
                index + 1 >= marker_end,
                "one byte per read at {index}"
            );
        }
        for split in 0..=stream.len() {
            let mut marker = OutputMarker::new(b"TITLE_DONE");
            marker.feed(&stream[..split]);
            assert_eq!(marker.seen, split >= marker_end, "before split at {split}");
            marker.feed(&stream[split..]);
            assert!(marker.seen, "split at {split}");
        }
    }

    #[test]
    fn output_marker_ignores_partial_matches_and_bounds_carry() {
        let mut marker = OutputMarker::new(b"TITLE_DONE");
        marker.feed(b"TITLE_DON");
        assert!(!marker.seen);
        for _ in 0..32 {
            marker.feed(&[b'x'; 4096]);
            assert!(!marker.seen);
            assert!(marker.carry.len() < marker.needle.len());
        }
        marker.feed(b"TITLE_DONTITLE_DONE");
        assert!(marker.seen, "recover from an incomplete earlier match");
        marker.feed(b"later output");
        assert!(marker.seen, "a completed marker stays seen");
    }

    #[test]
    fn pty_recorder_retains_output_marker_after_byte_tail_eviction() {
        use std::io::Read;

        let recorder = PtyRecorder::default();
        recorder.watch_output(b"TITLE_DONE");
        let first = std::io::Cursor::new(b"\x1b]2;marvin@evil:~/leak\x07TITLE_");
        let mut rest = b"DONE".to_vec();
        rest.extend(vec![b'x'; OUTPUT_TAIL_BYTES + 4096]);
        recorder
            .read_from(first.chain(std::io::Cursor::new(rest)))
            .join()
            .expect("reader thread");
        assert!(recorder.output_seen(), "marker survives tail eviction");
        assert_eq!(recorder.titles(), ["marvin@evil:~/leak"]);
        assert_eq!(recorder.output_bytes(), vec![b'x'; OUTPUT_TAIL_BYTES]);
    }

    #[test]
    fn osc_titles_parses_bel_and_st_terminators() {
        assert_eq!(
            OscTitleScanner::default()
                .feed(b"noise\x1b]0;zero\x07\x1b]1;one\x1b\\\x1b]2;two\x07tail"),
            ["zero", "one", "two"],
        );
    }

    #[test]
    fn osc_titles_are_independent_of_read_boundaries() {
        let stream = b"noise\x1b]0;zero\x07\x1b]1;one\x1b\\\x1b]2;tw\xc3\xb6;two\x07tail";
        let expected = ["zero", "one", "twö;two"];
        let mut scanner = OscTitleScanner::default();
        let titles: Vec<_> = stream
            .iter()
            .flat_map(|byte| scanner.feed(std::slice::from_ref(byte)))
            .collect();
        assert_eq!(titles, expected, "one byte per read");
        for split in 0..=stream.len() {
            let mut scanner = OscTitleScanner::default();
            let mut titles = scanner.feed(&stream[..split]);
            titles.extend(scanner.feed(&stream[split..]));
            assert_eq!(titles, expected, "split at {split}");
        }
    }

    #[test]
    fn osc_titles_ignore_other_osc_and_unterminated_sequences() {
        assert_eq!(
            OscTitleScanner::default()
                .feed(b"\x1b]8;;https://example.com\x07link\x1b]8;;\x1b\\\x1b]2;title\x07\x1b]0;unfinished\x1b"),
            ["title"],
        );
    }

    #[test]
    fn osc_titles_bound_unterminated_payload_and_recover() {
        let mut scanner = OscTitleScanner::default();
        assert!(scanner.feed(b"\x1b]2;").is_empty());
        for _ in 0..32 {
            assert!(scanner.feed(&[b'x'; 4096]).is_empty());
            assert!(scanner.payload.len() <= MAX_OSC_TITLE_BYTES);
        }
        assert_eq!(
            scanner.feed(b"\x1b\\\x1b]1;after\x07"),
            ["after"],
            "discard the oversized title, then resume scanning",
        );
    }

    #[test]
    fn pty_recorder_retains_titles_after_byte_tail_eviction() {
        let recorder = PtyRecorder::default();
        let mut stream = b"\x1b]0;room | shell\x07\x1b]2;marvin@evil:~/leak\x1b\\".to_vec();
        stream.extend(vec![b'x'; OUTPUT_TAIL_BYTES + 4096]);
        recorder
            .read_from(std::io::Cursor::new(stream))
            .join()
            .expect("reader thread");
        assert_eq!(recorder.titles(), ["room | shell", "marvin@evil:~/leak"]);
        assert_eq!(recorder.output_bytes(), vec![b'x'; OUTPUT_TAIL_BYTES]);
    }

    #[test]
    fn pty_recorder_keeps_completed_titles_but_not_partial_sequences_across_readers() {
        let recorder = PtyRecorder::default();
        let first = b"\x1b]0;first\x07\x1b]2;partial";
        let second = b"suffix\x07\x1b]1;second\x1b\\";
        for stream in [first.as_slice(), second.as_slice()] {
            recorder
                .read_from(std::io::Cursor::new(stream.to_vec()))
                .join()
                .expect("reader thread");
        }
        assert_eq!(recorder.titles(), ["first", "second"]);
        assert_eq!(
            recorder.output_bytes(),
            [first.as_slice(), second.as_slice()].concat()
        );
    }
}
