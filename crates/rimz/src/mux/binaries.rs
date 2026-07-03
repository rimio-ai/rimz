//! PATH and live-server binary probes for `rimz doctor`.
//!
//! The scan stays best-effort: PATH tells which client a new command runs,
//! `/proc` tells which executable live servers use on Linux, and every miss
//! degrades to an absent row rather than blocking doctor.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::ids::MuxName;
use crate::proc::ProcInfo;

use super::CommandSpec;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BinaryInstall {
    pub path: PathBuf,
    pub canonical: PathBuf,
    pub version: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BinaryScan {
    pub installs: Vec<BinaryInstall>,
    pub servers: Vec<ServerBinary>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerBinary {
    pub pid: u32,
    pub exe: PathBuf,
    pub deleted: bool,
    pub matches_active: bool,
}

pub fn scan(mux: MuxName) -> BinaryScan {
    let (program, version_args) = program_version_args(mux);
    let installs = dedupe_by_canonical(path_candidates(program))
        .into_iter()
        .map(|(path, canonical)| BinaryInstall {
            version: version_for(&path, version_args),
            path,
            canonical,
        })
        .collect::<Vec<_>>();
    let active = installs.first().map(|install| install.canonical.as_path());
    let servers = servers_from_processes(
        mux,
        &crate::proc::list_processes(),
        crate::proc::own_uid(),
        active,
        crate::proc::exe_path,
    );
    BinaryScan { installs, servers }
}

fn program_version_args(mux: MuxName) -> (&'static str, &'static [&'static str]) {
    match mux {
        MuxName::Zellij => ("zellij", &["--version"]),
        MuxName::Tmux => ("tmux", &["-V"]),
    }
}

fn path_candidates(program: &str) -> Vec<(PathBuf, PathBuf)> {
    which::which_all(program)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|path| path.canonicalize().ok().map(|canonical| (path, canonical)))
        .collect()
}

fn dedupe_by_canonical(candidates: Vec<(PathBuf, PathBuf)>) -> Vec<(PathBuf, PathBuf)> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (path, canonical) in candidates {
        if seen.insert(canonical.clone()) {
            out.push((path, canonical));
        }
    }
    out
}

fn version_for(path: &Path, args: &[&str]) -> Option<String> {
    let output = CommandSpec::new(path.to_string_lossy().into_owned())
        .args(args.iter().copied())
        .run()
        .ok()?;
    let version = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!version.is_empty()).then_some(version)
}

fn servers_from_processes<F>(
    mux: MuxName,
    procs: &[ProcInfo],
    own_uid: Option<u32>,
    active: Option<&Path>,
    mut exe_path: F,
) -> Vec<ServerBinary>
where
    F: FnMut(u32) -> Option<(PathBuf, bool)>,
{
    let Some(own_uid) = own_uid else {
        return Vec::new();
    };
    procs
        .iter()
        .filter(|process| process.real_uid == own_uid)
        .filter(|process| is_server_cmdline(mux, &process.cmdline))
        .filter_map(|process| {
            let (exe, deleted) = exe_path(process.pid)?;
            let matches_active = !deleted
                && active.is_some_and(|active| canonical_for_compare(&exe).as_path() == active);
            Some(ServerBinary {
                pid: process.pid,
                exe,
                deleted,
                matches_active,
            })
        })
        .collect()
}

fn canonical_for_compare(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn is_server_cmdline(mux: MuxName, cmdline: &str) -> bool {
    match mux {
        MuxName::Zellij => is_zellij_server_cmdline(cmdline),
        MuxName::Tmux => is_tmux_server_cmdline(cmdline),
    }
}

fn is_zellij_server_cmdline(cmdline: &str) -> bool {
    let mut args = cmdline.split_whitespace();
    let Some(program) = args.next() else {
        return false;
    };
    path_ends_with_program(program, "zellij") && args.any(|arg| arg == "--server")
}

pub(crate) fn is_tmux_server_cmdline(cmdline: &str) -> bool {
    cmdline.starts_with("tmux: server")
}

fn path_ends_with_program(token: &str, program: &str) -> bool {
    Path::new(token).file_name().and_then(|name| name.to_str()) == Some(program)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, uid: u32, cmdline: &str) -> ProcInfo {
        ProcInfo {
            pid,
            ppid: 1,
            real_uid: uid,
            cmdline: cmdline.to_owned(),
        }
    }

    #[test]
    fn dedupe_collapses_canonical_duplicates_and_preserves_order() {
        let installs = dedupe_by_canonical(vec![
            ("/path/first/zellij".into(), "/real/zellij".into()),
            ("/path/second/zellij".into(), "/real/zellij".into()),
            ("/path/third/zellij".into(), "/other/zellij".into()),
        ]);
        assert_eq!(
            installs,
            vec![
                (
                    PathBuf::from("/path/first/zellij"),
                    PathBuf::from("/real/zellij")
                ),
                (
                    PathBuf::from("/path/third/zellij"),
                    PathBuf::from("/other/zellij")
                ),
            ]
        );
    }

    #[test]
    fn server_matchers_accept_real_shapes_and_reject_lookalikes() {
        assert!(is_zellij_server_cmdline(
            "zellij --server /run/user/1000/zellij"
        ));
        assert!(is_zellij_server_cmdline(
            "/home/dev/.cargo/bin/zellij --server /run/user/1000/zellij",
        ));
        assert!(!is_zellij_server_cmdline("zellij action list-panes"));
        assert!(!is_zellij_server_cmdline(
            "rimz sidebar serve --mux zellij --server /tmp/nope",
        ));

        assert!(is_tmux_server_cmdline(
            "tmux: server (/tmp/tmux-1000/default)"
        ));
        assert!(!is_tmux_server_cmdline("tmux new-session -s rimz"));
        assert!(!is_tmux_server_cmdline("rimz sidebar serve --mux tmux"));
    }

    #[test]
    fn server_scan_flags_mismatches_against_active_binary() {
        let procs = vec![
            proc(10, 1000, "zellij --server /run/user/1000/zellij"),
            proc(11, 1000, "/old/bin/zellij --server /run/user/1000/zellij"),
            proc(12, 1001, "/old/bin/zellij --server /run/user/1000/zellij"),
        ];
        let servers = servers_from_processes(
            MuxName::Zellij,
            &procs,
            Some(1000),
            Some(Path::new("/active/bin/zellij")),
            |pid| match pid {
                10 => Some(("/active/bin/zellij".into(), false)),
                11 => Some(("/old/bin/zellij".into(), false)),
                _ => None,
            },
        );
        assert_eq!(
            servers,
            vec![
                ServerBinary {
                    pid: 10,
                    exe: "/active/bin/zellij".into(),
                    deleted: false,
                    matches_active: true,
                },
                ServerBinary {
                    pid: 11,
                    exe: "/old/bin/zellij".into(),
                    deleted: false,
                    matches_active: false,
                },
            ]
        );
    }

    #[test]
    fn deleted_server_exe_never_matches_active_binary() {
        let procs = vec![proc(10, 1000, "tmux: server (/tmp/tmux-1000/default)")];
        let servers = servers_from_processes(
            MuxName::Tmux,
            &procs,
            Some(1000),
            Some(Path::new("/active/bin/tmux")),
            |_| Some(("/active/bin/tmux".into(), true)),
        );
        assert_eq!(servers.len(), 1);
        assert!(!servers[0].matches_active);
        assert!(servers[0].deleted);
    }
}
