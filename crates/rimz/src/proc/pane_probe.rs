//! Pane-local process probes layered on the minimal process reader.

use std::path::{Path, PathBuf};

use crate::ids::AgentKind;
use crate::pane::ElevatedAgent;

/// Maximum process-tree depth walked below a pane root when looking for an
/// elevated or pane-hosted agent. `sudo su` + login shell + node launcher +
/// agent is shallow; this cap keeps a pathological pane from turning a sidebar
/// tick into an unbounded tree walk.
const PANE_AGENT_DESCENT_DEPTH: usize = 8;

/// A live in-pane agent CLI found below a pane root for a requested kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InPaneAgentProcess {
    pub pid: u32,
    pub started_at: jiff::Timestamp,
    pub cwd: Option<PathBuf>,
}

/// A live known agent CLI proven along a pane root's single-child process chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostedAgentProcess {
    pub kind: AgentKind,
    pub pid: u32,
    pub started_at: jiff::Timestamp,
    pub cwd: Option<PathBuf>,
}

/// Whether a mux foreground command is an elevation entrypoint. The producer
/// uses this as the cheap gate before walking descendants of a pane root.
pub(crate) fn command_starts_with_elevation_wrapper(command: &str) -> bool {
    command
        .split_whitespace()
        .next()
        .map(basename)
        .is_some_and(is_elevation_wrapper)
}

/// A different-real-uid agent descendant under an elevation wrapper in this
/// pane, if one is visible through the process backend. The marker is display-only; callers
/// must keep the pane's original command unchanged so the sidebar never binds a
/// foreign-user agent as a local store session.
pub fn elevated_in_pane_agent(pane_pid: u32) -> Option<ElevatedAgent> {
    elevated_in_pane_agent_with(
        pane_pid,
        crate::proc::own_uid()?,
        &|pid| crate::proc::children(pid),
        &|pid| crate::proc::cmdline(pid),
        &|pid| crate::proc::comm(pid),
        &|pid| crate::proc::real_uid(pid),
    )
}

fn elevated_in_pane_agent_with(
    pane_pid: u32,
    own_uid: u32,
    children: &dyn Fn(u32) -> Vec<u32>,
    cmdline: &dyn Fn(u32) -> Option<String>,
    comm: &dyn Fn(u32) -> Option<String>,
    real_uid: &dyn Fn(u32) -> Option<u32>,
) -> Option<ElevatedAgent> {
    let mut stack = vec![(pane_pid, 0, false)];
    let mut seen = std::collections::HashSet::new();
    while let Some((pid, depth, wrapper_seen)) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        let command = cmdline(pid).unwrap_or_default();
        let comm = comm(pid);
        let wrapper_seen = wrapper_seen || command_starts_with_elevation_wrapper(&command);
        if wrapper_seen
            && let Some(kind) =
                crate::agents::registry::command_agent_kind_with_comm(&command, comm.as_deref())
            && let Some(uid) = real_uid(pid)
            && uid != own_uid
        {
            return Some(ElevatedAgent {
                kind: AgentKind::new_unchecked(kind),
                uid,
            });
        }
        if depth >= PANE_AGENT_DESCENT_DEPTH {
            continue;
        }
        for child in children(pid) {
            stack.push((child, depth + 1, wrapper_seen));
        }
    }
    None
}

fn is_elevation_wrapper(program: &str) -> bool {
    matches!(program, "sudo" | "su" | "doas")
}

fn basename(token: &str) -> &str {
    Path::new(token)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(token)
}

/// Start time of the in-pane agent CLI process backing a live pane, found by
/// working directory. This is the exact single-process case only: a cwd with no
/// match or multiple same-kind agent CLIs abstains so callers keep pane starts
/// unknown rather than duplicate one cwd-level timestamp across several panes.
pub fn in_pane_agent_start(kind: &str, pane_cwd: &str) -> Option<jiff::Timestamp> {
    let starts = in_pane_agent_starts(kind, pane_cwd);
    (starts.len() == 1).then_some(starts[0])
}

