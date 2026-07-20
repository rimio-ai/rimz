use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::{env, fs};

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
fn install_rebuilds_after_a_checkout_move_hides_the_mixed_build_failure() {
    let mut revisions = std::collections::VecDeque::from([
        Some("old".to_owned()),
        Some("new".to_owned()),
        Some("new".to_owned()),
        Some("new".to_owned()),
    ]);
    let mut attempts = 0;

    let artifact = build_at_stable_checkout(
        || revisions.pop_front().expect("revision probe"),
        || {
            attempts += 1;
            if attempts == 1 {
                Err(anyhow::anyhow!("binary saw the new library shape"))
            } else {
                Ok("stable artifact")
            }
        },
    )
    .unwrap();

    assert_eq!(artifact, "stable artifact");
    assert_eq!(attempts, 2);
}

#[test]
fn install_keeps_a_real_build_failure_from_a_stable_checkout() {
    let mut attempts = 0;

    let err = build_at_stable_checkout(
        || Some("stable".to_owned()),
        || {
            attempts += 1;
            Err::<(), _>(anyhow::anyhow!("real compiler error"))
        },
    )
    .unwrap_err();

    assert_eq!(err.to_string(), "real compiler error");
    assert_eq!(attempts, 1);
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

#[test]
fn darwin_zigbuild_sdkroot_carries_framework_stubs() {
    let sdkroot = temp_path("darwin-zigbuild-sdkroot");

    prepare_darwin_zigbuild_sdkroot(&sdkroot).unwrap();

    assert!(sdkroot.join("usr").join("lib").is_dir());
    let core_foundation = fs::read_to_string(
        sdkroot
            .join("System")
            .join("Library")
            .join("Frameworks")
            .join("CoreFoundation.framework")
            .join("CoreFoundation.tbd"),
    )
    .unwrap();
    assert!(core_foundation.contains("CoreFoundation.framework"));
    assert!(core_foundation.contains("_CFDataGetBytes"));
    assert!(core_foundation.contains("_kCFAllocatorDefault"));

    let iokit = fs::read_to_string(
        sdkroot
            .join("System")
            .join("Library")
            .join("Frameworks")
            .join("IOKit.framework")
            .join("IOKit.tbd"),
    )
    .unwrap();
    assert!(iokit.contains("IOKit.framework"));
    assert!(iokit.contains("_IOServiceMatching"));
    assert!(iokit.contains("_kIOMasterPortDefault"));

    let _ = fs::remove_dir_all(sdkroot);
}

fn temp_path(label: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time after epoch")
        .as_nanos();
    env::temp_dir().join(format!("{label}-{}-{unique}", std::process::id()))
}
