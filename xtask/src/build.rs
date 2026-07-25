use std::collections::BTreeMap;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::files::{copy_atomically, remove_stale_file, sha256_file, target_dir, write_atomically};
use crate::pricing::pricing_refresh;
use crate::runner::{run, run_with_env};

const PRESENCE_PLUGIN_TARGET: &str = "wasm32-wasip1";
const DARWIN_TARGETS: [&str; 2] = ["aarch64-apple-darwin", "x86_64-apple-darwin"];
const PROFILING_RUSTFLAGS: &str = "-C force-frame-pointers=yes -C symbol-mangling-version=v0";
const BUILD_PROFILE_OVERRIDE_ENV: &str = "RIMZ_BUILD_PROFILE_OVERRIDE";
const BUILD_VERSION_OVERRIDE_ENV: &str = "RIMZ_BUILD_VERSION_OVERRIDE";
const STABLE_CHECKOUT_BUILD_ATTEMPTS: usize = 3;
pub(crate) const WASM_MAGIC: [u8; 4] = *b"\0asm";
const ENCODED_RUSTFLAGS_SEPARATOR: &str = "\x1f";
const DARWIN_COREFOUNDATION_TBD: &str = r#"--- !tapi-tbd
tbd-version:     4
targets:         [ x86_64-macos, arm64-macos ]
install-name:    '/System/Library/Frameworks/CoreFoundation.framework/Versions/A/CoreFoundation'
current-version: 1
compatibility-version: 1
exports:
  - targets:         [ x86_64-macos, arm64-macos ]
    symbols:         [ _CFDataGetBytes, _CFDataGetLength, _CFDataGetTypeID, _CFGetTypeID,
                       _CFRelease, _CFRetain, _CFStringCreateWithBytesNoCopy,
                       _kCFAllocatorDefault, _kCFAllocatorNull ]
...
"#;
const DARWIN_IOKIT_TBD: &str = r#"--- !tapi-tbd
tbd-version:     4
targets:         [ x86_64-macos, arm64-macos ]
install-name:    '/System/Library/Frameworks/IOKit.framework/Versions/A/IOKit'
current-version: 1
compatibility-version: 1
exports:
  - targets:         [ x86_64-macos, arm64-macos ]
    symbols:         [ _IOIteratorNext, _IOObjectRelease, _IORegistryEntryCreateCFProperty,
                       _IORegistryEntryGetName, _IOServiceGetMatchingServices,
                       _IOServiceMatching, _kIOMasterPortDefault ]
...
"#;

pub(crate) fn build(root: &Path) -> Result<()> {
    build_plugin(root)?;
    let envs = presence_plugin_embed_env(root);
    run_with_env(
        root,
        "cargo",
        ["build", "--workspace", "--all-features", "--locked"],
        &envs,
    )
}

pub(crate) fn dist(root: &Path) -> Result<()> {
    pricing_refresh(root)?;
    build_plugin(root)?;
    let mut artifacts = BTreeMap::new();
    let host_target = rustc_host_target(root)?;
    if !DARWIN_TARGETS.contains(&host_target.as_str()) {
        build_host_release(root)?;
        artifacts.insert(host_target, release_artifact(root, "rimz"));
    }
    build_darwin_artifacts(root)?;
    codesign_arm64_artifact(root)?;
    for target in DARWIN_TARGETS {
        artifacts.insert(
            target.to_owned(),
            target_release_artifact(root, target, "rimz"),
        );
    }
    package_dist_artifacts(root, &artifacts)
}

/// Build the Zellij presence plugin for its real target. The host-target
/// workspace build only compiles the crate's pure policy core and a stub bin;
/// this produces the `.wasm` Zellij actually loads.
pub(crate) fn build_plugin(root: &Path) -> Result<()> {
    ensure_rust_target(root, PRESENCE_PLUGIN_TARGET)?;
    let rustflags = canonical_plugin_rustflags(root)?;
    run_with_env(
        root,
        "cargo",
        [
            "build",
            "-p",
            "rimz-presence-zellij",
            "--target",
            PRESENCE_PLUGIN_TARGET,
            "--release",
            "--locked",
        ],
        &[("CARGO_ENCODED_RUSTFLAGS", PathBuf::from(rustflags))],
    )
}

