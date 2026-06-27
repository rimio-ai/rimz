//! Probes terminal and tmux capabilities for kitty graphics support.

use std::io;
use std::time::Duration;

use crate::config::PetsGlyphMode;
use crate::ids::MuxName;
use crate::mux::CommandSpec;

const MIN_PIXEL_TMUX_VERSION: (u32, u32, u32) = (3, 6, 0);
const COMMAND_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PetRenderCaps {
    pub pixel: bool,
}

pub(crate) fn detect(mux: MuxName, mode: PetsGlyphMode, session_name: &str) -> PetRenderCaps {
    detect_with(mux, mode, session_name, &LiveProbe)
}

pub fn detect_env(mode: PetsGlyphMode) -> (PetRenderCaps, bool) {
    detect_env_with(mode, &LiveProbe)
}

trait Probe {
    fn tmux_version(&self) -> io::Result<String>;
    fn tmux_allow_passthrough(&self) -> io::Result<String>;
    fn tmux_client_termnames(&self, session_name: &str) -> io::Result<Vec<String>>;
    fn tmux_session_name(&self) -> io::Result<String>;
    fn env_var(&self, key: &str) -> Option<String>;
}

fn detect_with(
    probed_mux: MuxName,
    mode: PetsGlyphMode,
    session_name: &str,
    probe: &impl Probe,
) -> PetRenderCaps {
    match probed_mux {
        MuxName::Tmux => detect_tmux(mode, session_name, probe),
        MuxName::Zellij => detect_zellij(probe),
    }
}

fn detect_env_with(mode: PetsGlyphMode, probe: &impl Probe) -> (PetRenderCaps, bool) {
    if env_present(probe, "TMUX") {
        let caps = probe
            .tmux_session_name()
            .map(|session_name| detect_tmux(mode, &session_name, probe))
            .unwrap_or_default();
        return (caps, true);
    }
    if env_present(probe, "ZELLIJ") {
        return (detect_zellij(probe), false);
    }
    (detect_standalone(mode, probe), false)
}

fn detect_tmux(mode: PetsGlyphMode, session_name: &str, probe: &impl Probe) -> PetRenderCaps {
    let kitty_term = probe
        .tmux_client_termnames(session_name)
        .is_ok_and(|termnames| termnames_allowed(&termnames));
    let version_ok = probe
        .tmux_version()
        .ok()
        .and_then(|version| crate::mux::tmux::parse_version(&version))
        .is_some_and(|version| version >= MIN_PIXEL_TMUX_VERSION);
    let passthrough_ok = probe
        .tmux_allow_passthrough()
        .is_ok_and(|allow| matches!(allow.trim(), "on" | "all"));
    PetRenderCaps {
        pixel: version_ok && passthrough_ok && (kitty_term || mode == PetsGlyphMode::Pixel),
    }
}

fn detect_zellij(_probe: &impl Probe) -> PetRenderCaps {
    PetRenderCaps { pixel: false }
}

fn detect_standalone(mode: PetsGlyphMode, probe: &impl Probe) -> PetRenderCaps {
    let kitty_term = standalone_term_allowed(probe);
    PetRenderCaps {
        pixel: kitty_term || mode == PetsGlyphMode::Pixel,
    }
}

fn standalone_term_allowed(probe: &impl Probe) -> bool {
    probe
        .env_var("TERM")
        .as_deref()
        .is_some_and(termname_allowed)
}

fn env_present(probe: &impl Probe, key: &str) -> bool {
    probe.env_var(key).is_some_and(|value| !value.is_empty())
}

struct LiveProbe;

impl Probe for LiveProbe {
    fn tmux_version(&self) -> io::Result<String> {
        run_tmux(["-V"])
    }

    fn tmux_allow_passthrough(&self) -> io::Result<String> {
        run_tmux(["show-options", "-gv", "allow-passthrough"])
    }

    fn tmux_client_termnames(&self, session_name: &str) -> io::Result<Vec<String>> {
        run_tmux([
            "list-clients",
            "-t",
            session_name,
            "-F",
            "#{client_control_mode} #{client_termname}",
        ])
        .map(|out| rendering_termnames(&out))
    }

    fn tmux_session_name(&self) -> io::Result<String> {
        run_tmux(["display-message", "-p", "#{session_name}"])
    }

