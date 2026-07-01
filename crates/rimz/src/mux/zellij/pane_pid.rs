use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Resolves Zellij pane references to tmux-style pane root pids.
///
/// Zellij reports no pane pid. The resolver matches a pane's foreground
/// command against the session server's process forest, then walks up to the
/// direct child of the session's `zellij --server <socket>` process.
pub struct ZellijPaneResolver<'a> {
    procs: &'a [crate::proc::ProcInfo],
    server_pid: u32,
    forest: HashSet<u32>,
    parent_of: HashMap<u32, u32>,
    claimed: HashSet<u32>,
}

impl<'a> ZellijPaneResolver<'a> {
    /// `None` when the session's `zellij --server <session>` process is not in
    /// `procs`.
    pub fn new(
        procs: &'a [crate::proc::ProcInfo],
        children: &HashMap<u32, Vec<u32>>,
        session_name: &str,
        own_uid: Option<u32>,
    ) -> Option<Self> {
        let server_pid = zellij_server_pid(procs, session_name, own_uid)?;
        let forest = descendants(children, server_pid);
        let parent_of: HashMap<u32, u32> = procs.iter().map(|p| (p.pid, p.ppid)).collect();
        Some(Self {
            procs,
            server_pid,
            forest,
            parent_of,
            claimed: HashSet::new(),
        })
    }

    /// Pre-mark a root pid taken by another pane.
    pub fn claim(&mut self, root: u32) {
        self.claimed.insert(root);
    }

    /// Resolves a pane's root pid from its foreground cmdline and reported cwd.
    ///
    /// The winner is marked claimed. `None` means no match or ambiguity.
    pub fn resolve(
        &mut self,
        command: &str,
        cwd: Option<&str>,
        proc_cwd: &dyn Fn(u32) -> Option<PathBuf>,
    ) -> Option<u32> {
        let candidates: Vec<(u32, u32)> = self
            .procs
            .iter()
            .filter(|p| self.forest.contains(&p.pid) && p.cmdline == command)
            .filter_map(|p| {
                walk_to_server_child(&self.parent_of, self.server_pid, p.pid)
                    .filter(|root| !self.claimed.contains(root))
                    .map(|root| (p.pid, root))
            })
            .collect();
        let matched = resolve_candidate_root(&candidates, cwd, proc_cwd)?;
        self.claimed.insert(matched);
        Some(matched)
    }
}

fn resolve_candidate_root(
    candidates: &[(u32, u32)],
    cwd: Option<&str>,
    proc_cwd: &dyn Fn(u32) -> Option<PathBuf>,
) -> Option<u32> {
    let roots = unique_candidate_roots(candidates);
    match roots.as_slice() {
        [root] => Some(*root),
        [] => None,
        _ => {
            let cwd = cwd?;
            let narrowed: Vec<(u32, u32)> = candidates
                .iter()
                .copied()
                .filter(|&(pid, _)| proc_cwd(pid).as_deref() == Some(Path::new(cwd)))
                .collect();
            let narrowed_roots = unique_candidate_roots(&narrowed);
            match narrowed_roots.as_slice() {
                [root] => Some(*root),
                _ => None,
            }
        }
    }
}

fn unique_candidate_roots(candidates: &[(u32, u32)]) -> Vec<u32> {
    let mut roots = Vec::new();
    for (_, root) in candidates {
        if !roots.iter().any(|known| known == root) {
            roots.push(*root);
        }
    }
    roots
}

/// The pid of the session's Zellij server: the same-uid process whose cmdline
/// is `zellij --server <socket>` with the socket's file name equal to the
/// session name. The uid gate keeps a same-named session of another user from
/// being walked.
fn zellij_server_pid(
    procs: &[crate::proc::ProcInfo],
    session_name: &str,
    own_uid: Option<u32>,
) -> Option<u32> {
    let own_uid = own_uid?;
    procs
        .iter()
        .find(|p| p.real_uid == own_uid && cmdline_is_session_server(&p.cmdline, session_name))
        .map(|p| p.pid)
}

/// Whether a cmdline runs the Zellij server for `session_name` — exactly
/// `<path>/zellij --server <socket>` with `basename(socket) == session_name`.
fn cmdline_is_session_server(cmdline: &str, session_name: &str) -> bool {
    let mut tokens = cmdline.split_whitespace();
    let file_name = |token: Option<&str>, name: &str| {
        token
            .map(Path::new)
            .and_then(Path::file_name)
            .is_some_and(|file| file == name)
    };
    file_name(tokens.next(), "zellij")
        && tokens.next() == Some("--server")
        && file_name(tokens.next(), session_name)
}