pub(crate) fn plugin_refresh(root: &Path) -> Result<()> {
    build_plugin(root)?;
    let artifact = plugin_artifact(root);
    let bytes = fs::read(&artifact).with_context(|| format!("reading {}", artifact.display()))?;
    if !is_wasm_module(&bytes) {
        bail!("{} is not a wasm module", artifact.display());
    }
    copy_atomically(&artifact, &vendored_plugin_path(root))?;
    let provenance = PluginProvenance {
        source_sha256: presence_plugin_source_digest(root)?,
        wasm_sha256: sha256_file(&vendored_plugin_path(root))?,
        rustc: rustc_stdout(root, &["--version"])?,
    };
    write_vendored_plugin_provenance(root, &provenance)
}

pub(crate) fn vendored_plugin_path(root: &Path) -> PathBuf {
    root.join("crates")
        .join("rimz")
        .join("presence")
        .join("rimz-presence-zellij.wasm")
}

pub(crate) fn vendored_provenance_path(root: &Path) -> PathBuf {
    let mut path = vendored_plugin_path(root).into_os_string();
    path.push(".provenance.json");
    PathBuf::from(path)
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct PluginProvenance {
    pub(crate) source_sha256: String,
    pub(crate) wasm_sha256: String,
    pub(crate) rustc: String,
}

pub(crate) fn read_vendored_plugin_provenance(root: &Path) -> Result<PluginProvenance> {
    let path = vendored_provenance_path(root);
    let bytes = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))
}

fn write_vendored_plugin_provenance(root: &Path, provenance: &PluginProvenance) -> Result<()> {
    let path = vendored_provenance_path(root);
    let mut bytes =
        serde_json::to_vec_pretty(provenance).context("serializing plugin provenance")?;
    bytes.push(b'\n');
    write_atomically(&path, &bytes)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PluginProvenanceDecision {
    Compare,
    Skip,
    Fail,
}

fn plugin_provenance_decision(
    recorded_rustc: &str,
    current_rustc: &str,
    is_ci: bool,
) -> PluginProvenanceDecision {
    if recorded_rustc == current_rustc {
        PluginProvenanceDecision::Compare
    } else if is_ci {
        PluginProvenanceDecision::Fail
    } else {
        PluginProvenanceDecision::Skip
    }
}

#[expect(
    clippy::print_stderr,
    reason = "a local provenance skip needs a visible contributor diagnostic"
)]
pub(crate) fn verify_vendored_plugin(root: &Path) -> Result<()> {
    let provenance_path = vendored_provenance_path(root);
    let provenance = read_vendored_plugin_provenance(root).with_context(|| {
        format!(
            "{} is missing or invalid; run `cargo xtask plugin-refresh`",
            provenance_path.display()
        )
    })?;
    let current_rustc = rustc_stdout(root, &["--version"])?;
    match plugin_provenance_decision(
        &provenance.rustc,
        &current_rustc,
        env::var_os("CI").is_some(),
    ) {
        PluginProvenanceDecision::Compare => {}
        PluginProvenanceDecision::Skip => {
            eprintln!(
                "plugin-provenance: skipped rebuild comparison because the vendored plugin records `{}` and the current toolchain is `{current_rustc}`",
                provenance.rustc
            );
            return Ok(());
        }
        PluginProvenanceDecision::Fail => {
            bail!(
                "vendored presence plugin provenance records `{}`, but CI uses `{current_rustc}`; run `cargo xtask plugin-refresh` with rustc `{current_rustc}`",
                provenance.rustc
            );
        }
    }

    let artifact = plugin_artifact(root);
    let rebuilt = fs::read(&artifact).with_context(|| format!("reading {}", artifact.display()))?;
    let vendored_path = vendored_plugin_path(root);
    let vendored =
        fs::read(&vendored_path).with_context(|| format!("reading {}", vendored_path.display()))?;
    if rebuilt != vendored {
        bail!(
            "vendored presence plugin does not match a rebuild from source; run `cargo xtask plugin-refresh`"
        );
    }
    Ok(())
}