/// Start times for in-pane agent CLI processes whose process cwd equals
/// `pane_cwd`. Callers that know other panes' exact starts subtract those before
/// deciding whether one unaccounted process remains.
pub fn in_pane_agent_starts(kind: &str, pane_cwd: &str) -> Vec<jiff::Timestamp> {
    if !in_pane_agent_probe_supported(kind) {
        return Vec::new();
    }
    let pane_cwd = Path::new(pane_cwd);
    let mut starts = crate::proc::list_processes()
        .into_iter()
        .filter(|process| in_pane_agent_cmdline_matches(kind, &process.cmdline))
        .filter(|process| crate::proc::cwd(process.pid).as_deref() == Some(pane_cwd))
        .filter_map(|process| crate::proc::process_start(process.pid))
        .collect::<Vec<_>>();
    starts.sort();
    starts.dedup();
    starts
}

/// Start time of the in-pane agent CLI behind a pane's bound root process —
/// the per-pane exact signal the frame stamp prefers over the cwd scan above.
/// The root is the CLI itself when its cmdline reads as the agent TUI (a pane
/// running it directly); shell-hosted and wrapper-hosted CLIs are accepted only
/// along a single-child chain from the pane root. The cmdline check is
/// load-bearing twice over: a shell outlives the agents it hosts, so stamping
/// its older start would re-admit the very sessions `pane_start_allows_bind`
/// refuses, and a re-run CLI is a fresh child pid even when the hosting shell
/// survives, so re-tenancy stays visible. `None` for an unknown kind, a branchy
/// process tree, or when no descendant reads as the CLI, so the caller falls
/// back rather than guesses.
pub fn in_pane_agent_start_for_root(kind: &str, root_pid: u32) -> Option<jiff::Timestamp> {
    in_pane_agent_process_for_root(kind, root_pid).map(|process| process.started_at)
}

/// The requested in-pane agent CLI process backing a live pane, including its cwd
/// when readable. RimZ walks only a single-child chain from the pane root:
/// direct agent launches, shell-hosted agents, and wrapper-spawned subshells
/// such as `chezmoi cd -> zsh -> codex` are unambiguous, while a branching
/// process tree abstains.
pub fn in_pane_agent_process_for_root(kind: &str, root_pid: u32) -> Option<InPaneAgentProcess> {
    in_pane_agent_process_for_root_with(
        kind,
        root_pid,
        &crate::proc::cmdline,
        &crate::proc::children,
        &crate::proc::process_start,
        &crate::proc::cwd,
    )
}

/// The outermost known agent CLI hosted below a pane root. RimZ classifies the
/// full command line at each node on one single-child walk, and abstains when
/// the tree cannot prove one unambiguous live CLI.
pub fn hosted_agent_process_for_root(root_pid: u32) -> Option<HostedAgentProcess> {
    hosted_agent_process_for_root_with(
        root_pid,
        &crate::proc::cmdline,
        &crate::proc::children,
        &crate::proc::process_start,
        &crate::proc::cwd,
    )
}

/// Whether an agent of the requested kind is authoritatively absent below a pane root.
/// This is stricter than [`in_pane_agent_process_for_root`]: unreadable
/// cmdlines, branching trees, and depth exhaustion are indeterminate, so they
/// return `false` and keep callers on their transient-miss path.
pub fn hosted_agent_absent_under_root(kind: &str, root_pid: u32) -> bool {
    hosted_agent_absent_under_root_with(
        kind,
        root_pid,
        &crate::proc::cmdline,
        &crate::proc::children,
    )
}

fn in_pane_agent_process_for_root_with(
    kind: &str,
    root_pid: u32,
    cmdline: &dyn Fn(u32) -> Option<String>,
    children: &dyn Fn(u32) -> Vec<u32>,
    process_start: &dyn Fn(u32) -> Option<jiff::Timestamp>,
    cwd: &dyn Fn(u32) -> Option<PathBuf>,
) -> Option<InPaneAgentProcess> {
    if !in_pane_agent_probe_supported(kind) {
        return None;
    }
    pane_agent_process_for_root_with(
        root_pid,
        &|cmdline| {
            in_pane_agent_cmdline_matches(kind, cmdline)
                .then(|| AgentKind::new_unchecked(kind.to_owned()))
        },
        cmdline,
        children,
        process_start,
        cwd,
    )
    .map(|process| InPaneAgentProcess {
        pid: process.pid,
        started_at: process.started_at,
        cwd: process.cwd,
    })
}

