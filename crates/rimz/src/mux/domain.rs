//! Environment-resolved process domain identity.
//!
//! Every heuristic process kill passes this guard. A process in a foreign
//! domain, such as a `cargo xtask sandbox` or another runtime namespace, is not
//! ours to signal, and an unreadable process environment is spared.

use std::path::PathBuf;

use crate::ids::MuxName;

/// The state and multiplexer namespace inherited by a process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessDomain {
    state_home: PathBuf,
    runtime_home: PathBuf,
    zellij_socket_base: PathBuf,
    tmux_socket: PathBuf,
}

impl ProcessDomain {
    /// Resolve the invoker's process domain from its current environment.
    pub fn current() -> Self {
        Self::from_env(
            |key| std::env::var(key).ok().filter(|value| !value.is_empty()),
            crate::proc::own_uid().unwrap_or_default(),
        )
    }

    /// Resolve `pid`'s inherited process domain. An unreadable environment is
    /// unknown and returns `None`, allowing signal callers to spare it.
    pub fn of_process(pid: u32) -> Option<Self> {
        let environment = crate::proc::environ(pid)?;
        Some(Self::from_env(
            |key| {
                environment
                    .iter()
                    .find_map(|(candidate, value)| (candidate == key).then(|| value.clone()))
                    .filter(|value| !value.is_empty())
            },
            crate::proc::own_uid().unwrap_or_default(),
        ))
    }

    /// Whether two processes share RimZ's persistent and runtime state world.
    pub fn same_world(&self, other: &Self) -> bool {
        self.state_home == other.state_home && self.runtime_home == other.runtime_home
    }

    /// Whether two processes share the state world and selected mux endpoint.
    pub fn same_mux_endpoint(&self, other: &Self, mux: MuxName) -> bool {
        self.same_world(other)
            && match mux {
                MuxName::Zellij => self.zellij_socket_base == other.zellij_socket_base,
                MuxName::Tmux => self.tmux_socket == other.tmux_socket,
            }
    }

    fn from_env(get: impl Fn(&str) -> Option<String>, uid: u32) -> Self {
        let path = |key| get(key).map(PathBuf::from);
        let tmpdir = path("TMPDIR").unwrap_or_else(|| PathBuf::from("/tmp"));
        let xdg_runtime = path("XDG_RUNTIME_DIR");
        let state_home = crate::store::paths::state_home_from(
            path("XDG_STATE_HOME").as_deref(),
            path("HOME").as_deref(),
            &tmpdir,
        );
        let runtime_home = crate::store::paths::runtime_home_from(xdg_runtime.as_deref(), uid);
        let zellij_socket_base = crate::mux::zellij::socket::socket_base_from(
            path("ZELLIJ_SOCKET_DIR").as_deref(),
            xdg_runtime.as_deref(),
            &tmpdir,
            &uid.to_string(),
        );
        let tmux_tmpdir = path("TMUX_TMPDIR").unwrap_or_else(|| PathBuf::from("/tmp"));
        let tmux_socket = get("TMUX")
            .as_deref()
            .and_then(crate::mux::tmux::socket_path_from_tmux_var)
            .unwrap_or_else(|| {
                crate::mux::tmux::default_server_socket_path_from(&tmux_tmpdir, uid)
            });
        Self {
            state_home,
            runtime_home,
            zellij_socket_base,
            tmux_socket,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    const UID: u32 = 1_000;

    fn domain(values: &[(&str, &str)]) -> ProcessDomain {
        let environment = values.iter().copied().collect::<HashMap<_, _>>();
        ProcessDomain::from_env(
            |key| environment.get(key).map(|value| (*value).to_owned()),
            UID,
        )
    }

    fn host() -> ProcessDomain {
        domain(&[
            ("HOME", "/home/user"),
            ("XDG_STATE_HOME", "/home/user/.local/state"),
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
            ("TMUX_TMPDIR", "/tmp"),
        ])
    }

    #[test]
    fn sandbox_is_a_foreign_world() {
        let sandbox = domain(&[
            ("HOME", "/tmp/rimz-sandbox-X/home"),
            ("XDG_STATE_HOME", "/tmp/rimz-sandbox-X/state"),
            ("XDG_RUNTIME_DIR", "/tmp/rimz-sandbox-X/runtime"),
            ("TMUX_TMPDIR", "/tmp/rimz-sandbox-X/tmux"),
            ("TMPDIR", "/tmp/rimz-sandbox-X/tmp"),
        ]);

        assert!(!host().same_world(&sandbox));
    }

    #[test]
    fn missing_runtime_root_is_a_foreign_world() {
        let without_runtime = domain(&[
            ("HOME", "/home/user"),
            ("XDG_STATE_HOME", "/home/user/.local/state"),
            ("TMUX_TMPDIR", "/tmp"),
        ]);

        assert!(!host().same_world(&without_runtime));
    }

    #[test]
    fn mux_endpoint_comparison_uses_only_the_selected_mux() {
        let alternate_zellij = domain(&[
            ("HOME", "/home/user"),
            ("XDG_STATE_HOME", "/home/user/.local/state"),
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
            ("ZELLIJ_SOCKET_DIR", "/tmp/alternate-zellij"),
            ("TMUX_TMPDIR", "/tmp"),
        ]);
        let host = host();

        assert!(host.same_world(&alternate_zellij));
        assert!(!host.same_mux_endpoint(&alternate_zellij, MuxName::Zellij));
        assert!(host.same_mux_endpoint(&alternate_zellij, MuxName::Tmux));
    }

    #[test]
    fn explicit_default_tmux_socket_matches_the_fallback() {
        let explicit = domain(&[("TMUX", "/tmp/tmux-1000/default,123,0")]);
        let fallback = domain(&[]);

        assert!(explicit.same_mux_endpoint(&fallback, MuxName::Tmux));
    }

    #[test]
    fn all_defaults_are_equal() {
        let left = domain(&[]);
        let right = domain(&[]);

        assert!(left.same_world(&right));
        assert!(left.same_mux_endpoint(&right, MuxName::Zellij));
        assert!(left.same_mux_endpoint(&right, MuxName::Tmux));
    }

    #[test]
    fn default_state_home_uses_the_tmpdir_fallback() {
        let default = domain(&[]);
        assert_eq!(default.state_home, std::path::Path::new("/tmp/rimz-state"));
    }
}
