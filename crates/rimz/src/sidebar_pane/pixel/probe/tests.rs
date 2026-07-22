use super::*;
use std::cell::RefCell;
use std::collections::BTreeMap;

const TEST_SESSION: &str = "rimz-test";

fn process(pid: u32, ppid: u32, cmdline: &str) -> crate::proc::ProcInfo {
    crate::proc::ProcInfo {
        pid,
        ppid,
        real_uid: 1000,
        cmdline: cmdline.to_owned(),
    }
}

struct FakeProbe {
    version: Option<String>,
    allow_passthrough: Option<String>,
    expected_session: String,
    termnames: Option<Vec<String>>,
    session_name: Option<String>,
    processes: Vec<crate::proc::ProcInfo>,
    daemon_record: Option<(u32, u32)>,
    env: BTreeMap<String, String>,
    passthrough_targets: RefCell<Vec<String>>,
    passthrough_all_panes: RefCell<Vec<String>>,
}

impl FakeProbe {
    fn ok() -> Self {
        Self {
            version: Some("tmux 3.6b".to_owned()),
            allow_passthrough: Some("on".to_owned()),
            expected_session: TEST_SESSION.to_owned(),
            termnames: Some(vec!["xterm-ghostty".to_owned()]),
            session_name: Some(TEST_SESSION.to_owned()),
            processes: Vec::new(),
            daemon_record: None,
            env: BTreeMap::new(),
            passthrough_targets: RefCell::new(Vec::new()),
            passthrough_all_panes: RefCell::new(Vec::new()),
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

    fn tmux_allow_passthrough(&self, target: &str) -> io::Result<String> {
        self.passthrough_targets
            .borrow_mut()
            .push(target.to_owned());
        self.allow_passthrough
            .clone()
            .ok_or_else(|| io::Error::other("tmux passthrough unavailable"))
    }

    fn tmux_set_pane_passthrough_all(&self, pane: &str) -> io::Result<()> {
        self.passthrough_all_panes
            .borrow_mut()
            .push(pane.to_owned());
        Ok(())
    }

    fn tmux_rendering_clients(&self, session_name: &str) -> io::Result<Vec<RenderingClient>> {
        assert_eq!(session_name, self.expected_session);
        self.termnames
            .clone()
            .map(|termnames| {
                termnames
                    .into_iter()
                    .enumerate()
                    .map(|(index, termname)| RenderingClient {
                        termname,
                        pid: 100 + index as u32,
                    })
                    .collect()
            })
            .ok_or_else(|| io::Error::other("tmux termnames unavailable"))
    }

    fn tmux_session_name(&self) -> io::Result<String> {
        self.session_name
            .clone()
            .ok_or_else(|| io::Error::other("tmux session unavailable"))
    }

    fn processes(&self) -> Vec<crate::proc::ProcInfo> {
        self.processes.clone()
    }

    fn pixel_daemon_record(&self) -> Option<(u32, u32)> {
        self.daemon_record
    }

    fn env_var(&self, key: &str) -> Option<String> {
        self.env.get(key).cloned()
    }
}

#[test]
fn tmux_passthrough_probe_targets_own_pane_then_session() {
    let own_pane = FakeProbe::ok().with_env("TMUX_PANE", "%7");
    detect_with(
        MuxName::Tmux,
        TEST_SESSION,
        PixelRenderCaps::default(),
        &own_pane,
    );
    assert_eq!(&*own_pane.passthrough_targets.borrow(), &["%7"]);

    let session = FakeProbe::ok();
    detect_with(
        MuxName::Tmux,
        TEST_SESSION,
        PixelRenderCaps::default(),
        &session,
    );
    assert_eq!(
        &*session.passthrough_targets.borrow(),
        &[TEST_SESSION.to_owned()]
    );
}

#[test]
fn tmux_sidebar_escalates_own_inherited_passthrough() {
    let probe = FakeProbe::ok().with_env("TMUX_PANE", "%7");

    escalate_with(&probe).expect("escalation");

    assert_eq!(&*probe.passthrough_targets.borrow(), &["%7"]);
    assert_eq!(&*probe.passthrough_all_panes.borrow(), &["%7"]);
}

#[test]
fn tmux_sidebar_preserves_all_off_and_missing_pane() {
    for allow_passthrough in ["all", "off"] {
        let probe = FakeProbe {
            allow_passthrough: Some(allow_passthrough.to_owned()),
            ..FakeProbe::ok().with_env("TMUX_PANE", "%7")
        };

        escalate_with(&probe).expect("no-op");

        assert_eq!(&*probe.passthrough_targets.borrow(), &["%7"]);
        assert!(probe.passthrough_all_panes.borrow().is_empty());
    }

    let missing_pane = FakeProbe::ok();
    escalate_with(&missing_pane).expect("no-op");
    assert!(missing_pane.passthrough_targets.borrow().is_empty());
    assert!(missing_pane.passthrough_all_panes.borrow().is_empty());
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
            kitty_clients: true,
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
            kitty_clients: true,
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
            kitty_clients: true,
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
            kitty_clients: true,
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
            kitty_clients: false,
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
            kitty_clients: false,
        }
    );
}

#[test]
fn tmux_probe_failures_keep_previous_fact_values() {
    let prev = PixelRenderCaps {
        pixel_transport: true,
        kitty_clients: true,
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
            kitty_clients: false,
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
            kitty_clients: true,
        }
    );
}

