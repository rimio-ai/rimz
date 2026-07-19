use super::super::MIN_ZELLIJ_VERSION;
use super::*;

fn presence_opts(session_name: &str, rimz_bin: &str) -> crate::mux::PresencePluginOptions {
    crate::mux::PresencePluginOptions {
        session_name: session_name.to_owned(),
        workspace_id: crate::ids::WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
        wasm: std::path::PathBuf::from("/tmp/rimz-presence-zellij.wasm"),
        rimz_bin: std::path::PathBuf::from(rimz_bin),
        converge: false,
        seed_permissions: false,
        focus_key: None,
        focus_follows_mouse: false,
        mouse_click_through: true,
    }
}

fn permission_children(document: &KdlDocument, key: &str) -> Vec<String> {
    document
        .get(key)
        .expect("permission node exists")
        .children()
        .expect("permission node has children")
        .nodes()
        .iter()
        .map(|node| node.name().value().to_owned())
        .collect()
}

#[test]
fn seed_presence_permissions_adds_node_to_empty_document() {
    let key = "/tmp/rimz-presence-zellij.wasm";
    let mut document = KdlDocument::new();

    assert!(ensure_presence_permissions_document(
        &mut document,
        key,
        false
    ));
    document.fmt();
    let rendered = document.to_string();
    rendered
        .parse::<KdlDocument>()
        .expect("seeded KDL round-trips");
    assert_eq!(
        permission_children(&document, key),
        PRESENCE_PLUGIN_BASE_PERMISSIONS
    );
    assert!(
        rendered.starts_with("\"/tmp/rimz-presence-zellij.wasm\""),
        "path cache key is quoted as a KDL node name: {rendered}"
    );
}

#[test]
fn seed_presence_permissions_merges_partial_node_and_preserves_foreign_nodes() {
    let key = "/tmp/rimz-presence-zellij.wasm";
    let mut document: KdlDocument = r#""/other-plugin.wasm" {
    RunCommands
}
"/tmp/rimz-presence-zellij.wasm" {
    ReadApplicationState
    RunCommands
    Reconfigure
}
"#
    .parse()
    .expect("parse starting permissions");

    assert!(ensure_presence_permissions_document(
        &mut document,
        key,
        true
    ));
    document.fmt();
    document
        .to_string()
        .parse::<KdlDocument>()
        .expect("merged KDL round-trips");
    assert_eq!(
        permission_children(&document, "/other-plugin.wasm"),
        ["RunCommands"]
    );
    assert_eq!(
        permission_children(&document, key),
        [
            "ReadApplicationState",
            "RunCommands",
            "Reconfigure",
            "StartWebServer"
        ]
    );
}

#[test]
fn seed_presence_permissions_is_noop_when_complete() {
    let key = "/tmp/rimz-presence-zellij.wasm";
    let mut document = KdlDocument::new();
    assert!(ensure_presence_permissions_document(
        &mut document,
        key,
        true
    ));
    document.fmt();
    let once = document.to_string();

    assert!(!ensure_presence_permissions_document(
        &mut document,
        key,
        true
    ));
    document.fmt();
    assert_eq!(document.to_string(), once);
}

#[cfg(unix)]
fn zellij_shim(script: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::TempDir::new().expect("tempdir");
    let shim = temp.path().join("zellij");
    let mut file = std::fs::File::create(&shim).expect("create shim");
    file.write_all(script.as_bytes()).expect("write shim");
    let mut perms = file.metadata().expect("shim metadata").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&shim, perms).expect("chmod shim");
    drop(file);
    (temp, shim)
}

#[cfg(unix)]
fn presence_convergence_log(
    writer: Option<crate::mux::zellij::pane_topology::TopologyWriter>,
) -> String {
    use crate::mux::zellij::pane_topology::PaneTopologyCache;
    use crate::sidebar::cache::write_pane_topology_cache;
    use crate::store::RuntimePaths;

    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/zellij.log"
case " $* " in
  *" action list-panes --all --json "*)
    printf '[{"id":2,"is_plugin":true,"title":"file:/tmp/rimz-presence-zellij.wasm"},{"id":3,"is_plugin":true,"title":"rimz-presence-zellij stale"},{"id":4,"is_plugin":true,"title":"status-bar"},{"id":5,"is_plugin":false,"title":"rimz-presence-zellij"}]\n'
    exit 0 ;;