/// Every descendant of `root` in the ppid→children map — the session server's
/// process forest, one tree per pane.
fn descendants(children: &HashMap<u32, Vec<u32>>, root: u32) -> HashSet<u32> {
    let mut out = HashSet::new();
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        for &child in children.get(&pid).map(Vec::as_slice).unwrap_or_default() {
            if out.insert(child) {
                stack.push(child);
            }
        }
    }
    out
}

/// Walk `pid` up its parent chain to the direct child of `server_pid` — the
/// pane root. The `None` arm covers a chain that leaves the table mid-walk,
/// e.g. a process that exited between reads.
fn walk_to_server_child(
    parent_of: &HashMap<u32, u32>,
    server_pid: u32,
    mut pid: u32,
) -> Option<u32> {
    loop {
        match parent_of.get(&pid) {
            Some(&ppid) if ppid == server_pid => return Some(pid),
            Some(&ppid) => pid = ppid,
            None => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "rimz-query-engine";

    fn proc_info(pid: u32, ppid: u32, cmdline: &str) -> crate::proc::ProcInfo {
        crate::proc::ProcInfo {
            pid,
            ppid,
            real_uid: 1000,
            cmdline: cmdline.to_owned(),
        }
    }

    fn server(pid: u32, session: &str) -> crate::proc::ProcInfo {
        proc_info(
            pid,
            1,
            &format!("/usr/bin/zellij --server /run/user/1000/zellij/contract_version_1/{session}"),
        )
    }

    fn children_of(procs: &[crate::proc::ProcInfo]) -> HashMap<u32, Vec<u32>> {
        let mut children = HashMap::new();
        for p in procs {
            children.entry(p.ppid).or_insert_with(Vec::new).push(p.pid);
        }
        children
    }

    #[test]
    fn resolve_candidate_root_handles_empty_unique_and_cwd_narrowing() {
        assert_eq!(
            resolve_candidate_root(&[], Some("/repo"), &|_| {
                panic!("empty candidates do not need cwd")
            }),
            None
        );

        assert_eq!(
            resolve_candidate_root(&[(300, 200), (301, 200)], None, &|_| {
                panic!("one root does not need cwd")
            }),
            Some(200)
        );

        let candidates = [(300, 200), (310, 210)];
        assert_eq!(
            resolve_candidate_root(&candidates, Some("/repo/feature"), &|pid| match pid {
                300 => Some(PathBuf::from("/repo/main")),
                310 => Some(PathBuf::from("/repo/feature")),
                _ => None,
            }),
            Some(210)
        );
    }

    #[test]
    fn resolver_claims_each_winner_once() {
        let procs = vec![
            server(100, SESSION),
            proc_info(200, 100, "zsh"),
            proc_info(300, 200, "codex"),
            proc_info(210, 100, "zsh"),
            proc_info(310, 210, "codex"),
        ];
        let cwds = HashMap::from([
            (300, PathBuf::from("/repo/main")),
            (310, PathBuf::from("/repo/feature")),
        ]);
        let mut resolver =
            ZellijPaneResolver::new(&procs, &children_of(&procs), SESSION, Some(1000))
                .expect("server exists");

        assert_eq!(
            resolver.resolve("codex", Some("/repo/main"), &|pid| cwds.get(&pid).cloned()),
            Some(200)
        );
        assert_eq!(
            resolver.resolve("codex", Some("/repo/feature"), &|pid| cwds
                .get(&pid)
                .cloned()),
            Some(210)
        );
        assert_eq!(
            resolver.resolve("codex", Some("/repo/main"), &|pid| cwds.get(&pid).cloned()),
            None
        );
    }

    #[test]
    fn new_returns_none_without_matching_session_server() {
        let mut other_uid = server(100, SESSION);
        other_uid.real_uid = 1001;
        let procs = vec![
            server(110, "rimz-other"),
            other_uid,
            proc_info(200, 100, "zsh"),
            proc_info(300, 200, "htop"),
        ];

        assert!(
            ZellijPaneResolver::new(&procs, &children_of(&procs), SESSION, Some(1000)).is_none()
        );
        let no_uid_procs = vec![server(100, SESSION), proc_info(300, 100, "htop")];
        assert!(
            ZellijPaneResolver::new(&no_uid_procs, &children_of(&no_uid_procs), SESSION, None,)
                .is_none()
        );
    }
}