fn hosted_agent_process_for_root_with(
    root_pid: u32,
    cmdline: &dyn Fn(u32) -> Option<String>,
    children: &dyn Fn(u32) -> Vec<u32>,
    process_start: &dyn Fn(u32) -> Option<jiff::Timestamp>,
    cwd: &dyn Fn(u32) -> Option<PathBuf>,
) -> Option<HostedAgentProcess> {
    pane_agent_process_for_root_with(
        root_pid,
        &|cmdline| {
            crate::agents::registry::command_agent_kind(cmdline).map(AgentKind::new_unchecked)
        },
        cmdline,
        children,
        process_start,
        cwd,
    )
}

fn pane_agent_process_for_root_with(
    root_pid: u32,
    classify: &dyn Fn(&str) -> Option<AgentKind>,
    cmdline: &dyn Fn(u32) -> Option<String>,
    children: &dyn Fn(u32) -> Vec<u32>,
    process_start: &dyn Fn(u32) -> Option<jiff::Timestamp>,
    cwd: &dyn Fn(u32) -> Option<PathBuf>,
) -> Option<HostedAgentProcess> {
    let mut pid = root_pid;
    for _ in 0..=PANE_AGENT_DESCENT_DEPTH {
        let command = cmdline(pid)?;
        if let Some(kind) = classify(&command) {
            return Some(HostedAgentProcess {
                kind,
                pid,
                started_at: process_start(pid)?,
                cwd: cwd(pid),
            });
        }
        let children = children(pid);
        let [child] = children.as_slice() else {
            return None;
        };
        pid = *child;
    }
    None
}

fn hosted_agent_absent_under_root_with(
    kind: &str,
    root_pid: u32,
    cmdline: &dyn Fn(u32) -> Option<String>,
    children: &dyn Fn(u32) -> Vec<u32>,
) -> bool {
    if !in_pane_agent_probe_supported(kind) {
        return false;
    }
    let mut pid = root_pid;
    for _ in 0..=PANE_AGENT_DESCENT_DEPTH {
        let Some(cmdline) = cmdline(pid) else {
            return false;
        };
        if in_pane_agent_cmdline_matches(kind, &cmdline) {
            return false;
        }
        let children = children(pid);
        match children.as_slice() {
            [] => return true,
            [child] => pid = *child,
            _ => return false,
        }
    }
    false
}

fn in_pane_agent_cmdline_matches(kind: &str, cmdline: &str) -> bool {
    // The caller already owns provider identity from a durable session or hook.
    // Admit a colliding native basename for liveness without exposing that
    // candidate to generic pane classification.
    crate::agents::registry::command_may_be_agent_kind(cmdline, kind)
}