pub(crate) fn presence_plugin_source_digest(root: &Path) -> Result<String> {
    let plugin_root = root.join("crates").join("rimz-presence-zellij");
    let output = Command::new("git")
        .args(["ls-files"])
        .current_dir(&plugin_root)
        .output()
        .with_context(|| format!("running `git ls-files` in {}", plugin_root.display()))?;
    if !output.status.success() {
        bail!(
            "git ls-files failed in {} with {}: {}",
            plugin_root.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let mut paths: Vec<_> = String::from_utf8(output.stdout)
        .context("reading plugin source file list")?
        .lines()
        .map(str::to_owned)
        .collect();
    paths.sort();

    let mut hasher = Sha256::new();
    for path in paths {
        let bytes = fs::read(plugin_root.join(&path))
            .with_context(|| format!("reading {}", plugin_root.join(&path).display()))?;
        hasher.update(path.as_bytes());
        hasher.update([0]);
        hasher.update(&bytes);
        hasher.update([0]);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub(crate) fn is_wasm_module(bytes: &[u8]) -> bool {
    bytes.starts_with(&WASM_MAGIC)
}

/// The built presence-plugin artifact, honoring a `CARGO_TARGET_DIR` override.
fn plugin_artifact(root: &Path) -> PathBuf {
    target_dir(root)
        .join(PRESENCE_PLUGIN_TARGET)
        .join("release")
        .join("rimz-presence-zellij.wasm")
}

fn canonical_plugin_rustflags(root: &Path) -> Result<OsString> {
    let cargo_home = env::var_os("CARGO_HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("HOME")
                .filter(|path| !path.is_empty())
                .map(|home| PathBuf::from(home).join(".cargo"))
        })
        .context("$CARGO_HOME or $HOME is required to build the presence plugin")?;
    let sysroot = PathBuf::from(rustc_stdout(root, &["--print", "sysroot"])?);
    let mut flags = OsString::new();
    for (source, destination) in [
        (cargo_home.as_path(), "/cargo"),
        (sysroot.as_path(), "/rust-sysroot"),
        (root, "/rimz"),
    ] {
        if !flags.is_empty() {
            flags.push(ENCODED_RUSTFLAGS_SEPARATOR);
        }
        flags.push("--remap-path-prefix=");
        flags.push(source.as_os_str());
        flags.push("=");
        flags.push(destination);
    }
    Ok(flags)
}

fn rustc_stdout(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("rustc")
        .args(args)
        .current_dir(root)
        .output()
        .with_context(|| format!("running `rustc {}`", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "`rustc {}` failed with {}: {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8(output.stdout)
        .with_context(|| format!("reading `rustc {}` output", args.join(" ")))?
        .trim()
        .to_owned())
}

fn ensure_rust_target(root: &Path, target: &str) -> Result<()> {
    if rustup_target_installed(root, target)? || rust_target_std_available(root, target)? {
        return Ok(());
    }

    let status = Command::new("rustup")
        .args(["target", "add", target])
        .current_dir(root)
        .status()
        .with_context(|| {
            format!(
                "Rust target `{target}` is missing and `rustup` could not be run; install it with `rustup target add {target}`"
            )
        })?;
    if !status.success() {
        bail!("Rust target `{target}` is missing and `rustup target add {target}` failed");
    }

    if rustup_target_installed(root, target)? || rust_target_std_available(root, target)? {
        return Ok(());
    }
    bail!("Rust target `{target}` is still unavailable after `rustup target add {target}`");
}

fn rustup_target_installed(root: &Path, target: &str) -> Result<bool> {
    let output = match Command::new("rustup")
        .args(["target", "list", "--installed"])
        .current_dir(root)
        .output()
    {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err).context("checking installed rustup targets"),
    };
    if !output.status.success() {
        return Ok(false);
    }

    let installed =
        String::from_utf8(output.stdout).context("reading installed rustup targets output")?;
    Ok(target_list_contains(&installed, target))
}

fn target_list_contains(installed: &str, target: &str) -> bool {
    installed.lines().any(|line| line.trim() == target)
}

fn rust_target_std_available(root: &Path, target: &str) -> Result<bool> {
    let output = Command::new("rustc")
        .args(["--print", "target-libdir", "--target", target])
        .current_dir(root)
        .output()
        .with_context(|| format!("checking Rust target `{target}`"))?;
    if !output.status.success() {
        return Ok(false);
    }

    let target_libdir =
        String::from_utf8(output.stdout).context("reading rustc target libdir output")?;
    let target_libdir = PathBuf::from(target_libdir.trim());
    let entries = match fs::read_dir(&target_libdir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            return Err(err).with_context(|| format!("reading {}", target_libdir.display()));
        }
    };
    for entry in entries {
        let name = entry
            .with_context(|| format!("reading {}", target_libdir.display()))?
            .file_name();
        if let Some(name) = name.to_str()
            && (name == "libcore.rlib" || (name.starts_with("libcore-") && name.ends_with(".rlib")))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn release_artifact(root: &Path, bin: &str) -> PathBuf {
    profile_artifact(root, "release", bin)
}

/// The built host binary for a cargo profile directory (`release` or `debug`),
/// honoring a `CARGO_TARGET_DIR` override.
fn profile_artifact(root: &Path, profile_dir: &str, bin: &str) -> PathBuf {
    let mut artifact = target_dir(root).join(profile_dir).join(bin);
    if !env::consts::EXE_EXTENSION.is_empty() {
        artifact.set_extension(env::consts::EXE_EXTENSION);
    }
    artifact
}

fn target_release_artifact(root: &Path, target: &str, bin: &str) -> PathBuf {
    target_dir(root).join(target).join("release").join(bin)
}

fn stage_bin_dir(root: &Path) -> PathBuf {
    target_dir(root).join("xtask").join("install").join("bin")
}

fn dist_dir(root: &Path) -> PathBuf {
    target_dir(root).join("dist")
}

/// Where `cargo xtask install` lands binaries.
fn home_cargo_bin_dir() -> Result<PathBuf> {
    let home = env::var_os("HOME").context("$HOME is required to install rimz to ~/.cargo/bin")?;
    Ok(home_cargo_bin_dir_from(PathBuf::from(home)))
}

fn home_cargo_bin_dir_from(home: PathBuf) -> PathBuf {
    home.join(".cargo").join("bin")
}

pub(crate) fn install(root: &Path) -> Result<()> {
    let stage = stage_install(root)?;
    install_from_stage(&stage)
}

/// Build host `rimz` with the dev-only `sentry` feature and install it. The
/// profiling build reports off-box events to the `development` environment by
/// default, so contributor telemetry stays off the production dashboard; opt in
/// by resolving a DSN at runtime. See
/// [off-box error reporting](../../docs/internals/diagnostics.md#off-box-error-reporting).
pub(crate) fn install_dev(root: &Path) -> Result<()> {
    let stage = stage_dev_install(root)?;
    install_from_stage(&stage)?;
    upload_debug_files(&stage.join("rimz"))
}

/// Build an optimized host `rimz` with line-tables debug info, frame pointers,
/// and v0 symbol mangling for perf/samply profiling. The artifact stays under
/// `target/profiling/` and is never installed over the everyday binary.
pub(crate) fn profile_build(root: &Path) -> Result<()> {
    build_plugin(root)?;
    let mut envs = presence_plugin_embed_env(root);
    envs.push(HostProfile::Profiling.build_profile_override_env());
    envs.push(("RUSTFLAGS", PathBuf::from(PROFILING_RUSTFLAGS)));
    run_with_env(root, "cargo", profiling_build_args(), &envs)?;
    report_profile_build(&profile_artifact(root, "profiling", "rimz"));
    Ok(())
}

/// Copy the staged install artifacts into `~/.cargo/bin`, then report the
/// version the way `rimz --version` does.
fn install_from_stage(stage: &Path) -> Result<()> {
    let dest_dir = home_cargo_bin_dir()?;
    fs::create_dir_all(&dest_dir).with_context(|| format!("creating {}", dest_dir.display()))?;
    for artifact in INSTALL_ARTIFACTS {
        copy_atomically(&stage.join(artifact), &dest_dir.join(artifact))
            .with_context(|| format!("installing {artifact} to {}", dest_dir.display()))?;
    }
    let installed = vec![absolute_lexical_path(&dest_dir.join("rimz"))?];
    let version = binary_build_version(&installed[0])?;
    report_install(&version, &installed);
    Ok(())
}

const INSTALL_ARTIFACTS: [&str; 1] = ["rimz"];

fn absolute_lexical_path(path: &Path) -> Result<PathBuf> {
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .context("reading current directory")?
            .join(path)
    })
}

/// The build version embedded in `rimz`, read from the binary itself so the
/// install summary matches what `rimz --version` reports.
fn binary_build_version(bin: &Path) -> Result<String> {
    let output = Command::new(bin)
        .arg("--version")
        .output()
        .with_context(|| format!("running {} --version", bin.display()))?;
    if !output.status.success() {
        bail!("{} --version failed", bin.display());
    }
    let stdout = String::from_utf8(output.stdout).context("reading rimz --version output")?;
    Ok(parse_version_line(&stdout))
}

/// `rimz --version` prints `rimz <version>`; keep just the version token.
fn parse_version_line(line: &str) -> String {
    let line = line.trim();
    line.strip_prefix("rimz ").unwrap_or(line).trim().to_owned()
}

#[expect(
    clippy::print_stdout,
    reason = "install summary is the command's stdout contract"
)]
fn report_install(version: &str, paths: &[PathBuf]) {
    let paths = paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    println!("Installed rimz {version} to {paths}");
}