    fn env_var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

fn run_tmux<const N: usize>(args: [&str; N]) -> io::Result<String> {
    let output = CommandSpec::new("tmux")
        .args(args)
        .run_with_timeout(COMMAND_TIMEOUT)
        .map_err(io::Error::other)?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn termnames_allowed(termnames: &[String]) -> bool {
    !termnames.is_empty() && termnames.iter().all(|termname| termname_allowed(termname))
}

fn rendering_termnames(list_clients_output: &str) -> Vec<String> {
    list_clients_output
        .lines()
        .filter_map(|line| {
            let (control_mode, termname) = line.split_once(' ')?;
            (control_mode.trim() != "1").then(|| termname.trim().to_owned())
        })
        .collect()
}

fn termname_allowed(termname: &str) -> bool {
    matches!(
        termname.trim().to_ascii_lowercase().as_str(),
        "xterm-ghostty" | "ghostty" | "xterm-kitty" | "kitty"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    const TEST_SESSION: &str = "rimz-test";

    struct FakeProbe {
        version: Option<String>,
        allow_passthrough: Option<String>,
        expected_session: String,
        termnames: Option<Vec<String>>,
        session_name: Option<String>,
        env: BTreeMap<String, String>,
    }

    impl FakeProbe {
        fn ok() -> Self {
            Self {
                version: Some("tmux 3.6b".to_owned()),
                allow_passthrough: Some("on".to_owned()),
                expected_session: TEST_SESSION.to_owned(),
                termnames: Some(vec!["xterm-ghostty".to_owned()]),
                session_name: Some(TEST_SESSION.to_owned()),
                env: BTreeMap::new(),
            }
        }

        fn with_env(mut self, key: &str, value: &str) -> Self {
            self.env.insert(key.to_owned(), value.to_owned());
            self
        }
    }

    impl Probe for FakeProbe {
        fn tmux_version(&self) -> io::Result<String> {
            self.version
                .clone()
                .ok_or_else(|| io::Error::other("tmux version unavailable"))
        }

        fn tmux_allow_passthrough(&self) -> io::Result<String> {
            self.allow_passthrough
                .clone()
                .ok_or_else(|| io::Error::other("tmux passthrough unavailable"))
        }

        fn tmux_client_termnames(&self, session_name: &str) -> io::Result<Vec<String>> {
            assert_eq!(session_name, self.expected_session);
            self.termnames
                .clone()
                .ok_or_else(|| io::Error::other("tmux termnames unavailable"))
        }

        fn tmux_session_name(&self) -> io::Result<String> {
            self.session_name
                .clone()
                .ok_or_else(|| io::Error::other("tmux session unavailable"))
        }

        fn env_var(&self, key: &str) -> Option<String> {
            self.env.get(key).cloned()
        }
    }

    #[test]
    fn tmux_pixel_gate_requires_version_passthrough_and_allowed_termname() {
        assert_eq!(
            detect_with(
                MuxName::Tmux,
                PetsGlyphMode::Auto,
                TEST_SESSION,
                &FakeProbe::ok()
            ),
            PetRenderCaps { pixel: true }
        );

        assert_eq!(
            detect_with(
                MuxName::Zellij,
                PetsGlyphMode::Auto,
                TEST_SESSION,
                &FakeProbe::ok().with_env("TERM", "xterm-ghostty")
            ),
            PetRenderCaps { pixel: false }
        );

        let old = FakeProbe {
            version: Some("tmux 3.5a".to_owned()),
            ..FakeProbe::ok()
        };
        assert_eq!(
            detect_with(MuxName::Tmux, PetsGlyphMode::Auto, TEST_SESSION, &old),
            PetRenderCaps { pixel: false }
        );

        let off = FakeProbe {
            allow_passthrough: Some("off".to_owned()),
            ..FakeProbe::ok()
        };
        assert_eq!(
            detect_with(MuxName::Tmux, PetsGlyphMode::Auto, TEST_SESSION, &off),
            PetRenderCaps { pixel: false }
        );

        let unsupported_term = FakeProbe {
            termnames: Some(vec!["screen-256color".to_owned()]),
            ..FakeProbe::ok()
        };
        assert_eq!(
            detect_with(
                MuxName::Tmux,
                PetsGlyphMode::Auto,
                TEST_SESSION,
                &unsupported_term
            ),
            PetRenderCaps::default()
        );
    }

    #[test]
    fn tmux_gate_accepts_all_passthrough_value() {
        let all = FakeProbe {
            allow_passthrough: Some("all\n".to_owned()),
            ..FakeProbe::ok()
        };

        assert!(detect_with(MuxName::Tmux, PetsGlyphMode::Auto, TEST_SESSION, &all).pixel);
    }

    #[test]
    fn termname_gate_accepts_known_kitty_terminals_case_insensitively() {
        for termname in [
            "xterm-ghostty",
            "ghostty",
            "xterm-kitty",
            "kitty",
            "XTERM-KITTY",
        ] {
            let probe = FakeProbe {
                termnames: Some(vec![termname.to_owned()]),
                ..FakeProbe::ok()
            };

            assert!(
                detect_with(MuxName::Tmux, PetsGlyphMode::Auto, TEST_SESSION, &probe).pixel,
                "{termname} should enable pixels"
            );
        }
    }

    #[test]
    fn termname_gate_requires_every_attached_client_to_match() {
        let allowed = FakeProbe {
            termnames: Some(vec!["xterm-ghostty".to_owned(), "kitty".to_owned()]),
            ..FakeProbe::ok()
        };
        assert!(detect_with(MuxName::Tmux, PetsGlyphMode::Auto, TEST_SESSION, &allowed).pixel);

        let mixed = FakeProbe {
            termnames: Some(vec![
                "xterm-ghostty".to_owned(),
                "screen-256color".to_owned(),
            ]),
            ..FakeProbe::ok()
        };
        assert!(!detect_with(MuxName::Tmux, PetsGlyphMode::Auto, TEST_SESSION, &mixed).pixel);
    }

    #[test]
    fn tmux_control_mode_clients_are_ignored_for_termname_gate() {
        assert_eq!(
            rendering_termnames("0 xterm-ghostty\n1 tmux-256color"),
            vec!["xterm-ghostty".to_owned()]
        );

        let probe = FakeProbe {
            termnames: Some(rendering_termnames("0 xterm-ghostty\n1 tmux-256color")),
            ..FakeProbe::ok()
        };
        assert!(detect_with(MuxName::Tmux, PetsGlyphMode::Auto, TEST_SESSION, &probe).pixel);
    }

    #[test]
    fn termname_gate_scopes_clients_to_sidebar_session() {
        let probe = FakeProbe {
            expected_session: "room-a".to_owned(),
            termnames: Some(vec!["xterm-ghostty".to_owned()]),
            ..FakeProbe::ok()
        };

        assert!(detect_with(MuxName::Tmux, PetsGlyphMode::Auto, "room-a", &probe).pixel);
    }

    #[test]
    fn termname_gate_rejects_empty_unattached_clients() {
        let unattached = FakeProbe {
            termnames: Some(Vec::new()),
            ..FakeProbe::ok()
        };

        assert_eq!(
            detect_with(
                MuxName::Tmux,
                PetsGlyphMode::Auto,
                TEST_SESSION,
                &unattached
            ),
            PetRenderCaps::default()
        );
    }

    #[test]
    fn explicit_pixel_mode_skips_termname_only_for_pixel_gate() {
        let unsupported_term = FakeProbe {
            termnames: Some(vec!["wezterm".to_owned()]),
            ..FakeProbe::ok()
        };
        assert_eq!(
            detect_with(
                MuxName::Tmux,
                PetsGlyphMode::Pixel,
                TEST_SESSION,
                &unsupported_term
            ),
            PetRenderCaps { pixel: true }
        );

        let old = FakeProbe {
            version: Some("tmux 3.5a".to_owned()),
            ..unsupported_term
        };
        assert_eq!(
            detect_with(MuxName::Tmux, PetsGlyphMode::Pixel, TEST_SESSION, &old),
            PetRenderCaps::default()
        );

        let off = FakeProbe {
            allow_passthrough: Some("off".to_owned()),
            ..FakeProbe::ok()
        };
        assert_eq!(
            detect_with(MuxName::Tmux, PetsGlyphMode::Pixel, TEST_SESSION, &off),
            PetRenderCaps { pixel: false }
        );
    }

    #[test]
    fn standalone_env_detects_native_kitty_and_wrap_mode() {
        let plain = FakeProbe::ok().with_env("TERM", "xterm-kitty");
        assert_eq!(
            detect_env_with(PetsGlyphMode::Auto, &plain),
            (PetRenderCaps { pixel: true }, false)
        );

        let tmux = FakeProbe::ok()
            .with_env("TMUX", "/tmp/tmux-1000/default,123,0")
            .with_env("TERM", "screen-256color");
        assert_eq!(
            detect_env_with(PetsGlyphMode::Auto, &tmux),
            (PetRenderCaps { pixel: true }, true)
        );
    }

    #[test]
    fn standalone_explicit_pixel_bypasses_term_allowlist() {
        let plain = FakeProbe::ok().with_env("TERM", "xterm-256color");

        assert_eq!(
            detect_env_with(PetsGlyphMode::Pixel, &plain),
            (PetRenderCaps { pixel: true }, false)
        );
    }
}
