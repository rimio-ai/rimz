use std::os::unix::fs::PermissionsExt;

use super::*;

#[test]
fn gate_listener_does_not_inherit_the_room_pin() {
    let spec = gate_spec(
        Path::new("/opt/rimz/bin/rimz"),
        "127.0.0.1:8200".parse().unwrap(),
        "127.0.0.1:8201".parse().unwrap(),
        &[],
        &WebAuth::Basic,
        &[],
    );
    assert!(spec.env_remove.contains(crate::workspace::ENV_WORKSPACE_ID));
}

#[test]
fn pixel_daemon_filter_requires_current_protocol_records() {
    // An inherited RIMZ_HOME can hold a live daemon's records; never clobber them.
    if [DAEMON_FILE, SHARE_DAEMON_FILE]
        .iter()
        .any(|file| state_path(file).exists())
    {
        return;
    }
    let pids = crate::web::pixel_ttyd_pids;
    let mut writable = WritableDaemonRecord::basic_loopback(42, 8200);
    writable.process.pixel_protocol = Some(crate::web::TTYD_PIXEL_PROTOCOL);
    let mut broadcast = writable.process.clone();
    broadcast.pid = 43;
    broadcast.port = 8201;
    write_daemon(&writable).expect("writable record");
    write_cache_json(&state_path(SHARE_DAEMON_FILE), &broadcast).expect("broadcast record");
    assert_eq!(pids(), [42, 43]);

    for protocol in [
        Some(crate::web::TTYD_PIXEL_PROTOCOL - 1),
        Some(crate::web::TTYD_PIXEL_PROTOCOL + 1),
        None,
    ] {
        writable.process.pixel_protocol = protocol;
        write_daemon(&writable).expect("writable record");
        assert_eq!(pids(), [43]);
        broadcast.pixel_protocol = protocol;
        write_cache_json(&state_path(SHARE_DAEMON_FILE), &broadcast).expect("broadcast record");
        assert!(pids().is_empty());
        broadcast.pixel_protocol = Some(crate::web::TTYD_PIXEL_PROTOCOL);
        write_cache_json(&state_path(SHARE_DAEMON_FILE), &broadcast).expect("broadcast record");
    }

    fs::remove_file(state_path(DAEMON_FILE)).expect("remove writable");
    assert_eq!(pids(), [43]);
    fs::write(state_path(DAEMON_FILE), b"corrupt").expect("corrupt writable");
    assert_eq!(pids(), [43]);
    fs::write(state_path(SHARE_DAEMON_FILE), b"corrupt").expect("corrupt broadcast");
    assert!(pids().is_empty());
    fs::remove_file(state_path(DAEMON_FILE)).expect("remove writable");
    fs::remove_file(state_path(SHARE_DAEMON_FILE)).expect("remove broadcast");
    assert!(pids().is_empty());
}

fn credential() -> WebCredential {
    WebCredential {
        username: "rimz".to_owned(),
        secret: "secret".to_owned(),
    }
}

fn profile_with_index(index_key: Option<&str>) -> ClientProfile {
    ClientProfile {
        index_key: index_key.map(str::to_owned),
        ..ClientProfile::default()
    }
}

#[test]
fn broadcast_argv_has_no_write_or_auth_and_uses_share_shim() {
    let spec = spawn_spec_for(
        Path::new("/tmp/ttyd"),
        Path::new("/opt/rimz/bin/rimz"),
        "127.0.0.1".parse().expect("IP"),
        8201,
        TtydMode::Broadcast,
        &["-t".to_owned(), "cursorBlink=false".to_owned()],
    );

    assert_eq!(
        spec.args,
        [
            "-O",
            "-a",
            "-P",
            "3600",
            "-i",
            "127.0.0.1",
            "-p",
            "8201",
            "-t",
            "cursorBlink=false",
            "/opt/rimz/bin/rimz",
            "web",
            "exec",
            "--share"
        ]
    );
    assert!(spec.args.windows(2).any(|args| args == ["-P", "3600"]));
    assert!(!spec.args.iter().any(|arg| arg == "-W"));
    assert!(!spec.args.iter().any(|arg| arg == "-c"));
    assert!(
        super::super::TTYD_AMBIENT_CONTEXT_ENV
            .into_iter()
            .flatten()
            .all(|key| spec.env_remove.contains(*key))
    );
    assert!(spec.env_remove.contains("ZELLIJ_SESSION_NAME"));
    assert!(spec.env_remove.contains("RIMZ_AGENT_KIND"));
}