esac
exit 0
"#,
    );
    let backend = ZellijBackend::with_program_and_runtime_for_test(&shim, temp.path());
    let mut opts = presence_opts("rimz-test", "/home/user/.cargo/bin/rimz");
    opts.converge = true;
    if let Some(writer) = writer {
        let runtime =
            RuntimePaths::under(opts.workspace_id.clone(), temp.path()).expect("runtime paths");
        runtime.ensure_dirs().expect("runtime dirs");
        write_pane_topology_cache(
            &runtime,
            &PaneTopologyCache {
                session_name: opts.session_name.clone(),
                produced_at_ms: u64::MAX,
                writer: Some(writer),
                focused_pane: None,
                clients: None,
                panes: Vec::new(),
            },
        )
        .expect("write topology cache");
    }

    backend
        .converge_presence_plugin_for_with(&opts, Duration::ZERO, Duration::ZERO)
        .expect("converge presence plugin");

    std::fs::read_to_string(temp.path().join("zellij.log")).expect("read log")
}

fn current_writer(
    plugin_id: u32,
    loaded_at_ms: u64,
) -> crate::mux::zellij::pane_topology::TopologyWriter {
    let opts = presence_opts("rimz-test", "/home/user/.cargo/bin/rimz");
    let configuration = presence_plugin_configuration(&opts);
    crate::mux::zellij::pane_topology::TopologyWriter {
        plugin_id,
        loaded_at_ms,
        build: Some(presence_plugin_build().to_owned()),
        config: presence_plugin_config_hash(&configuration).map(str::to_owned),
    }
}

#[test]
fn embedded_presence_plugin_is_present() {
    assert!(!EMBEDDED_PRESENCE_PLUGIN.is_empty());
    assert!(EMBEDDED_PRESENCE_PLUGIN.starts_with(b"\0asm"));
}

#[cfg(unix)]
#[test]
fn live_presence_plugin_ids_list_only_matching_plugin_panes() {
    let (_temp, shim) = zellij_shim(
        r#"#!/bin/sh
case " $* " in
  *" action list-panes --all --json "*)
    printf '[{"id":9,"is_plugin":true,"title":"foreign-plugin"},{"id":3,"is_plugin":true,"title":"file:/tmp/rimz-presence-zellij.wasm"},{"id":2,"is_plugin":true,"title":"rimz-presence-zellij stale"},{"id":4,"is_plugin":false,"title":"rimz-presence-zellij"},{"id":3,"is_plugin":true,"title":"rimz-presence-zellij"}]\n'
    exit 0 ;;
esac
exit 1
"#,
    );
    let backend = ZellijBackend::with_program_for_test(&shim);

    assert_eq!(
        backend
            .live_presence_plugin_ids("rimz-test")
            .expect("list live presence plugins"),
        vec![2, 3]
    );
}

#[cfg(unix)]
#[test]
fn share_web_session_pipes_share_payload_to_presence_plugin() {
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/zellij.log"
if [ "$1" = "--version" ]; then
  printf 'zellij 0.44.3\n'
fi
"#,
    );
    let backend = ZellijBackend::with_program_for_test(&shim);
    let opts = presence_opts("rimz-test", "/home/user/.cargo/bin/rimz");

    backend
        .share_web_session_for(&opts)
        .expect("share session pipe");

    let log = std::fs::read_to_string(temp.path().join("zellij.log")).expect("read log");
    assert!(
        log.contains("--session rimz-test pipe --plugin file:/tmp/rimz-presence-zellij.wasm"),
        "share should target the presence plugin by session and wasm URL:\n{log}",
    );
    assert!(
        log.contains("--name rimz_presence_boot -- load"),
        "share should first load and grant the presence plugin:\n{log}",
    );
    assert!(
        log.contains("--name rimz:share_session -- share"),
        "share should send the runtime web-sharing pipe and payload:\n{log}",
    );
}

#[cfg(unix)]
#[test]
fn presence_convergence_skips_retire_without_replacement_topology() {
    let log = presence_convergence_log(None);

    assert!(
        log.contains("--name rimz_presence_boot -- load"),
        "convergence should boot the replacement:\n{log}",
    );
    assert!(
        !log.contains("--name rimz:retire"),
        "an unproven replacement must not retire the old plugin:\n{log}",
    );
}

#[cfg(unix)]
#[test]
fn topology_dumps_broadcast_without_launching_plugins() {
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/zellij.log"
exit 0
"#,
    );
    let backend = ZellijBackend::with_program_for_test(&shim);
    let opts = presence_opts("rimz-test", "/home/user/.cargo/bin/rimz");

    backend.dump_topology_for(&opts).expect("first dump");
    backend.dump_topology_for(&opts).expect("second dump");

    let log = std::fs::read_to_string(temp.path().join("zellij.log")).expect("read log");
    assert_eq!(log.matches("--name rimz:dump_topology -- dump").count(), 2);
    assert!(
        !log.contains("--plugin"),
        "generic topology reads must not launch a build-specific plugin:\n{log}",
    );
}