fn in_pane_agent_probe_supported(kind: &str) -> bool {
    crate::agents::spec_by_kind(kind).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;

    #[test]
    fn elevation_wrapper_gate_matches_supported_entrypoints() {
        for (command, expected) in [
            ("sudo su", true),
            ("/usr/bin/doas claude", true),
            ("su -", true),
            ("claude", false),
            ("zsh", false),
        ] {
            assert_eq!(command_starts_with_elevation_wrapper(command), expected);
        }
    }

    #[test]
    fn elevated_agent_scan_requires_wrapper_and_foreign_uid() {
        const CLAUDE_NODE: &str = "node /opt/node_modules/@anthropic-ai/claude-code/cli.js";
        let cases = [
            (
                chain([
                    (10, "zsh"),
                    (20, "sudo su"),
                    (21, "-bash"),
                    (22, CLAUDE_NODE),
                ])
                .uids([21, 22], 0)
                .comm(22, "node"),
                Some(("claude", 0)),
            ),
            (
                chain([(10, "zsh"), (20, "sudo su"), (21, "-bash"), (22, "")])
                    .uids([21, 22], 0)
                    .comm(22, "claude"),
                Some(("claude", 0)),
            ),
            (
                chain([(10, "zsh"), (20, "sudo -u root claude")]).uids([20], 0),
                Some(("claude", 0)),
            ),
            (chain([(10, "zsh"), (20, "sudo su"), (21, "claude")]), None),
            (chain([(10, "zsh"), (20, "claude")]).uids([20], 0), None),
        ];

        for (fixture, expected) in cases {
            let actual = fixture.elevated_agent(10, 1_000);
            assert_eq!(
                actual
                    .as_ref()
                    .map(|agent| (agent.kind.as_str(), agent.uid)),
                expected
            );
        }
    }

    #[test]
    fn requested_agent_process_finds_exact_supported_cli() {
        let start = timestamp("2026-06-30T11:18:03Z");
        let cwd = PathBuf::from("/home/marvin/.local/share/chezmoi");
        let cases = [
            (
                "codex",
                chain([
                    (10, "zsh"),
                    (20, "chezmoi cd"),
                    (30, "/bin/zsh"),
                    (40, "codex"),
                ])
                .live(40, start, Some(&cwd)),
                Some((40, start, Some(cwd.clone()))),
            ),
            (
                "qwen",
                chain([(10, "qwen")]).live(10, start, None),
                Some((10, start, None)),
            ),
            (
                "codex",
                chain([(10, "zsh"), (20, "codex"), (30, "make")]).branch(10, [20, 30]),
                None,
            ),
        ];

        for (kind, fixture, expected) in cases {
            let actual = fixture
                .in_pane_agent(kind, 10)
                .map(|process| (process.pid, process.started_at, process.cwd));
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn hosted_agent_process_finds_outermost_qwen_cli() {
        let start = timestamp("2026-07-01T10:00:00Z");
        let cwd = PathBuf::from("/repo/qwen");
        let qwen = "node --expose-gc /home/u/.local/lib/qwen-code/lib/cli.js";
        let fixture = chain([(10, "zsh"), (20, qwen), (30, qwen)])
            .live(20, start, Some(&cwd))
            .live(30, start, None);

        let found = fixture.hosted_agent(10).expect("outer Qwen CLI");
        assert_eq!(found.kind.as_str(), "qwen");
        assert_eq!(
            (found.pid, found.started_at, found.cwd),
            (20, start, Some(cwd))
        );
    }

    #[test]
    fn hosted_agent_process_requires_complete_live_proof() {
        let start = timestamp("2026-07-01T10:00:00Z");
        let cases = [
            chain([(10, "zsh"), (20, "qwen"), (30, "make")]).branch(10, [20, 30]),
            chain([(10, "zsh"), (20, "qwen")]).unreadable(20),
            ProcFixture::depth_bound(),
            chain([(10, "codex app-server")]).live(10, start, None),
            chain([(10, "qwen")]),
        ];

        for fixture in cases {
            assert_eq!(fixture.hosted_agent(10), None);
        }
    }

    #[test]
    fn hosted_agent_absence_is_a_three_state_proof() {
        let start = timestamp("2026-06-30T11:18:03Z");
        let cases = [
            (
                "codex",
                chain([(10, "zsh"), (20, "chezmoi cd"), (30, "/bin/zsh")]),
                (false, true),
            ),
            (
                "codex",
                chain([(10, "zsh"), (20, "chezmoi cd"), (30, "codex")]).live(30, start, None),
                (true, false),
            ),
            (
                "codex",
                chain([(10, "zsh"), (20, "codex"), (30, "make")]).branch(10, [20, 30]),
                (false, false),
            ),
            (
                "codex",
                chain([(10, "zsh"), (20, "codex")]).unreadable(20),
                (false, false),
            ),
            ("codex", ProcFixture::depth_bound(), (false, false)),
            ("unknown", chain([(10, "unknown")]), (false, false)),
        ];

        for (kind, fixture, expected) in cases {
            let actual = (
                fixture.in_pane_agent(kind, 10).is_some(),
                fixture.hosted_absent(kind, 10),
            );
            assert_eq!(actual, expected);
        }
    }

    fn timestamp(value: &str) -> jiff::Timestamp {
        value.parse().unwrap()
    }

    fn chain<const N: usize>(nodes: [(u32, &'static str); N]) -> ProcFixture {
        ProcFixture::chain(nodes)
    }

    #[derive(Default)]
    struct ProcNode {
        uid: u32,
        comm: Option<&'static str>,
        cmdline: Option<&'static str>,
        children: Vec<u32>,
        started_at: Option<jiff::Timestamp>,
        cwd: Option<PathBuf>,
    }

    struct ProcFixture {
        nodes: BTreeMap<u32, ProcNode>,
    }

    impl ProcFixture {
        fn chain(nodes: impl IntoIterator<Item = (u32, &'static str)>) -> Self {
            let entries = nodes.into_iter().collect::<Vec<_>>();
            let mut nodes = entries
                .iter()
                .map(|&(pid, command)| {
                    (
                        pid,
                        ProcNode {
                            uid: 1_000,
                            cmdline: Some(command),
                            ..ProcNode::default()
                        },
                    )
                })
                .collect::<BTreeMap<_, _>>();
            for pair in entries.windows(2) {
                nodes.get_mut(&pair[0].0).unwrap().children.push(pair[1].0);
            }
            Self { nodes }
        }

        fn depth_bound() -> Self {
            Self::chain((10..=19).map(|pid| (pid, if pid == 19 { "qwen" } else { "zsh" }))).live(
                19,
                timestamp("2026-07-01T10:00:00Z"),
                None,
            )
        }

        fn uids(mut self, pids: impl IntoIterator<Item = u32>, uid: u32) -> Self {
            for pid in pids {
                self.nodes.get_mut(&pid).unwrap().uid = uid;
            }
            self
        }

        fn comm(mut self, pid: u32, comm: &'static str) -> Self {
            self.nodes.get_mut(&pid).unwrap().comm = Some(comm);
            self
        }

        fn unreadable(mut self, pid: u32) -> Self {
            self.nodes.get_mut(&pid).unwrap().cmdline = None;
            self
        }

        fn branch(mut self, pid: u32, children: impl IntoIterator<Item = u32>) -> Self {
            self.nodes.get_mut(&pid).unwrap().children = children.into_iter().collect();
            self
        }

        fn live(mut self, pid: u32, started_at: jiff::Timestamp, cwd: Option<&Path>) -> Self {
            let node = self.nodes.get_mut(&pid).unwrap();
            node.started_at = Some(started_at);
            node.cwd = cwd.map(Path::to_path_buf);
            self
        }

        fn cmdline(&self, pid: u32) -> Option<String> {
            self.nodes.get(&pid)?.cmdline.map(str::to_owned)
        }

        fn children(&self, pid: u32) -> Vec<u32> {
            self.nodes
                .get(&pid)
                .map_or_else(Vec::new, |node| node.children.clone())
        }

        fn in_pane_agent(&self, kind: &str, root: u32) -> Option<InPaneAgentProcess> {
            in_pane_agent_process_for_root_with(
                kind,
                root,
                &|pid| self.cmdline(pid),
                &|pid| self.children(pid),
                &|pid| self.nodes.get(&pid)?.started_at,
                &|pid| self.nodes.get(&pid)?.cwd.clone(),
            )
        }

        fn hosted_agent(&self, root: u32) -> Option<HostedAgentProcess> {
            hosted_agent_process_for_root_with(
                root,
                &|pid| self.cmdline(pid),
                &|pid| self.children(pid),
                &|pid| self.nodes.get(&pid)?.started_at,
                &|pid| self.nodes.get(&pid)?.cwd.clone(),
            )
        }

        fn hosted_absent(&self, kind: &str, root: u32) -> bool {
            hosted_agent_absent_under_root_with(kind, root, &|pid| self.cmdline(pid), &|pid| {
                self.children(pid)
            })
        }

        fn elevated_agent(&self, root: u32, own_uid: u32) -> Option<ElevatedAgent> {
            elevated_in_pane_agent_with(
                root,
                own_uid,
                &|pid| self.children(pid),
                &|pid| self.cmdline(pid),
                &|pid| self.nodes.get(&pid)?.comm.map(str::to_owned),
                &|pid| self.nodes.get(&pid).map(|node| node.uid),
            )
        }
    }
}