#[expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "install-dev reports optional Sentry debug-file enrichment"
)]
fn upload_debug_files(binary: &Path) -> Result<()> {
    match Command::new("sentry-cli")
        .args(["debug-files", "upload"])
        .arg(binary)
        .status()
    {
        Ok(status) if status.success() => {
            println!("Uploaded Sentry debug files for {}", binary.display());
        }
        Ok(status) => {
            eprintln!(
                "warning: sentry-cli debug-files upload failed with status {status}; install still succeeded"
            );
        }
        Err(err) => {
            eprintln!(
                "warning: sentry-cli debug-files upload could not start: {err}; install still succeeded"
            );
        }
    }
    Ok(())
}

#[expect(
    clippy::print_stdout,
    reason = "profile-build summary is the command's stdout contract"
)]
fn report_profile_build(artifact: &Path) {
    println!("Built profiling rimz at {}", artifact.display());
}

pub(crate) fn stage_install(root: &Path) -> Result<PathBuf> {
    stage_host_rimz(root, HostProfile::Release, &[], None)
}

/// Stage a dev host `rimz`: an optimized profiling build with the `sentry`
/// feature compiled in.
fn stage_dev_install(root: &Path) -> Result<PathBuf> {
    stage_host_rimz(
        root,
        HostProfile::Profiling,
        &["sentry"],
        Some(PROFILING_RUSTFLAGS),
    )
}

