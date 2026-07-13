//! Probes terminal and tmux capabilities for shared kitty graphics support.

use std::io;
use std::time::Duration;

use crate::ids::MuxName;
use crate::mux::CommandSpec;

const MIN_PIXEL_TMUX_VERSION: (u32, u32, u32) = (3, 6, 0);
const COMMAND_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PixelRenderCaps {
    pub pixel_transport: bool,
    pub kitty_term: bool,
}

pub(crate) fn detect(mux: MuxName, session_name: &str, prev: PixelRenderCaps) -> PixelRenderCaps {
    detect_with(mux, session_name, prev, &LiveProbe)
}

pub fn detect_env() -> (PixelRenderCaps, bool) {
    detect_env_with(&LiveProbe)
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
    session_name: &str,
    prev: PixelRenderCaps,
    probe: &impl Probe,
) -> PixelRenderCaps {
    match probed_mux {
        MuxName::Tmux => detect_tmux(session_name, probe, prev),
        MuxName::Zellij => detect_zellij(probe),
    }
}

fn detect_env_with(probe: &impl Probe) -> (PixelRenderCaps, bool) {
    if env_present(probe, "TMUX") {
        let caps = probe
            .tmux_session_name()
            .map(|session_name| detect_tmux(&session_name, probe, PixelRenderCaps::default()))
            .unwrap_or_default();
        return (caps, true);
    }
    if env_present(probe, "ZELLIJ") {
        return (detect_zellij(probe), false);
    }
    (detect_standalone(probe), false)
}

fn detect_tmux(session_name: &str, probe: &impl Probe, prev: PixelRenderCaps) -> PixelRenderCaps {
    let kitty_term = match probe.tmux_client_termnames(session_name) {
        Ok(termnames) if !termnames.is_empty() => termnames_allowed(&termnames),
        _ => prev.kitty_term,
    };
    let pixel_transport = match (probe.tmux_version(), probe.tmux_allow_passthrough()) {
        (Ok(version), Ok(allow)) => {
            let version_ok = crate::mux::tmux::parse_version(&version)
                .is_some_and(|version| version >= MIN_PIXEL_TMUX_VERSION);
            let passthrough_ok = matches!(allow.trim(), "on" | "all");
            version_ok && passthrough_ok
        }
        _ => prev.pixel_transport,
    };
    PixelRenderCaps {
        pixel_transport,
        kitty_term,
    }
}

fn detect_zellij(_probe: &impl Probe) -> PixelRenderCaps {
    PixelRenderCaps::default()
}

