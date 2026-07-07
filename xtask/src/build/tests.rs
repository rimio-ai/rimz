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
fn install_destination_is_home_cargo_bin() {
    let dir = home_cargo_bin_dir_from(PathBuf::from("/home/me"));

    assert_eq!(dir, PathBuf::from("/home/me/.cargo/bin"));
}

#[test]
fn dev_install_builds_profiling_with_the_sentry_feature() {
    let args = host_build_args(HostProfile::Profiling, &["sentry"]);

    assert!(
        args.windows(2)
            .any(|pair| pair[0] == "--profile" && pair[1] == "profiling"),
        "dev install must use the profiling profile: {args:?}"
    );
    let features = args
        .windows(2)
        .find(|pair| pair[0] == "--features")
        .map(|pair| pair[1].as_str());
    assert_eq!(features, Some("sentry"));
}

#[test]
fn dev_install_tells_build_script_it_is_profiling() {
    let envs = host_build_envs(
        Path::new("/workspace"),
        HostProfile::Profiling,
        Some(PROFILING_RUSTFLAGS),
    );

    assert_eq!(
        env_value(&envs, BUILD_PROFILE_OVERRIDE_ENV),
        Some(Path::new("profiling"))
    );
    assert_eq!(
        env_value(&envs, "RUSTFLAGS"),
        Some(Path::new(PROFILING_RUSTFLAGS))
    );
}

#[test]
fn release_install_tells_build_script_it_is_release() {
    let envs = host_build_envs(Path::new("/workspace"), HostProfile::Release, None);

    assert_eq!(
        env_value(&envs, BUILD_PROFILE_OVERRIDE_ENV),
        Some(Path::new("release"))
    );
    assert_eq!(env_value(&envs, "RUSTFLAGS"), None);
}

fn env_value<'a>(envs: &'a [(&str, PathBuf)], key: &str) -> Option<&'a Path> {
    envs.iter()
        .find_map(|(env_key, value)| (*env_key == key).then_some(value.as_path()))
}

#[test]
fn release_install_adds_no_extra_features() {
    let args = host_build_args(HostProfile::Release, &[]);

    assert!(args.iter().any(|arg| arg == "--release"));
    assert!(!args.iter().any(|arg| arg == "--features"));
    assert!(
        !args
            .iter()
            .any(|arg| arg == r#"profile.dev.split-debuginfo="off""#)
    );
}

#[test]
fn dev_install_uses_manifest_profile_debug_info() {
    let args = host_build_args(HostProfile::Profiling, &["sentry"]);

    assert!(
        !args.iter().any(|arg| arg == "--config"),
        "profiling profile carries debug-info settings in Cargo.toml: {args:?}"
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