#[test]
fn tmux_empty_rendering_client_list_keeps_previous_kitty_fact() {
    let prev = PixelRenderCaps {
        pixel_transport: false,
        kitty_clients: true,
    };
    let unattached = FakeProbe {
        termnames: Some(Vec::new()),
        ..FakeProbe::ok()
    };

    assert_eq!(
        detect_with(MuxName::Tmux, TEST_SESSION, prev, &unattached),
        PixelRenderCaps {
            pixel_transport: true,
            kitty_clients: true,
        }
    );
}

#[test]
fn tmux_successful_probe_overrides_previous_facts() {
    let prev = PixelRenderCaps {
        pixel_transport: true,
        kitty_clients: true,
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
            kitty_clients: true,
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
            .kitty_clients,
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
        .kitty_clients
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
        .kitty_clients
    );

    assert_eq!(
        rendering_clients("0 xterm-ghostty 42\n1 tmux-256color 7"),
        vec![RenderingClient {
            termname: "xterm-ghostty".to_owned(),
            pid: 42,
        }]
    );

    let probe = FakeProbe {
        termnames: Some(vec!["xterm-ghostty".to_owned()]),
        ..FakeProbe::ok()
    };
    assert!(
        detect_with(
            MuxName::Tmux,
            TEST_SESSION,
            PixelRenderCaps::default(),
            &probe
        )
        .kitty_clients
    );
}

#[test]
fn ttyd_descendant_client_requires_live_matching_protocol_daemon() {
    let capable = FakeProbe {
        termnames: Some(vec!["xterm-256color".to_owned()]),
        processes: vec![
            process(10, 1, "/usr/bin/ttyd -p 8200"),
            process(100, 10, "tmux attach -t rimz-test"),
        ],
        daemon_record: Some((10, crate::web::TTYD_PIXEL_PROTOCOL)),
        ..FakeProbe::ok()
    };
    assert!(
        detect_with(
            MuxName::Tmux,
            TEST_SESSION,
            PixelRenderCaps::default(),
            &capable,
        )
        .kitty_clients
    );

    for rejected in [
        FakeProbe {
            termnames: Some(vec!["xterm-256color".to_owned()]),
            processes: capable.processes.clone(),
            daemon_record: None,
            ..FakeProbe::ok()
        },
        FakeProbe {
            termnames: Some(vec!["xterm-256color".to_owned()]),
            processes: capable.processes.clone(),
            daemon_record: Some((10, crate::web::TTYD_PIXEL_PROTOCOL + 1)),
            ..FakeProbe::ok()
        },
        FakeProbe {
            termnames: Some(vec!["xterm-256color".to_owned()]),
            processes: vec![process(100, 10, "tmux attach -t rimz-test")],
            daemon_record: Some((10, crate::web::TTYD_PIXEL_PROTOCOL)),
            ..FakeProbe::ok()
        },
        FakeProbe {
            termnames: Some(vec!["xterm-256color".to_owned()]),
            processes: vec![
                process(10, 1, "sleep 60"),
                process(100, 10, "tmux attach -t rimz-test"),
            ],
            daemon_record: Some((10, crate::web::TTYD_PIXEL_PROTOCOL)),
            ..FakeProbe::ok()
        },
    ] {
        assert!(
            !detect_with(
                MuxName::Tmux,
                TEST_SESSION,
                PixelRenderCaps::default(),
                &rejected,
            )
            .kitty_clients
        );
    }
}

#[test]
fn ttyd_ancestry_walk_is_bounded_to_four_hops() {
    let probe = FakeProbe {
        termnames: Some(vec!["xterm-256color".to_owned()]),
        processes: vec![
            process(10, 1, "ttyd -p 8200"),
            process(100, 20, "tmux attach"),
            process(20, 21, "rimz web exec"),
            process(21, 22, "sh"),
            process(22, 23, "sh"),
            process(23, 10, "sh"),
        ],
        daemon_record: Some((10, crate::web::TTYD_PIXEL_PROTOCOL)),
        ..FakeProbe::ok()
    };

    assert!(
        !detect_with(
            MuxName::Tmux,
            TEST_SESSION,
            PixelRenderCaps::default(),
            &probe,
        )
        .kitty_clients
    );
}

#[test]
fn native_and_ttyd_clients_share_the_all_clients_gate() {
    let capable = FakeProbe {
        termnames: Some(vec!["kitty".to_owned(), "xterm-256color".to_owned()]),
        processes: vec![
            process(10, 1, "ttyd -p 8200"),
            process(101, 10, "tmux attach"),
        ],
        daemon_record: Some((10, crate::web::TTYD_PIXEL_PROTOCOL)),
        ..FakeProbe::ok()
    };
    assert!(
        detect_with(
            MuxName::Tmux,
            TEST_SESSION,
            PixelRenderCaps::default(),
            &capable,
        )
        .kitty_clients
    );

    let foreign = FakeProbe {
        termnames: Some(vec![
            "kitty".to_owned(),
            "xterm-256color".to_owned(),
            "screen-256color".to_owned(),
        ]),
        processes: capable.processes.clone(),
        daemon_record: capable.daemon_record,
        ..FakeProbe::ok()
    };
    assert!(
        !detect_with(
            MuxName::Tmux,
            TEST_SESSION,
            PixelRenderCaps::default(),
            &foreign,
        )
        .kitty_clients
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
                kitty_clients: true,
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
                kitty_clients: true,
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
                kitty_clients: false,
            },
            false
        )
    );
}