#[derive(Clone, Copy)]
enum HostProfile {
    Release,
    Profiling,
}

impl HostProfile {
    fn cargo_arg(self) -> &'static str {
        match self {
            Self::Release => "--release",
            Self::Profiling => "--profile",
        }
    }

    fn cargo_arg_value(self) -> Option<&'static str> {
        match self {
            Self::Release => None,
            Self::Profiling => Some("profiling"),
        }
    }

    fn target_dir(self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::Profiling => "profiling",
        }
    }

    fn build_profile_override_env(self) -> (&'static str, PathBuf) {
        (BUILD_PROFILE_OVERRIDE_ENV, PathBuf::from(self.target_dir()))
    }
}

/// Build the host `rimz` binary for the given profile and feature set, then copy
/// it into the install staging directory.
fn stage_host_rimz(
    root: &Path,
    profile: HostProfile,
    features: &[&str],
    rustflags: Option<&'static str>,
) -> Result<PathBuf> {
    let profile_dir = profile.target_dir();
    let stage = stage_bin_dir(root);
    build_at_stable_checkout(
        || git_head(root),
        || {
            let envs = host_build_envs(root, profile, rustflags);
            build_plugin(root)?;
            run_with_env(root, "cargo", host_build_args(profile, features), &envs)?;
            fs::create_dir_all(&stage).with_context(|| format!("creating {}", stage.display()))?;
            copy_atomically(
                &profile_artifact(root, profile_dir, "rimz"),
                &stage.join("rimz"),
            )?;
            Ok(stage.clone())
        },
    )
}

