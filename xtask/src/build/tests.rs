use std::env;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use super::*;

#[test]
fn rustup_target_list_match_is_exact() {
    let installed = "wasm32-unknown-unknown\nwasm32-wasip1\n";

    assert!(target_list_contains(installed, "wasm32-wasip1"));
    assert!(!target_list_contains(installed, "wasm32-wasi"));
}

#[test]
fn version_line_keeps_only_the_version_token() {
    assert_eq!(
        parse_version_line("rimz 0.0.0+gabc123def456\n"),
        "0.0.0+gabc123def456"
    );
    assert_eq!(parse_version_line("rimz 1.2.3\n"), "1.2.3");
    assert_eq!(parse_version_line("0.0.0\n"), "0.0.0");
}

#[test]
fn relative_install_paths_report_as_absolute() {
    let path = absolute_lexical_path(Path::new("target/xtask-install/bin/rimz")).unwrap();

    assert!(path.is_absolute());
    assert!(path.ends_with("target/xtask-install/bin/rimz"));
}

#[test]
fn install_destinations_include_cargo_bin_and_usr_local_bin() {
    let dirs = install_bin_dirs_from(PathBuf::from("/home/me/.cargo/bin"));

    assert_eq!(
        dirs,
        vec![
            PathBuf::from("/home/me/.cargo/bin"),
            PathBuf::from("/usr/local/bin")
        ]
    );
}

#[test]
fn install_destinations_do_not_duplicate_usr_local_bin() {
    let dirs = install_bin_dirs_from(PathBuf::from("/usr/local/bin"));

    assert_eq!(dirs, vec![PathBuf::from("/usr/local/bin")]);
}

#[test]
fn dev_install_builds_debug_with_the_sentry_feature() {
    let args = host_build_args(false, &["sentry"], &[]);

    assert!(
        !args.iter().any(|arg| arg == "--release"),
        "dev install must stay a debug build so reporting defaults to development: {args:?}"
    );
    let features = args
        .windows(2)
        .find(|pair| pair[0] == "--features")
        .map(|pair| pair[1].as_str());
    assert_eq!(features, Some("sentry"));
}

#[test]
fn release_install_adds_no_extra_features() {
    let args = host_build_args(true, &[], &[]);

    assert!(args.iter().any(|arg| arg == "--release"));
    assert!(!args.iter().any(|arg| arg == "--features"));
    assert!(
        !args
            .iter()
            .any(|arg| arg == r#"profile.dev.split-debuginfo="off""#)
    );
}

#[test]
fn dev_install_can_embed_debug_info_for_sentry_upload() {
    let args = host_build_args(
        false,
        &["sentry"],
        &["--config", r#"profile.dev.split-debuginfo="off""#],
    );

    assert!(args.iter().any(|arg| arg == "--config"));
    assert!(
        args.iter()
            .any(|arg| arg == r#"profile.dev.split-debuginfo="off""#)
    );
}

#[test]
fn profile_build_uses_custom_profile_and_rustflags() {
    let args = profiling_build_args();

    assert!(!args.iter().any(|arg| arg == "--release"));
    assert!(
        args.windows(2)
            .any(|pair| pair[0] == "--profile" && pair[1] == "profiling")
    );
    assert!(PROFILING_RUSTFLAGS.contains("force-frame-pointers=yes"));
    assert!(PROFILING_RUSTFLAGS.contains("symbol-mangling-version=v0"));
}

#[test]
fn macos_sdkroot_acceptance_matches_rustc_shape() {
    let cwd = env::current_dir().unwrap();

    assert!(rustc_accepts_macos_sdkroot(cwd.as_os_str()));
    assert!(!rustc_accepts_macos_sdkroot(OsStr::new("/")));
    assert!(!rustc_accepts_macos_sdkroot(OsStr::new("relative-sdk")));
    assert!(!rustc_accepts_macos_sdkroot(OsStr::new(
        "/definitely/missing/MacOSX.sdk"
    )));
}

#[test]
fn macos_sdkroot_rejects_other_apple_platforms() {
    assert!(macos_sdkroot_points_at_other_apple_platform(OsStr::new(
        "/Xcode/Platforms/iPhoneOS.platform/Developer/SDKs/iPhoneOS.sdk"
    )));
    assert!(!macos_sdkroot_points_at_other_apple_platform(OsStr::new(
        "/Xcode/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk"
    )));
}
