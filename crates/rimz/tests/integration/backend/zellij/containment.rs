#[cfg(target_os = "linux")]
use rimz::testkit::sandbox::SandboxSpec;

use crate::common::{CommandTimeoutExt, ZellijNamespace};

#[test]
fn namespace_drop_stops_an_unguarded_real_zellij_server() {
    require_zellij!();
    let namespace = ZellijNamespace::new();
    let root = namespace.path().to_path_buf();
    #[cfg(target_os = "linux")]
    let spec = SandboxSpec {
        home_root: root.clone(),
        runtime_root: root.clone(),
    };
    let output = namespace
        .command()
        .args(["attach", "--create-background", "containment"])
        .bounded_output()
        .expect("start unguarded Zellij server");
    assert!(
        output.status.success(),
        "start Zellij server: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    #[cfg(target_os = "linux")]
    assert!(
        !rimz::testkit::sandbox::sandbox_processes(&spec).is_empty(),
        "the Zellij server carries its namespace marker"
    );

    drop(namespace);

    assert!(!root.exists(), "Zellij namespace root removed");
    #[cfg(target_os = "linux")]
    assert!(
        rimz::testkit::sandbox::sandbox_processes(&spec).is_empty(),
        "no Zellij process retains the namespace marker"
    );
}