fn build_at_stable_checkout<T>(
    mut checkout_revision: impl FnMut() -> Option<String>,
    mut build: impl FnMut() -> Result<T>,
) -> Result<T> {
    for attempt in 1..=STABLE_CHECKOUT_BUILD_ATTEMPTS {
        let before = checkout_revision();
        let result = build();
        let after = checkout_revision();
        if checkout_moved(before.as_deref(), after.as_deref()) {
            report_checkout_moved(before.as_deref(), after.as_deref());
            if attempt == STABLE_CHECKOUT_BUILD_ATTEMPTS {
                bail!(
                    "the checkout changed during {STABLE_CHECKOUT_BUILD_ATTEMPTS} install build attempts; pause merges and rerun the install"
                );
            }
            continue;
        }
        return result;
    }
    bail!("install build attempts exhausted without producing a stable artifact")
}

fn git_head(root: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .current_dir(root)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn checkout_moved(before: Option<&str>, after: Option<&str>) -> bool {
    before != after
}

#[expect(
    clippy::print_stderr,
    reason = "the installer reports why it is rebuilding an otherwise opaque cargo failure"
)]
fn report_checkout_moved(before: Option<&str>, after: Option<&str>) {
    let revision = |revision: Option<&str>| {
        revision
            .map(|revision| revision.chars().take(12).collect::<String>())
            .unwrap_or_else(|| "unavailable".to_owned())
    };
    eprintln!(
        "checkout moved from {} to {} during the install build; rebuilding from one revision",
        revision(before),
        revision(after),
    );
}

fn host_build_envs(
    root: &Path,
    profile: HostProfile,
    rustflags: Option<&'static str>,
) -> Vec<(&'static str, PathBuf)> {
    let mut envs = presence_plugin_embed_env(root);
    envs.push(profile.build_profile_override_env());
    if let Some(rustflags) = rustflags {
        envs.push(("RUSTFLAGS", PathBuf::from(rustflags)));
    }
    envs
}

/// Cargo args to build the host `rimz` binary. `features` opts dev-only cargo
/// features (`sentry`) in.
fn host_build_args(profile: HostProfile, features: &[&str]) -> Vec<String> {
    let mut args = vec![
        "build".to_owned(),
        "-p".to_owned(),
        "rimz".to_owned(),
        "--bin".to_owned(),
        "rimz".to_owned(),
        "--locked".to_owned(),
    ];
    args.push(profile.cargo_arg().to_owned());
    if let Some(value) = profile.cargo_arg_value() {
        args.push(value.to_owned());
    }
    if !features.is_empty() {
        args.push("--features".to_owned());
        args.push(features.join(","));
    }
    args
}

