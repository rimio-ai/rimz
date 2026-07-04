//! Backend selection precedence:
//!
//! 1. explicit `--mux <name>` flag
//! 2. active mux environment (`ZELLIJ` / `TMUX`)
//! 3. `[mux] default` config — errors if it names an uninstalled backend
//! 4. installed binary (tmux preferred when both are present)

use super::MuxErr;
use crate::ids::MuxName;

pub type Result<T> = std::result::Result<T, MuxErr>;

pub(crate) fn select_backend(
    explicit: Option<MuxName>,
    env_mux: Option<MuxName>,
    configured_default: Option<MuxName>,
    zellij_installed: bool,
    tmux_installed: bool,
) -> Result<MuxName> {
    if let Some(mux) = explicit {
        return Ok(mux);
    }
    if let Some(mux) = env_mux {
        return Ok(mux);
    }
    if let Some(mux) = configured_default {
        let installed = match mux {
            MuxName::Zellij => zellij_installed,
            MuxName::Tmux => tmux_installed,
        };
        return installed
            .then_some(mux)
            .ok_or(MuxErr::ConfiguredMuxNotInstalled { mux });
    }
    match (zellij_installed, tmux_installed) {
        (_, true) => Ok(MuxName::Tmux),
        (true, false) => Ok(MuxName::Zellij),
        (false, false) => Err(MuxErr::NoMuxFound),
    }
}

fn detect_env_mux() -> Option<MuxName> {
    if std::env::var_os("ZELLIJ").is_some() || std::env::var_os("ZELLIJ_PANE_ID").is_some() {
        Some(MuxName::Zellij)
    } else if std::env::var_os("TMUX").is_some() || std::env::var_os("TMUX_PANE").is_some() {
        Some(MuxName::Tmux)
    } else {
        None
    }
}

pub fn auto_detect_backend(explicit: Option<MuxName>) -> Result<MuxName> {
    let env_mux = detect_env_mux();
    if explicit.is_some() || env_mux.is_some() {
        return select_backend(explicit, env_mux, None, false, false);
    }
    select_backend(
        None,
        None,
        crate::config::MachineConfig::load_lenient().mux.default,
        which::which("zellij").is_ok(),
        which::which("tmux").is_ok(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn select(
        explicit: Option<MuxName>,
        env_mux: Option<MuxName>,
        configured_default: Option<MuxName>,
        zellij_installed: bool,
        tmux_installed: bool,
    ) -> Result<MuxName> {
        select_backend(
            explicit,
            env_mux,
            configured_default,
            zellij_installed,
            tmux_installed,
        )
    }

    #[test]
    fn explicit_flag_wins_over_everything() {
        assert_eq!(
            select(
                Some(MuxName::Zellij),
                Some(MuxName::Tmux),
                Some(MuxName::Tmux),
                false,
                false,
            )
            .expect("select explicit"),
            MuxName::Zellij
        );
    }

    #[test]
    fn active_env_wins_over_config_and_binaries() {
        assert_eq!(
            select(
                None,
                Some(MuxName::Zellij),
                Some(MuxName::Tmux),
                false,
                true,
            )
            .expect("select env"),
            MuxName::Zellij
        );
    }

    #[test]
    fn configured_default_is_used_when_installed() {
        assert_eq!(
            select(None, None, Some(MuxName::Zellij), true, true).expect("select default"),
            MuxName::Zellij
        );
        assert_eq!(
            select(None, None, Some(MuxName::Tmux), true, true).expect("select default"),
            MuxName::Tmux
        );
    }

    #[test]
    fn configured_default_errors_when_not_installed() {
        assert!(matches!(
            select(None, None, Some(MuxName::Zellij), false, true),
            Err(MuxErr::ConfiguredMuxNotInstalled {
                mux: MuxName::Zellij
            })
        ));
        assert!(matches!(
            select(None, None, Some(MuxName::Tmux), true, false),
            Err(MuxErr::ConfiguredMuxNotInstalled { mux: MuxName::Tmux })
        ));
    }

    #[test]
    fn unset_default_prefers_tmux_when_both_are_installed() {
        assert_eq!(
            select(None, None, None, true, true).expect("select installed"),
            MuxName::Tmux
        );
    }

    #[test]
    fn unset_default_uses_only_installed_backend() {
        assert_eq!(
            select(None, None, None, true, false).expect("select zellij"),
            MuxName::Zellij
        );
        assert_eq!(
            select(None, None, None, false, true).expect("select tmux"),
            MuxName::Tmux
        );
    }

    #[test]
    fn unset_default_errors_when_no_backend_is_installed() {
        assert!(matches!(
            select(None, None, None, false, false),
            Err(MuxErr::NoMuxFound)
        ));
    }
}