#[cfg(unix)]
#[test]
fn owner_launch_records_the_desired_writer_identity() {
    use crate::sidebar::cache::read_presence_desired;
    use crate::store::RuntimePaths;

    let (temp, shim) = zellij_shim("#!/bin/sh\nexit 0\n");
    let backend = ZellijBackend::with_program_and_runtime_for_test(&shim, temp.path());
    let opts = presence_opts("rimz-test", "/home/user/.cargo/bin/rimz");
    let runtime = RuntimePaths::under(opts.workspace_id.clone(), temp.path()).unwrap();
    runtime.ensure_dirs().unwrap();

    backend
        .ensure_presence_plugin_for(&opts)
        .expect("ensure presence plugin");

    let desired = read_presence_desired(&runtime).expect("desired writer record");
    let configuration = presence_plugin_configuration(&opts);
    assert_eq!(desired.build, presence_plugin_build());
    assert_eq!(
        Some(desired.config.as_str()),
        presence_plugin_config_hash(&configuration)
    );
    assert!(desired.recorded_at_ms > 0);
}

#[cfg(unix)]
#[test]
fn presence_convergence_retires_after_replacement_writer_is_proven() {
    let log = presence_convergence_log(Some(current_writer(2, u64::MAX)));

    assert!(
        log.contains("--name rimz:dump_topology -- dump"),
        "convergence should request replacement topology:\n{log}",
    );
    assert!(log.contains("--name rimz:retire -- {\"plugin_id\":2,\"loaded_at_ms\":18446744073709551615,\"build\":"), "a proven replacement should retire stale plugins:\n{log}");
    assert!(
        log.contains("action close-pane --pane-id plugin_3"),
        "the host sweep should close a stale presence-plugin id:\n{log}",
    );
    for untouched in ["plugin_2", "plugin_4", "plugin_5"] {
        assert!(
            !log.contains(&format!("close-pane --pane-id {untouched}")),
            "the host sweep must preserve the accepted writer, unrelated plugins, and terminals:\n{log}",
        );
    }
    assert_eq!(
        log.matches("--name rimz_presence_boot -- load").count(),
        2,
        "retire convergence should heal with a post-retire boot pipe:\n{log}",
    );
}

#[cfg(unix)]
#[test]
fn presence_force_sweep_listing_failure_keeps_retire_best_effort() {
    use crate::mux::zellij::pane_topology::PaneTopologyCache;
    use crate::sidebar::cache::write_pane_topology_cache;
    use crate::store::RuntimePaths;

    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/zellij.log"
case " $* " in
  *" action list-panes --all --json "*) exit 1 ;;
esac
exit 0
"#,
    );
    let backend = ZellijBackend::with_program_and_runtime_for_test(&shim, temp.path());
    let opts = presence_opts("rimz-test", "/home/user/.cargo/bin/rimz");
    let runtime = RuntimePaths::under(opts.workspace_id.clone(), temp.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    write_pane_topology_cache(
        &runtime,
        &PaneTopologyCache {
            session_name: opts.session_name.clone(),
            produced_at_ms: u64::MAX,
            writer: Some(current_writer(2, u64::MAX)),
            focused_pane: None,
            clients: None,
            panes: Vec::new(),
        },
    )
    .unwrap();

    backend.retire_proven_presence_plugin_for(&opts, 0, Duration::ZERO, Duration::ZERO);
    let log = std::fs::read_to_string(temp.path().join("zellij.log")).expect("read log");
    assert!(log.contains("--name rimz:retire"), "{log}");
    assert!(log.contains("action list-panes --all --json"), "{log}");
    assert!(!log.contains("close-pane"), "{log}");
    assert!(log.contains("--name rimz_presence_boot -- load"), "{log}");
}

#[cfg(unix)]
#[test]
fn presence_convergence_rejects_old_writer_generation() {
    let log = presence_convergence_log(Some(current_writer(1, 0)));

    assert!(
        !log.contains("--name rimz:retire"),
        "fresh topology from an old writer must not prove the replacement:\n{log}",
    );
}

#[cfg(unix)]
#[test]
fn presence_convergence_rejects_a_wrong_writer_identity() {
    let mut writer = current_writer(2, u64::MAX);
    writer.build = Some("old-build".to_owned());

    let log = presence_convergence_log(Some(writer));

    assert!(
        !log.contains("--name rimz:retire"),
        "a later writer from another build must not prove the replacement:\n{log}",
    );
}

