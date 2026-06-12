use std::env;
use std::ffi::OsStr;

use super::*;

#[test]
fn rustup_target_list_match_is_exact() {
    let installed = "wasm32-unknown-unknown\nwasm32-wasip1\n";

    assert!(target_list_contains(installed, "wasm32-wasip1"));
    assert!(!target_list_contains(installed, "wasm32-wasi"));
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