fn detect_standalone(probe: &impl Probe) -> PixelRenderCaps {
    PixelRenderCaps {
        pixel_transport: true,
        kitty_term: standalone_term_allowed(probe),
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
    termnames.iter().all(|termname| termname_allowed(termname))
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
                TEST_SESSION,
                PixelRenderCaps::default(),
                &FakeProbe::ok()
            ),
            PixelRenderCaps {
                pixel_transport: true,
                kitty_term: true,
            }
        );

        assert_eq!(
            detect_with(
                MuxName::Zellij,
                TEST_SESSION,
                PixelRenderCaps::default(),
                &FakeProbe::ok().with_env("TERM", "xterm-ghostty")
            ),
            PixelRenderCaps::default()
        );

        let old = FakeProbe {
            version: Some("tmux 3.5a".to_owned()),
            ..FakeProbe::ok()
        };
        assert_eq!(
            detect_with(
                MuxName::Tmux,
                TEST_SESSION,
                PixelRenderCaps::default(),
                &old
            ),
            PixelRenderCaps {
                pixel_transport: false,
                kitty_term: true,
            }
        );

        let off = FakeProbe {
            allow_passthrough: Some("off".to_owned()),
            ..FakeProbe::ok()
        };
        assert_eq!(
            detect_with(
                MuxName::Tmux,
                TEST_SESSION,
                PixelRenderCaps::default(),
                &off
            ),
            PixelRenderCaps {
                pixel_transport: false,
                kitty_term: true,
            }
        );

        let all = FakeProbe {
            allow_passthrough: Some("all\n".to_owned()),
            ..FakeProbe::ok()
        };
        assert_eq!(
            detect_with(
                MuxName::Tmux,
                TEST_SESSION,
                PixelRenderCaps::default(),
                &all
            ),
            PixelRenderCaps {
                pixel_transport: true,
                kitty_term: true,
            }
        );

        let unsupported_term = FakeProbe {
            termnames: Some(vec!["screen-256color".to_owned()]),
            ..FakeProbe::ok()
        };
        assert_eq!(
            detect_with(
                MuxName::Tmux,
                TEST_SESSION,
                PixelRenderCaps::default(),
                &unsupported_term
            ),
            PixelRenderCaps {
                pixel_transport: true,
                kitty_term: false,
            }
        );

        let unattached = FakeProbe {
            termnames: Some(Vec::new()),
            ..FakeProbe::ok()
        };
        assert_eq!(
            detect_with(
                MuxName::Tmux,
                TEST_SESSION,
                PixelRenderCaps::default(),
                &unattached
            ),
            PixelRenderCaps {
                pixel_transport: true,
                kitty_term: false,
            }
        );
    }

    #[test]
    fn tmux_probe_failures_keep_previous_fact_values() {
        let prev = PixelRenderCaps {
            pixel_transport: true,
            kitty_term: true,
        };

        let version_error = FakeProbe {
            version: None,
            termnames: Some(vec!["screen-256color".to_owned()]),
            ..FakeProbe::ok()
        };
        assert_eq!(
            detect_with(MuxName::Tmux, TEST_SESSION, prev, &version_error),
            PixelRenderCaps {
                pixel_transport: true,
                kitty_term: false,
            }
        );

        let passthrough_error = FakeProbe {
            allow_passthrough: None,
            ..FakeProbe::ok()
        };
        assert_eq!(
            detect_with(MuxName::Tmux, TEST_SESSION, prev, &passthrough_error),
            prev
        );

        let term_error = FakeProbe {
            termnames: None,
            allow_passthrough: Some("off".to_owned()),
            ..FakeProbe::ok()
        };
        assert_eq!(
            detect_with(MuxName::Tmux, TEST_SESSION, prev, &term_error),
            PixelRenderCaps {
                pixel_transport: false,
                kitty_term: true,
            }
        );
    }

    #[test]
    fn tmux_empty_rendering_client_list_keeps_previous_kitty_fact() {
        let prev = PixelRenderCaps {
            pixel_transport: false,
            kitty_term: true,
        };
        let unattached = FakeProbe {
            termnames: Some(Vec::new()),
            ..FakeProbe::ok()
        };

        assert_eq!(
            detect_with(MuxName::Tmux, TEST_SESSION, prev, &unattached),
            PixelRenderCaps {
                pixel_transport: true,
                kitty_term: true,
            }
        );
    }

    #[test]
    fn tmux_successful_probe_overrides_previous_facts() {
        let prev = PixelRenderCaps {
            pixel_transport: true,
            kitty_term: true,
        };
        let unsupported = FakeProbe {
            version: Some("tmux 3.5a".to_owned()),
            allow_passthrough: Some("off".to_owned()),
            termnames: Some(vec!["screen-256color".to_owned()]),
            ..FakeProbe::ok()
        };

        assert_eq!(
            detect_with(MuxName::Tmux, TEST_SESSION, prev, &unsupported),
            PixelRenderCaps::default()
        );
        assert_eq!(
            detect_with(
                MuxName::Tmux,
                TEST_SESSION,
                PixelRenderCaps::default(),
                &FakeProbe::ok()
            ),
            PixelRenderCaps {
                pixel_transport: true,
                kitty_term: true,
            }
        );
    }

    #[test]
    fn termname_gate_filters_control_clients_and_requires_all_to_match() {
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
                detect_with(
                    MuxName::Tmux,
                    TEST_SESSION,
                    PixelRenderCaps::default(),
                    &probe
                )
                .kitty_term,
                "{termname} should enable pixels"
            );
        }

        let allowed = FakeProbe {
            termnames: Some(vec!["xterm-ghostty".to_owned(), "kitty".to_owned()]),
            ..FakeProbe::ok()
        };
        assert!(
            detect_with(
                MuxName::Tmux,
                TEST_SESSION,
                PixelRenderCaps::default(),
                &allowed
            )
            .kitty_term
        );

        let mixed = FakeProbe {
            termnames: Some(vec![
                "xterm-ghostty".to_owned(),
                "screen-256color".to_owned(),
            ]),
            ..FakeProbe::ok()
        };
        assert!(
            !detect_with(
                MuxName::Tmux,
                TEST_SESSION,
                PixelRenderCaps::default(),
                &mixed
            )
            .kitty_term
        );

        assert_eq!(
            rendering_termnames("0 xterm-ghostty\n1 tmux-256color"),
            vec!["xterm-ghostty".to_owned()]
        );

        let probe = FakeProbe {
            termnames: Some(rendering_termnames("0 xterm-ghostty\n1 tmux-256color")),
            ..FakeProbe::ok()
        };
        assert!(
            detect_with(
                MuxName::Tmux,
                TEST_SESSION,
                PixelRenderCaps::default(),
                &probe
            )
            .kitty_term
        );
    }

    #[test]
    fn standalone_env_detects_native_kitty_and_wrap_mode() {
        let plain = FakeProbe::ok().with_env("TERM", "xterm-kitty");
        assert_eq!(
            detect_env_with(&plain),
            (
                PixelRenderCaps {
                    pixel_transport: true,
                    kitty_term: true,
                },
                false
            )
        );

        let tmux = FakeProbe::ok()
            .with_env("TMUX", "/tmp/tmux-1000/default,123,0")
            .with_env("TERM", "screen-256color");
        assert_eq!(
            detect_env_with(&tmux),
            (
                PixelRenderCaps {
                    pixel_transport: true,
                    kitty_term: true,
                },
                true
            )
        );

        let zellij = FakeProbe::ok()
            .with_env("ZELLIJ", "1")
            .with_env("TERM", "xterm-kitty");
        assert_eq!(
            detect_env_with(&zellij),
            (PixelRenderCaps::default(), false)
        );

        let unsupported = FakeProbe::ok().with_env("TERM", "xterm-256color");
        assert_eq!(
            detect_env_with(&unsupported),
            (
                PixelRenderCaps {
                    pixel_transport: true,
                    kitty_term: false,
                },
                false
            )
        );
    }
}
