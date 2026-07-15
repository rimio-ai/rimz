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
                crate::store::snapshot::command_agent_kind_with_comm(&command, comm.as_deref())
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
            let kind = crate::store::snapshot::command_agent_kind(cmdline)?;
            (kind != "codex" || crate::agents::codex::is_codex_cli_cmdline(cmdline))
                .then(|| AgentKind::new_unchecked(kind))
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
    if kind == "codex" {
        return crate::agents::codex::is_codex_cli_cmdline(cmdline);
    }
    crate::store::snapshot::command_agent_kind(cmdline) == Some(kind)
}

fn in_pane_agent_probe_supported(kind: &str) -> bool {
    crate::agents::descriptor_by_kind(kind).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;

    #[test]
    fn elevation_wrapper_gate_reads_the_entrypoint() {
        assert!(command_starts_with_elevation_wrapper("sudo su"));
        assert!(command_starts_with_elevation_wrapper(
            "/usr/bin/doas claude"
        ));
        assert!(command_starts_with_elevation_wrapper("su -"));
        assert!(!command_starts_with_elevation_wrapper("claude"));
        assert!(!command_starts_with_elevation_wrapper("zsh"));
    }

    #[test]
    fn elevated_agent_scan_detects_foreign_uid_descendant_past_sudo_su() {
        let fixture = ProcFixture::new([
            ProcNode::new(10, 1_000, "zsh", &[20]),
            ProcNode::new(20, 1_000, "sudo su", &[21]),
            ProcNode::new(21, 0, "-bash", &[22]),
            ProcNode::new(
                22,
                0,
                "node /opt/node_modules/@anthropic-ai/claude-code/cli.js",
                &[],
            )
            .with_comm("node"),
        ]);

        let elevated = fixture.elevated_agent(10, 1_000).expect("foreign agent");

        assert_eq!(elevated.kind.as_str(), "claude");
        assert_eq!(elevated.uid, 0);
    }

    #[test]
    fn elevated_agent_scan_can_fall_back_to_precise_comm() {
        let fixture = ProcFixture::new([
            ProcNode::new(10, 1_000, "zsh", &[20]),
            ProcNode::new(20, 1_000, "sudo su", &[21]),
            ProcNode::new(21, 0, "-bash", &[22]),
            ProcNode::new(22, 0, "", &[]).with_comm("claude"),
        ]);

        let elevated = fixture.elevated_agent(10, 1_000).expect("foreign agent");

        assert_eq!(elevated.kind.as_str(), "claude");
        assert_eq!(elevated.uid, 0);
    }

    #[test]
    fn elevated_agent_scan_detects_direct_sudo_agent() {
        let fixture = ProcFixture::new([
            ProcNode::new(10, 1_000, "zsh", &[20]),
            ProcNode::new(20, 0, "sudo -u root claude", &[]),
        ]);

        let elevated = fixture.elevated_agent(10, 1_000).expect("foreign agent");

        assert_eq!(elevated.kind.as_str(), "claude");
        assert_eq!(elevated.uid, 0);
    }

    #[test]
    fn elevated_agent_scan_ignores_same_uid_and_non_wrapper_paths() {
        let same_uid = ProcFixture::new([
            ProcNode::new(10, 1_000, "zsh", &[20]),
            ProcNode::new(20, 1_000, "sudo su", &[21]),
            ProcNode::new(21, 1_000, "claude", &[]),
        ]);
        assert_eq!(same_uid.elevated_agent(10, 1_000), None);

        let no_wrapper = ProcFixture::new([
            ProcNode::new(10, 1_000, "zsh", &[20]),
            ProcNode::new(20, 0, "claude", &[]),
        ]);
        assert_eq!(no_wrapper.elevated_agent(10, 1_000), None);
    }

    #[test]
    fn in_pane_agent_process_walks_wrapper_shell_chain() {
        let start: jiff::Timestamp = "2026-06-30T11:18:03Z".parse().unwrap();
        let cwd = PathBuf::from("/home/marvin/.local/share/chezmoi");
        let fixture = ProcFixture::new([
            ProcNode::new(10, 1_000, "zsh", &[20]),
            ProcNode::new(20, 1_000, "chezmoi cd", &[30]),
            ProcNode::new(30, 1_000, "/bin/zsh", &[40]),
            ProcNode::new(40, 1_000, "codex", &[]),
        ]);

        let found = in_pane_agent_process_for_root_with(
            "codex",
            10,
            &|pid| fixture.cmdline(pid),
            &|pid| fixture.children(pid),
            &|pid| (pid == 40).then_some(start),
            &|pid| (pid == 40).then_some(cwd.clone()),
        );

        assert_eq!(
            found,
            Some(InPaneAgentProcess {
                pid: 40,
                started_at: start,
                cwd: Some(cwd)
            })
        );
    }

    #[test]
    fn hosted_agent_process_finds_outer_qwen_cli_in_one_walk() {
        let start: jiff::Timestamp = "2026-07-01T10:00:00Z".parse().unwrap();
        let cwd = PathBuf::from("/repo/qwen");
        let qwen = "node --expose-gc /home/u/.local/lib/qwen-code/lib/cli.js";
        let fixture = ProcFixture::new([
            ProcNode::new(10, 1_000, "zsh", &[20]),
            ProcNode::new(20, 1_000, qwen, &[30]),
            ProcNode::new(30, 1_000, qwen, &[]),
        ]);
        let cmdline_reads = std::cell::RefCell::new(Vec::new());
        let child_reads = std::cell::RefCell::new(Vec::new());

        let found = hosted_agent_process_for_root_with(
            10,
            &|pid| {
                cmdline_reads.borrow_mut().push(pid);
                fixture.cmdline(pid)
            },
            &|pid| {
                child_reads.borrow_mut().push(pid);
                fixture.children(pid)
            },
            &|pid| (pid == 20).then_some(start),
            &|pid| (pid == 20).then_some(cwd.clone()),
        );

        assert_eq!(
            found,
            Some(HostedAgentProcess {
                kind: AgentKind::new_unchecked("qwen"),
                pid: 20,
                started_at: start,
                cwd: Some(cwd),
            })
        );
        assert_eq!(*cmdline_reads.borrow(), vec![10, 20]);
        assert_eq!(*child_reads.borrow(), vec![10]);
    }

    #[test]
    fn kind_specific_probe_accepts_eager_qwen() {
        let start: jiff::Timestamp = "2026-07-01T10:00:00Z".parse().unwrap();
        let fixture = ProcFixture::new([ProcNode::new(10, 1_000, "qwen", &[])]);

        assert_eq!(
            in_pane_agent_process_for_root_with(
                "qwen",
                10,
                &|pid| fixture.cmdline(pid),
                &|pid| fixture.children(pid),
                &|_| Some(start),
                &|_| None,
            ),
            Some(InPaneAgentProcess {
                pid: 10,
                started_at: start,
                cwd: None,
            })
        );
    }

    #[test]
    fn hosted_agent_process_abstains_without_complete_linear_proof() {
        let branch = ProcFixture::new([
            ProcNode::new(10, 1_000, "zsh", &[20, 30]),
            ProcNode::new(20, 1_000, "qwen", &[]),
            ProcNode::new(30, 1_000, "make", &[]),
        ]);
        let deep = ProcFixture::new([
            ProcNode::new(10, 1_000, "zsh", &[11]),
            ProcNode::new(11, 1_000, "zsh", &[12]),
            ProcNode::new(12, 1_000, "zsh", &[13]),
            ProcNode::new(13, 1_000, "zsh", &[14]),
            ProcNode::new(14, 1_000, "zsh", &[15]),
            ProcNode::new(15, 1_000, "zsh", &[16]),
            ProcNode::new(16, 1_000, "zsh", &[17]),
            ProcNode::new(17, 1_000, "zsh", &[18]),
            ProcNode::new(18, 1_000, "zsh", &[19]),
            ProcNode::new(19, 1_000, "qwen", &[]),
        ]);
        let unreadable = ProcFixture::new([
            ProcNode::new(10, 1_000, "zsh", &[20]),
            ProcNode::new(20, 1_000, "qwen", &[]).with_unreadable_cmdline(),
        ]);

        for fixture in [&branch, &deep, &unreadable] {
            assert_eq!(
                hosted_agent_process_for_root_with(
                    10,
                    &|pid| fixture.cmdline(pid),
                    &|pid| fixture.children(pid),
                    &|_| panic!("indeterminate proof must not read process starts"),
                    &|_| panic!("indeterminate proof must not read cwds"),
                ),
                None
            );
        }
    }

    #[test]
    fn hosted_agent_process_rejects_shared_runtimes_and_codex_daemons() {
        let start: jiff::Timestamp = "2026-07-01T10:00:00Z".parse().unwrap();
        for command in ["node", "codex app-server", "codex remote-control start"] {
            let fixture = ProcFixture::new([ProcNode::new(10, 1_000, command, &[])]);
            assert_eq!(
                hosted_agent_process_for_root_with(
                    10,
                    &|pid| fixture.cmdline(pid),
                    &|pid| fixture.children(pid),
                    &|_| Some(start),
                    &|_| None,
                ),
                None,
                "{command}"
            );
        }

        let qwen = ProcFixture::new([ProcNode::new(10, 1_000, "qwen", &[])]);
        assert_eq!(
            hosted_agent_process_for_root_with(
                10,
                &|pid| qwen.cmdline(pid),
                &|pid| qwen.children(pid),
                &|_| None,
                &|_| panic!("startless proof must not read cwd"),
            ),
            None,
            "a classified CLI without a process start is not proven live"
        );
    }

    #[test]
    fn in_pane_agent_process_abstains_on_branching_tree() {
        let fixture = ProcFixture::new([
            ProcNode::new(10, 1_000, "zsh", &[20, 30]),
            ProcNode::new(20, 1_000, "codex", &[]),
            ProcNode::new(30, 1_000, "make", &[]),
        ]);

        assert_eq!(
            in_pane_agent_process_for_root_with(
                "codex",
                10,
                &|pid| fixture.cmdline(pid),
                &|pid| fixture.children(pid),
                &|_| panic!("branching tree must not read process starts"),
                &|_| panic!("branching tree must not read cwds"),
            ),
            None
        );
    }

    #[test]
    fn hosted_agent_absent_detects_clean_linear_trees_without_agent() {
        let bare = ProcFixture::new([ProcNode::new(10, 1_000, "zsh", &[])]);
        assert!(bare.hosted_absent("codex", 10));

        let wrapped = ProcFixture::new([
            ProcNode::new(10, 1_000, "zsh", &[20]),
            ProcNode::new(20, 1_000, "chezmoi cd", &[30]),
            ProcNode::new(30, 1_000, "/bin/zsh", &[]),
        ]);
        assert!(wrapped.hosted_absent("codex", 10));
    }

    #[test]
    fn hosted_agent_absent_abstains_when_agent_may_be_present() {
        let wrapper_with_codex = ProcFixture::new([
            ProcNode::new(10, 1_000, "zsh", &[20]),
            ProcNode::new(20, 1_000, "chezmoi cd", &[30]),
            ProcNode::new(30, 1_000, "/bin/zsh", &[40]),
            ProcNode::new(40, 1_000, "codex", &[]),
        ]);
        assert!(!wrapper_with_codex.hosted_absent("codex", 10));

        let codex_with_child = ProcFixture::new([
            ProcNode::new(10, 1_000, "zsh", &[20]),
            ProcNode::new(20, 1_000, "codex", &[30]),
            ProcNode::new(30, 1_000, "bash", &[]),
        ]);
        assert!(!codex_with_child.hosted_absent("codex", 10));

        let branch = ProcFixture::new([
            ProcNode::new(10, 1_000, "zsh", &[20, 30]),
            ProcNode::new(20, 1_000, "codex", &[]),
            ProcNode::new(30, 1_000, "make", &[]),
        ]);
        assert!(!branch.hosted_absent("codex", 10));
    }

    #[test]
    fn hosted_agent_absent_abstains_on_indeterminate_scans() {
        assert!(!hosted_agent_absent_under_root_with(
            "codex",
            10,
            &|_| None,
            &|pid| panic!("unreadable cmdline must not descend into {pid}"),
        ));

        let deep = ProcFixture::new([
            ProcNode::new(10, 1_000, "zsh", &[11]),
            ProcNode::new(11, 1_000, "zsh", &[12]),
            ProcNode::new(12, 1_000, "zsh", &[13]),
            ProcNode::new(13, 1_000, "zsh", &[14]),
            ProcNode::new(14, 1_000, "zsh", &[15]),
            ProcNode::new(15, 1_000, "zsh", &[16]),
            ProcNode::new(16, 1_000, "zsh", &[17]),
            ProcNode::new(17, 1_000, "zsh", &[18]),
            ProcNode::new(18, 1_000, "zsh", &[19]),
            ProcNode::new(19, 1_000, "zsh", &[]),
        ]);
        assert!(!deep.hosted_absent("codex", 10));

        let eager = ProcFixture::new([ProcNode::new(10, 1_000, "qwen", &[])]);
        assert!(!eager.hosted_absent("qwen", 10));

        let unknown = ProcFixture::new([ProcNode::new(10, 1_000, "unknown", &[])]);
        assert!(!unknown.hosted_absent("unknown", 10));
    }

    #[test]
    fn in_pane_agent_process_and_absent_probe_are_mutually_exclusive() {
        let start: jiff::Timestamp = "2026-06-30T11:18:03Z".parse().unwrap();
        let cases = vec![
            (
                "direct present",
                ProcFixture::new([ProcNode::new(10, 1_000, "codex", &[])]),
                false,
            ),
            (
                "wrapper present",
                ProcFixture::new([
                    ProcNode::new(10, 1_000, "zsh", &[20]),
                    ProcNode::new(20, 1_000, "codex", &[]),
                ]),
                false,
            ),
            (
                "clean absent",
                ProcFixture::new([ProcNode::new(10, 1_000, "zsh", &[])]),
                false,
            ),
            (
                "branch indeterminate",
                ProcFixture::new([
                    ProcNode::new(10, 1_000, "zsh", &[20, 30]),
                    ProcNode::new(20, 1_000, "codex", &[]),
                    ProcNode::new(30, 1_000, "make", &[]),
                ]),
                true,
            ),
            (
                "unreadable indeterminate",
                ProcFixture::new([
                    ProcNode::new(10, 1_000, "zsh", &[20]),
                    ProcNode::new(20, 1_000, "codex", &[]).with_unreadable_cmdline(),
                ]),
                true,
            ),
            (
                "depth indeterminate",
                ProcFixture::new([
                    ProcNode::new(10, 1_000, "zsh", &[11]),
                    ProcNode::new(11, 1_000, "zsh", &[12]),
                    ProcNode::new(12, 1_000, "zsh", &[13]),
                    ProcNode::new(13, 1_000, "zsh", &[14]),
                    ProcNode::new(14, 1_000, "zsh", &[15]),
                    ProcNode::new(15, 1_000, "zsh", &[16]),
                    ProcNode::new(16, 1_000, "zsh", &[17]),
                    ProcNode::new(17, 1_000, "zsh", &[18]),
                    ProcNode::new(18, 1_000, "zsh", &[19]),
                    ProcNode::new(19, 1_000, "zsh", &[]),
                ]),
                true,
            ),
        ];

        for (name, fixture, indeterminate) in cases {
            let present = in_pane_agent_process_for_root_with(
                "codex",
                10,
                &|pid| fixture.cmdline(pid),
                &|pid| fixture.children(pid),
                &|pid| fixture.nodes.contains_key(&pid).then_some(start),
                &|_| None,
            );
            let absent = fixture.hosted_absent("codex", 10);
            assert!(
                !(present.is_some() && absent),
                "{name}: present={present:?} absent={absent}"
            );
            if indeterminate {
                assert_eq!(present, None, "{name}");
                assert!(!absent, "{name}");
            }
        }
    }

    struct ProcNode {
        pid: u32,
        uid: u32,
        comm: Option<&'static str>,
        cmdline: Option<&'static str>,
        children: &'static [u32],
    }

    impl ProcNode {
        const fn new(pid: u32, uid: u32, cmdline: &'static str, children: &'static [u32]) -> Self {
            Self {
                pid,
                uid,
                comm: None,
                cmdline: Some(cmdline),
                children,
            }
        }

        const fn with_comm(mut self, comm: &'static str) -> Self {
            self.comm = Some(comm);
            self
        }

        const fn with_unreadable_cmdline(mut self) -> Self {
            self.cmdline = None;
            self
        }
    }

    struct ProcFixture {
        nodes: BTreeMap<u32, ProcNode>,
    }

    impl ProcFixture {
        fn new(nodes: impl IntoIterator<Item = ProcNode>) -> Self {
            Self {
                nodes: nodes.into_iter().map(|node| (node.pid, node)).collect(),
            }
        }

        fn cmdline(&self, pid: u32) -> Option<String> {
            self.nodes
                .get(&pid)
                .and_then(|node| node.cmdline.map(str::to_owned))
        }

        fn children(&self, pid: u32) -> Vec<u32> {
            self.nodes
                .get(&pid)
                .map(|node| node.children.to_vec())
                .unwrap_or_default()
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
                &|pid| {
                    self.nodes
                        .get(&pid)
                        .and_then(|node| node.comm.map(str::to_owned))
                },
                &|pid| self.nodes.get(&pid).map(|node| node.uid),
            )
        }
    }
}