#[test]
fn presence_plugin_floor_is_the_zellij_floor() {
    assert_eq!(MIN_ZELLIJ_VERSION, (0, 44, 0));
    assert!((0, 44, 3) >= MIN_ZELLIJ_VERSION);
    assert!((0, 43, 9) < MIN_ZELLIJ_VERSION);
}
#[test]
fn presence_plugin_configuration_renders_expressible_fields() {
    type PresenceOpts = crate::mux::PresencePluginOptions;
    type MutatePresence = fn(&mut PresenceOpts);
    struct Case {
        session: &'static str,
        rimz_bin: &'static str,
        mutate: MutatePresence,
        expected: &'static str,
    }

    let cases = [
        Case {
            session: "rimz-test",
            rimz_bin: "/state/rimz/workspaces/ws_0123456789abcdef01234567/rimz",
            mutate: |_| {},
            expected: "workspace_id=ws_0123456789abcdef01234567,session_name=rimz-test,rimz_bin=/state/rimz/workspaces/ws_0123456789abcdef01234567/rimz,focus_follows_mouse=false,mouse_click_through=true",
        },
        Case {
            session: "rimz-test",
            rimz_bin: "/home/user/.cargo/bin/rimz",
            mutate: |opts| {
                opts.focus_follows_mouse = true;
                opts.mouse_click_through = false;
            },
            expected: "workspace_id=ws_0123456789abcdef01234567,session_name=rimz-test,rimz_bin=/home/user/.cargo/bin/rimz,focus_follows_mouse=true,mouse_click_through=false",
        },
        Case {
            session: "rimz-test",
            rimz_bin: "/tmp/a,b/rimz",
            mutate: |_| {},
            expected: "workspace_id=ws_0123456789abcdef01234567,session_name=rimz-test,focus_follows_mouse=false,mouse_click_through=true",
        },
        Case {
            session: "rimz-test",
            rimz_bin: "/tmp/a=b/rimz",
            mutate: |_| {},
            expected: "workspace_id=ws_0123456789abcdef01234567,session_name=rimz-test,focus_follows_mouse=false,mouse_click_through=true",
        },
        Case {
            session: "rimz,test",
            rimz_bin: "/home/user/.cargo/bin/rimz",
            mutate: |_| {},
            expected: "workspace_id=ws_0123456789abcdef01234567,rimz_bin=/home/user/.cargo/bin/rimz,focus_follows_mouse=false,mouse_click_through=true",
        },
        Case {
            session: "rimz=test",
            rimz_bin: "/home/user/.cargo/bin/rimz",
            mutate: |_| {},
            expected: "workspace_id=ws_0123456789abcdef01234567,rimz_bin=/home/user/.cargo/bin/rimz,focus_follows_mouse=false,mouse_click_through=true",
        },
        Case {
            session: "rimz-test",
            rimz_bin: "/home/user/.cargo/bin/rimz",
            mutate: |opts| opts.focus_key = Some("Alt+p".to_owned()),
            expected: "workspace_id=ws_0123456789abcdef01234567,session_name=rimz-test,rimz_bin=/home/user/.cargo/bin/rimz,focus_follows_mouse=false,mouse_click_through=true,focus_key=Alt+p",
        },
        Case {
            session: "rimz-test",
            rimz_bin: "/home/user/.cargo/bin/rimz",
            mutate: |opts| opts.focus_key = Some("Alt=p".to_owned()),
            expected: "workspace_id=ws_0123456789abcdef01234567,session_name=rimz-test,rimz_bin=/home/user/.cargo/bin/rimz,focus_follows_mouse=false,mouse_click_through=true",
        },
    ];

    for case in cases {
        let mut opts = presence_opts(case.session, case.rimz_bin);
        (case.mutate)(&mut opts);
        let without_config = format!("{},plugin_build={}", case.expected, presence_plugin_build());
        let expected = format!(
            "{without_config},plugin_config={}",
            crate::build_id::of_bytes(without_config.as_bytes())
        );
        assert_eq!(presence_plugin_configuration(&opts), expected);
    }
}

#[test]
fn materialize_presence_plugin_bytes_writes_stable_artifact_or_nothing() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        materialize_presence_plugin_bytes(b"", dir.path())
            .unwrap()
            .is_none()
    );

    let path = materialize_presence_plugin_bytes(b"wasm-bytes", dir.path())
        .unwrap()
        .unwrap();
    assert!(path.ends_with("rimz/plugins/rimz-presence-zellij.wasm"));
    assert_eq!(std::fs::read(&path).unwrap(), b"wasm-bytes");

    let same_path = materialize_presence_plugin_bytes(b"wasm-bytes", dir.path())
        .unwrap()
        .unwrap();
    assert_eq!(same_path, path);

    materialize_presence_plugin_bytes(b"new-bytes", dir.path()).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"new-bytes");
}
