use super::*;

#[cfg(unix)]
use super::super::pane_topology::TopologyWriter;
use super::super::tests::support::presence_opts;
#[cfg(unix)]
use super::super::tests::support::{
    current_writer, failing_roster_shim, logging_shim, pane_roster_shim, shim_log,
};

/// A roster covering every discrimination [`is_presence_plugin_pane`] must make:
/// the `file:` URL title Zellij gives a pipe-launched plugin, a stale
/// differently-titled instance, a foreign plugin, and a *terminal* pane whose
/// title merely looks like the plugin's.
#[cfg(unix)]
const MIXED_PLUGIN_ROSTER: &str = r#"[{"id":2,"is_plugin":true,"title":"file:/tmp/rimz-presence-zellij.wasm"},{"id":3,"is_plugin":true,"title":"rimz-presence-zellij stale"},{"id":4,"is_plugin":true,"title":"status-bar"},{"id":5,"is_plugin":false,"title":"rimz-presence-zellij"}]"#;

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

/// Drive one convergence against [`MIXED_PLUGIN_ROSTER`], optionally seeding the
/// topology cache with `writer` as the replacement's proof, and return the argv
/// log.
#[cfg(unix)]
fn presence_convergence_log(writer: Option<TopologyWriter>) -> String {
    use crate::mux::zellij::pane_topology::PaneTopologyCache;
    use crate::sidebar::cache::write_pane_topology_cache;
    use crate::store::RuntimePaths;

    let (temp, shim) = pane_roster_shim(MIXED_PLUGIN_ROSTER);
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

    shim_log(&temp)
}

#[test]
fn embedded_presence_plugin_is_present() {
    assert!(!EMBEDDED_PRESENCE_PLUGIN.is_empty());
    assert!(EMBEDDED_PRESENCE_PLUGIN.starts_with(b"\0asm"));
}

#[cfg(unix)]
#[test]
fn live_presence_plugin_ids_list_only_matching_plugin_panes() {
    // A duplicate id and an out-of-order id prove the sort and dedup; the
    // foreign plugin and the lookalike terminal pane prove the filter.
    let (_temp, shim) = pane_roster_shim(
        r#"[{"id":9,"is_plugin":true,"title":"foreign-plugin"},{"id":3,"is_plugin":true,"title":"file:/tmp/rimz-presence-zellij.wasm"},{"id":2,"is_plugin":true,"title":"rimz-presence-zellij stale"},{"id":4,"is_plugin":false,"title":"rimz-presence-zellij"},{"id":3,"is_plugin":true,"title":"rimz-presence-zellij"}]"#,
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
fn current_presence_cleanup_preserves_a_single_accepted_writer() {
    let (temp, shim) = pane_roster_shim(
        r#"[{"id":2,"is_plugin":true,"title":"file:/tmp/rimz-presence-zellij.wasm"}]"#,
    );
    let backend = ZellijBackend::with_program_for_test(&shim);
    let opts = presence_opts("rimz-test", "/home/user/.cargo/bin/rimz");

    assert_eq!(
        backend
            .cleanup_current_presence_plugin_for(&opts, &current_writer(2, u64::MAX))
            .expect("inspect current presence plugin"),
        PresencePluginCleanup::Current,
    );

    let log = shim_log(&temp);
    assert_eq!(log.matches("action list-panes --all --json").count(), 1);
    for mutation in [
        "--name rimz:retire",
        "action close-pane",
        "--name rimz_presence_boot",
    ] {
        assert!(
            !log.contains(mutation),
            "a singleton accepted writer must stay untouched:\n{log}",
        );
    }
}

#[cfg(unix)]
#[test]
fn current_presence_cleanup_retires_a_stale_loaded_id() {
    let (temp, shim) = pane_roster_shim(
        r#"[{"id":2,"is_plugin":true,"title":"file:/tmp/rimz-presence-zellij.wasm"},{"id":3,"is_plugin":true,"title":"rimz-presence-zellij stale"},{"id":4,"is_plugin":true,"title":"status-bar"}]"#,
    );
    let backend = ZellijBackend::with_program_for_test(&shim);
    let opts = presence_opts("rimz-test", "/home/user/.cargo/bin/rimz");

    assert_eq!(
        backend
            .cleanup_current_presence_plugin_for(&opts, &current_writer(2, u64::MAX))
            .expect("clean up stale presence plugin"),
        PresencePluginCleanup::Reconciled,
    );

    let log = shim_log(&temp);
    assert!(
        log.contains("--name rimz:retire -- {\"plugin_id\":2"),
        "cleanup should broadcast the accepted writer identity:\n{log}",
    );
    assert!(
        log.contains("action close-pane --pane-id plugin_3"),
        "cleanup should force-close the stale loaded id:\n{log}",
    );
    for untouched in ["plugin_2", "plugin_4"] {
        assert!(
            !log.contains(&format!("close-pane --pane-id {untouched}")),
            "cleanup must preserve the accepted writer and unrelated plugins:\n{log}",
        );
    }
    assert!(
        log.contains("--name rimz_presence_boot -- load"),
        "cleanup should heal any accepted writer closed with a same-id clone:\n{log}",
    );
}

#[cfg(unix)]
#[test]
fn share_web_session_pipes_share_payload_to_presence_plugin() {
    let (temp, shim) = logging_shim();
    let backend = ZellijBackend::with_program_for_test(&shim);
    let opts = presence_opts("rimz-test", "/home/user/.cargo/bin/rimz");

    backend
        .share_web_session_for(&opts)
        .expect("share session pipe");

    let log = shim_log(&temp);
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
fn topology_dumps_broadcast_without_launching_plugins() {
    let (temp, shim) = logging_shim();
    let backend = ZellijBackend::with_program_for_test(&shim);
    let opts = presence_opts("rimz-test", "/home/user/.cargo/bin/rimz");

    backend.dump_topology_for(&opts).expect("first dump");
    backend.dump_topology_for(&opts).expect("second dump");

    let log = shim_log(&temp);
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

    let (temp, shim) = logging_shim();
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

/// Retire is gated on a *proven* replacement: a writer whose generation is at or
/// past the convergence floor and whose build and config identities match this
/// host. Each row fails exactly one leg of that proof.
#[cfg(unix)]
#[test]
fn presence_convergence_retires_only_on_a_proven_replacement_writer() {
    let mut wrong_build = current_writer(2, u64::MAX);
    wrong_build.build = Some("old-build".to_owned());

    for (unproven, writer) in [
        ("no replacement topology", None),
        (
            "a writer loaded before the convergence floor",
            Some(current_writer(1, 0)),
        ),
        ("a writer from another build", Some(wrong_build)),
    ] {
        let log = presence_convergence_log(writer);

        assert!(
            log.contains("--name rimz_presence_boot -- load"),
            "{unproven}: convergence should still boot the replacement:\n{log}",
        );
        assert!(
            !log.contains("--name rimz:retire"),
            "{unproven} must not retire the old plugin:\n{log}",
        );
    }
}

#[cfg(unix)]
#[test]
fn presence_force_sweep_listing_failure_keeps_retire_best_effort() {
    use crate::mux::zellij::pane_topology::PaneTopologyCache;
    use crate::sidebar::cache::write_pane_topology_cache;
    use crate::store::RuntimePaths;

    let (temp, shim) = failing_roster_shim();
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
    let log = shim_log(&temp);
    assert!(log.contains("--name rimz:retire"), "{log}");
    assert!(log.contains("action list-panes --all --json"), "{log}");
    assert!(!log.contains("close-pane"), "{log}");
    assert!(log.contains("--name rimz_presence_boot -- load"), "{log}");
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