fn profiling_build_args() -> Vec<String> {
    [
        "build",
        "-p",
        "rimz",
        "--bin",
        "rimz",
        "--locked",
        "--profile",
        "profiling",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn presence_plugin_embed_env(root: &Path) -> Vec<(&'static str, PathBuf)> {
    presence_plugin_embed_env_with_version(
        root,
        workspace_build_version(root, env!("CARGO_PKG_VERSION")),
    )
}

fn presence_plugin_embed_env_with_version(
    root: &Path,
    version: Option<String>,
) -> Vec<(&'static str, PathBuf)> {
    let mut envs = vec![("RIMZ_EMBED_PRESENCE_PLUGIN", plugin_artifact(root))];
    if let Some(version) = version {
        envs.push((BUILD_VERSION_OVERRIDE_ENV, PathBuf::from(version)));
    }
    envs
}

/// Resolve the version already embedded by `rimz`'s build script so xtask host
/// builds invalidate on semantic Git state rather than mutable ref/index files.
fn workspace_build_version(root: &Path, package_version: &str) -> Option<String> {
    let exact_release_tag = git_stdout(
        root,
        &[
            "describe",
            "--tags",
            "--exact-match",
            "--match",
            "v[0-9]*",
            "HEAD",
        ],
    )
    .is_some();
    let short_revision = git_stdout(root, &["rev-parse", "--short=12", "HEAD"])?;
    let status = git_stdout(root, &["status", "--porcelain"])?;
    workspace_build_version_from_git(
        package_version,
        exact_release_tag,
        Some(&short_revision),
        Some(&status),
    )
}

fn workspace_build_version_from_git(
    package_version: &str,
    exact_release_tag: bool,
    short_revision: Option<&str>,
    status: Option<&str>,
) -> Option<String> {
    let short_revision = short_revision.filter(|revision| !revision.is_empty())?;
    let dirty = !status?.is_empty();
    if exact_release_tag && !dirty {
        Some(package_version.to_owned())
    } else {
        Some(format!(
            "{package_version}+g{short_revision}{}",
            if dirty { ".dirty" } else { "" }
        ))
    }
}

fn git_stdout(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        // Reading the semantic state must not rewrite the index Cargo would
        // otherwise consider an input on direct, non-xtask builds.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn rustc_host_target(root: &Path) -> Result<String> {
    let output = Command::new("rustc")
        .arg("-vV")
        .current_dir(root)
        .output()
        .context("querying rustc host target")?;
    if !output.status.success() {
        bail!("rustc -vV failed");
    }

    let stdout = String::from_utf8(output.stdout).context("reading rustc -vV output")?;
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::to_owned)
        .context("rustc -vV output did not include a host target")
}

fn build_host_release(root: &Path) -> Result<()> {
    let envs = presence_plugin_embed_env(root);
    run_with_env(
        root,
        "cargo",
        host_build_args(HostProfile::Release, &[]),
        &envs,
    )
}

fn build_darwin_artifacts(root: &Path) -> Result<()> {
    let envs = darwin_zigbuild_env(root)?;
    for target in DARWIN_TARGETS {
        ensure_rust_target(root, target)?;
        run_with_env(
            root,
            "cargo",
            [
                "zigbuild",
                "-p",
                "rimz",
                "--bin",
                "rimz",
                "--target",
                target,
                "--release",
                "--locked",
            ],
            &envs,
        )
        .with_context(|| {
            format!(
                "building macOS artifact for {target}; install cargo-zigbuild and zig if this fails before compilation"
            )
        })?;
    }
    Ok(())
}

/// The macOS target whose binary must carry an explicit code signature.
const DARWIN_SIGN_TARGET: &str = "aarch64-apple-darwin";

/// Ad-hoc sign the Apple Silicon binary in place. arm64 macOS refuses to `exec`
/// a mach-o that carries no code signature — the arm64 ABI, not Gatekeeper. zig
/// linker-signs this target and reserves the signature load command, so
/// `rcodesign sign` with no signing identity rewrites it to a proper ad-hoc
/// signature: no Apple certificate, no notarization, and a loud failure here if
/// the linker ever stops reserving that room. The x86_64 binary needs no
/// signature (Intel execs unsigned) and zig leaves no room to add one, so it
/// ships as built. Homebrew installs run the result without a Gatekeeper prompt
/// because `brew` fetches over curl and never sets the `com.apple.quarantine`
/// xattr.
fn codesign_arm64_artifact(root: &Path) -> Result<()> {
    let binary = target_release_artifact(root, DARWIN_SIGN_TARGET, "rimz");
    run(root, "rcodesign", [OsStr::new("sign"), binary.as_os_str()]).with_context(|| {
        format!(
            "ad-hoc signing {}; install rcodesign (cargo install apple-codesign) if this fails",
            binary.display()
        )
    })
}

fn darwin_zigbuild_env(root: &Path) -> Result<Vec<(&'static str, PathBuf)>> {
    let mut envs = presence_plugin_embed_env(root);
    if cfg!(target_os = "macos")
        || env::var_os("SDKROOT")
            .as_deref()
            .is_some_and(rustc_accepts_macos_sdkroot)
    {
        return Ok(envs);
    }

    // `rustc` shells out to `xcrun` for Apple SDK discovery unless SDKROOT is
    // an existing absolute path. This synthetic root gives rustc an SDK path
    // and Zig a framework search tree; cargo-zigbuild supplies the Darwin libc
    // stubs.
    let sdkroot = target_dir(root)
        .join("xtask")
        .join("darwin-zigbuild-sdkroot");
    prepare_darwin_zigbuild_sdkroot(&sdkroot)?;
    envs.push(("SDKROOT", sdkroot));
    Ok(envs)
}

fn prepare_darwin_zigbuild_sdkroot(sdkroot: &Path) -> Result<()> {
    // `cargo-zigbuild` brings libSystem and libiconv stubs, while macOS crates
    // can still ask the linker for framework load commands. These text stubs
    // carry the public framework symbols RimZ's current macOS process reader
    // references; the shipped binary resolves them from macOS at runtime.
    fs::create_dir_all(sdkroot.join("usr").join("lib"))
        .with_context(|| format!("creating {}", sdkroot.join("usr").join("lib").display()))?;
    write_framework_tbd(sdkroot, "CoreFoundation", DARWIN_COREFOUNDATION_TBD)?;
    write_framework_tbd(sdkroot, "IOKit", DARWIN_IOKIT_TBD)
}

fn write_framework_tbd(sdkroot: &Path, framework: &str, bytes: &str) -> Result<()> {
    let path = sdkroot
        .join("System")
        .join("Library")
        .join("Frameworks")
        .join(format!("{framework}.framework"))
        .join(format!("{framework}.tbd"));
    write_atomically(&path, bytes.as_bytes())
}

fn rustc_accepts_macos_sdkroot(sdkroot: &OsStr) -> bool {
    let path = Path::new(sdkroot);
    path.is_absolute()
        && path != Path::new("/")
        && path.exists()
        && !macos_sdkroot_points_at_other_apple_platform(sdkroot)
}

fn macos_sdkroot_points_at_other_apple_platform(sdkroot: &OsStr) -> bool {
    let sdkroot = sdkroot.to_string_lossy();
    [
        "iPhoneOS.platform",
        "iPhoneSimulator.platform",
        "AppleTVOS.platform",
        "AppleTVSimulator.platform",
        "WatchOS.platform",
        "WatchSimulator.platform",
        "XROS.platform",
        "XRSimulator.platform",
    ]
    .iter()
    .any(|platform| sdkroot.contains(platform))
}

fn package_dist_artifacts(root: &Path, artifacts: &BTreeMap<String, PathBuf>) -> Result<()> {
    let dist = dist_dir(root);
    fs::create_dir_all(&dist).with_context(|| format!("creating {}", dist.display()))?;
    remove_stale_dist_archives(&dist)?;
    let mut checksums = String::new();
    for (target, binary) in artifacts {
        let package = format!("rimz-{target}");
        let package_dir = dist.join(&package);
        fs::create_dir_all(&package_dir)
            .with_context(|| format!("creating {}", package_dir.display()))?;
        copy_atomically(binary, &package_dir.join("rimz"))?;

        let archive = format!("{package}.tar.gz");
        let archive_path = dist.join(&archive);
        create_archive_atomically(&dist, &package, &archive_path)?;
        let digest = sha256_file(&archive_path)?;
        checksums.push_str(&format!("{digest}  {archive}\n"));
    }
    write_atomically(&dist.join("SHA256SUMS"), checksums.as_bytes())
}

fn remove_stale_dist_archives(dist: &Path) -> Result<()> {
    for entry in fs::read_dir(dist).with_context(|| format!("reading {}", dist.display()))? {
        let entry = entry.with_context(|| format!("reading {}", dist.display()))?;
        let file_type = entry
            .file_type()
            .with_context(|| format!("reading {}", entry.path().display()))?;
        if !file_type.is_file() {
            continue;
        }

        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name == "SHA256SUMS" || (name.starts_with("rimz-") && name.ends_with(".tar.gz")) {
            fs::remove_file(entry.path())
                .with_context(|| format!("removing {}", entry.path().display()))?;
        }
    }
    Ok(())
}

fn create_archive_atomically(work_dir: &Path, package: &str, dest: &Path) -> Result<()> {
    let file_name = dest
        .file_name()
        .with_context(|| format!("{} has no file name", dest.display()))?
        .to_string_lossy();
    let staged = work_dir.join(format!(".{file_name}.tmp.{}", process::id()));
    remove_stale_file(&staged)?;
    run(
        work_dir,
        "tar",
        [OsStr::new("-czf"), staged.as_os_str(), OsStr::new(package)],
    )?;
    fs::rename(&staged, dest).with_context(|| format!("installing {}", dest.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests;