#[test]
fn allowlist_roundtrips_sorted_sessions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("web-share.json");
    let mut allowlist = Allowlist {
        sessions: vec!["rimz-b".to_owned(), "rimz-a".to_owned()],
    };
    normalize_sessions(&mut allowlist.sessions);
    write_cache_json(&path, &allowlist).expect("write allowlist");

    assert_eq!(
        read_json_optional(&path).expect("read allowlist"),
        Some(Allowlist {
            sessions: vec!["rimz-a".to_owned(), "rimz-b".to_owned()]
        })
    );
}

#[test]
fn broadcast_state_roundtrips_the_launch_context_marker() {
    let daemon = TtydProcessRecord {
        pid: 42,
        port: 8201,
        interface: "0.0.0.0".to_owned(),
        launch_context_scrubbed: true,
        pixel_protocol: Some(crate::web::TTYD_PIXEL_PROTOCOL),
        index_key: Some("generated-index-key".to_owned()),
    };
    let bytes = serde_json::to_vec(&daemon).expect("serialize daemon state");

    assert_eq!(
        serde_json::from_slice::<TtydProcessRecord>(&bytes).expect("parse daemon state"),
        daemon
    );
}

#[test]
fn old_broadcast_state_defaults_to_unscrubbed_launch_context() {
    let daemon: TtydProcessRecord =
        serde_json::from_str(r#"{"pid":42,"port":8201}"#).expect("old record");

    assert!(!daemon.launch_context_scrubbed);
    assert_eq!(daemon.pixel_protocol, None);
    assert_eq!(daemon.index_key, None);
}

#[test]
fn broadcast_reuse_requires_context_marker_and_generated_index_key() {
    let desired = ListenerSpec {
        port: 8201,
        interface: "127.0.0.1".to_owned(),
    };
    let mut daemon = TtydProcessRecord {
        pid: 42,
        port: desired.port,
        interface: desired.interface.clone(),
        launch_context_scrubbed: true,
        pixel_protocol: None,
        index_key: None,
    };

    assert!(daemon.matches(&desired, &profile_with_index(None)));
    daemon.launch_context_scrubbed = false;
    assert!(!daemon.matches(&desired, &profile_with_index(None)));
    daemon.launch_context_scrubbed = true;
    assert!(!daemon.matches(&desired, &profile_with_index(Some("current"))));
    daemon.index_key = Some("current".to_owned());
    assert!(daemon.matches(&desired, &profile_with_index(Some("current"))));
    daemon.index_key = Some("stale".to_owned());
    assert!(!daemon.matches(&desired, &profile_with_index(Some("current"))));
}

#[test]
fn ttyd_version_gate_accepts_the_minimum_and_rejects_the_previous_patch() {
    assert!(matches!(
        require_supported_version("ttyd version 1.7.4"),
        Err(WebErr::TtydTooOld { found, minimum })
            if found == "1.7.4" && minimum == "1.7.5"
    ));
    assert_eq!(
        require_supported_version("ttyd version 1.7.5").expect("minimum ttyd version"),
        MIN_TTYD_VERSION
    );
    assert_eq!(
        require_supported_version("ttyd version 1.7.7-1+deb13u1").expect("packaged ttyd version"),
        TtydVersion::new(1, 7, 7)
    );
}

#[test]
fn malformed_ttyd_versions_fail_with_the_reported_value() {
    for reported in ["", "ttyd 1.7.7", "ttyd version 1.7", "ttyd version current"] {
        assert!(matches!(
            require_supported_version(reported),
            Err(WebErr::TtydTooOld { found, minimum })
                if found == reported && minimum == "1.7.5"
        ));
    }
}

#[test]
fn argv_uses_loopback_auth_url_args_and_rimz_shim() {
    let spec = spawn_spec_for(
        Path::new("/tmp/ttyd"),
        Path::new("/opt/rimz/bin/rimz"),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        8201,
        TtydMode::Writable(&credential()),
        &["-t".to_owned(), "macOptionIsMeta=true".to_owned()],
    );
    assert_eq!(
        spec.args,
        [
            "-W",
            "-O",
            "-a",
            "-P",
            "3600",
            "-c",
            "rimz:secret",
            "-i",
            "127.0.0.1",
            "-p",
            "8201",
            "-t",
            "macOptionIsMeta=true",
            "/opt/rimz/bin/rimz",
            "web",
            "exec"
        ]
    );
    assert!(spec.args.windows(2).any(|args| args == ["-P", "3600"]));
    assert!(
        super::super::TTYD_AMBIENT_CONTEXT_ENV
            .into_iter()
            .flatten()
            .all(|key| spec.env_remove.contains(*key))
    );
    assert!(spec.env_remove.contains("ZELLIJ_SESSION_NAME"));
    assert!(spec.env_remove.contains("RIMZ_AGENT_KIND"));
}

#[test]
fn argv_uses_basic_auth_for_a_trusted_header_daemon() {
    let spec = spawn_spec_for(
        Path::new("/tmp/ttyd"),
        Path::new("/opt/rimz/bin/rimz"),
        "127.0.0.1".parse().expect("IP"),
        8399,
        TtydMode::Writable(&credential()),
        &[],
    );

    assert!(
        spec.args
            .windows(2)
            .any(|args| args == ["-c", "rimz:secret"])
    );
    assert!(!spec.args.iter().any(|arg| arg == "-H"));
    assert!(spec.args.windows(2).any(|args| args == ["-i", "127.0.0.1"]));
}

#[test]
fn styled_argv_keeps_all_client_options_before_rimz_shim() {
    let extra = vec![
        "-t".to_owned(),
        "macOptionIsMeta=true".to_owned(),
        "-t".to_owned(),
        "fontFamily=RimZ Font,monospace".to_owned(),
        "-t".to_owned(),
        "theme={\"background\":\"#010203\"}".to_owned(),
        "-I".to_owned(),
        "/cache/index.html".to_owned(),
    ];
    let spec = spawn_spec_for(
        Path::new("/tmp/ttyd"),
        Path::new("/opt/rimz/bin/rimz"),
        "127.0.0.1".parse().expect("IP"),
        8202,
        TtydMode::Writable(&credential()),
        &extra,
    );

    let shim = spec
        .args
        .iter()
        .position(|arg| arg == "/opt/rimz/bin/rimz")
        .expect("shim argv");
    assert_eq!(&spec.args[shim - extra.len()..shim], extra);
}

#[test]
fn ephemeral_port_is_bindable() {
    let port = choose_ephemeral_port().expect("ephemeral port");
    TcpListener::bind(("127.0.0.1", port)).expect("port released after selection");
}

#[test]
fn unspecified_listener_probes_use_the_matching_loopback_family() {
    assert_eq!(
        probe_address("0.0.0.0:8200".parse().expect("IPv4 socket")),
        "127.0.0.1:8200".parse().expect("IPv4 loopback socket")
    );
    assert_eq!(
        probe_address("[::]:8200".parse().expect("IPv6 socket")),
        "[::1]:8200".parse().expect("IPv6 loopback socket")
    );
}

#[test]
fn credential_roundtrip_is_private() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("credential.json");
    let credential = credential();
    atomic::write_private_temp_then_rename(&path, &credential).expect("write");
    assert_eq!(read_json_optional(&path).expect("read"), Some(credential));
    assert_eq!(
        fs::metadata(path).expect("metadata").permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn daemon_state_roundtrips() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("daemon.json");
    let daemon = WritableDaemonRecord {
        process: TtydProcessRecord {
            pid: u32::MAX,
            port: 8200,
            interface: "0.0.0.0".to_owned(),
            launch_context_scrubbed: true,
            pixel_protocol: Some(crate::web::TTYD_PIXEL_PROTOCOL),
            index_key: Some("generated-index-key".to_owned()),
        },
        auth: WebAuth::TrustedHeader {
            header: "X-Forwarded-User".to_owned(),
        },
        auth_users: vec!["alice".to_owned()],
        trusted_proxies: vec!["10.0.0.0/8".to_owned()],
        gate: Some(GateRecord {
            pid: u32::MAX - 1,
            upstream_port: 41820,
        }),
        token_auth: true,
    };
    write_cache_json(&path, &daemon).expect("write daemon state");
    assert_eq!(read_json_optional(&path).expect("read state"), Some(daemon));
}

#[test]
fn old_daemon_state_defaults_to_non_reusable_markers() {
    let daemon: WritableDaemonRecord =
        serde_json::from_str(r#"{"pid":42,"port":8200}"#).expect("old record");
    assert_eq!(daemon.process.pid, 42);
    assert_eq!(daemon.auth, WebAuth::Basic);
    assert!(daemon.auth_users.is_empty());
    assert!(daemon.gate.is_none());
    assert!(!daemon.token_auth);
    assert!(!daemon.process.launch_context_scrubbed);
    assert_eq!(daemon.process.index_key, None);
}

#[test]
fn desired_spec_validates_listener_and_proxy_cidrs() {
    let mut config = MachineConfig::default();
    config.web.interface = "localhost".to_owned();
    assert!(matches!(
        desired_spec(&config),
        Err(WebErr::InvalidInterface { value }) if value == "localhost"
    ));

    config.web.interface = "127.0.0.1".to_owned();
    config.web.trusted_proxies = vec!["10.0.0.0/33".to_owned()];
    assert!(matches!(
        desired_spec(&config),
        Err(WebErr::InvalidTrustedProxy { value, .. }) if value == "10.0.0.0/33"
    ));
}

#[test]
fn desired_spec_validates_and_trims_auth_users() {
    let mut config = MachineConfig::default();
    config.web.auth_users = vec!["alice".to_owned()];
    assert!(matches!(
        desired_spec(&config),
        Err(WebErr::AuthUsersRequireHeader)
    ));

    config.web.auth_header = Some(" \t ".to_owned());
    assert!(matches!(
        desired_spec(&config),
        Err(WebErr::AuthUsersRequireHeader)
    ));

    config.web.auth_header = Some("X-Forwarded-User".to_owned());
    config.web.auth_users = vec![" \t ".to_owned()];
    assert!(matches!(desired_spec(&config), Err(WebErr::EmptyAuthUser)));

    config.web.auth_users = vec![" alice ".to_owned(), "bob".to_owned()];
    let desired = desired_spec(&config).expect("valid auth users");
    assert_eq!(desired.auth_users, ["alice", "bob"]);
}

#[test]
fn non_loopback_trusted_header_auth_warns_without_a_proxy_allowlist() {
    let spec = WritableDaemonSpec {
        listener: ListenerSpec {
            port: 8200,
            interface: "0.0.0.0".to_owned(),
        },
        auth: WebAuth::TrustedHeader {
            header: "X-Forwarded-User".to_owned(),
        },
        auth_users: Vec::new(),
        trusted_proxies: Vec::new(),
    };
    assert!(matches!(
        auth_warnings(&spec).as_slice(),
        [WebWarning::HeaderAuthUnprotected(message)] if message.contains("trusted_proxies")
    ));
}

#[test]
fn pre_pixel_daemon_state_deserializes_without_protocol() {
    let daemon = serde_json::from_str::<WritableDaemonRecord>(r#"{"pid":42,"port":8200}"#)
        .expect("legacy daemon state");

    assert_eq!(daemon.process.pid, 42);
    assert_eq!(daemon.process.port, 8200);
    assert_eq!(daemon.process.pixel_protocol, None);
    assert_eq!(daemon.process.index_key, None);
}

#[test]
fn daemon_reuse_requires_context_marker_and_generated_index_key() {
    let config = MachineConfig::default();
    let desired = desired_spec(&config).expect("desired daemon");
    let mut daemon = WritableDaemonRecord::basic_loopback(42, config.web.port);
    let mut profile = ClientProfile::default();

    assert!(!record_matches(&daemon, &desired, &profile));
    daemon.gate = Some(GateRecord {
        pid: 43,
        upstream_port: 41820,
    });
    assert!(!record_matches(&daemon, &desired, &profile));
    daemon.token_auth = true;
    assert!(record_matches(&daemon, &desired, &profile));
    daemon.process.launch_context_scrubbed = false;
    assert!(!record_matches(&daemon, &desired, &profile));
    daemon.process.launch_context_scrubbed = true;
    daemon.token_auth = false;
    assert!(!record_matches(&daemon, &desired, &profile));
    daemon.token_auth = true;
    profile.index_key = Some("current".to_owned());
    assert!(!record_matches(&daemon, &desired, &profile));
    daemon.process.index_key = profile.index_key.clone();
    assert!(record_matches(&daemon, &desired, &profile));
    daemon.process.index_key = Some("stale".to_owned());
    assert!(!record_matches(&daemon, &desired, &profile));
    daemon.process.index_key = profile.index_key.clone();
    daemon.process.pixel_protocol = Some(crate::web::TTYD_PIXEL_PROTOCOL - 1);
    assert!(record_matches(&daemon, &desired, &profile));
    daemon.process.index_key = None;
    daemon.process.pixel_protocol = None;
    profile.index_key = None;
    assert!(record_matches(&daemon, &desired, &profile));
    daemon.auth_users.push("alice".to_owned());
    assert!(!record_matches(&daemon, &desired, &profile));
}

#[test]
fn pid_guard_requires_ttyd_as_the_program() {
    let process = |cmdline: &str| crate::proc::ProcInfo {
        pid: 42,
        ppid: 1,
        real_uid: 1000,
        cmdline: cmdline.to_owned(),
    };
    assert!(is_ttyd_process(&process("/usr/bin/ttyd -p 8200 sh")));
    assert!(!is_ttyd_process(&process("/usr/bin/ttyd-trace -p 8200")));
    assert!(!is_ttyd_process(&process("sh -c ttyd -p 8200")));
    assert!(!is_ttyd_process(&process("env A=b ttyd -p 8200")));
    assert!(is_gate_process(&process(
        "/opt/rimz/bin/rimz web gate --listen 0.0.0.0:8200"
    )));
    assert!(!is_gate_process(&process("/opt/rimz/bin/rimz web start")));
    assert!(!is_gate_process(&process(
        "sh -c rimz web gate --listen 0.0.0.0:8200"
    )));
    assert!(!is_gate_process(&process(
        "env A=b rimz web gate --listen 0.0.0.0:8200"
    )));
}
