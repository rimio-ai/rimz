//! Web-terminal backend selection, version gates, and daemon argv.

use std::fmt;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use crate::config::{MachineConfig, WebBackend};
use crate::mux::CommandSpec;

use super::{Result, WebErr, ttyd};

const MIN_TTYD_VERSION: Version = Version::new(1, 7, 5);
const MIN_GOTTY_VERSION: Version = Version::new(1, 8, 0);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Version {
    major: u32,
    minor: u32,
    patch: u32,
}

impl Version {
    const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

pub(super) fn program(backend: WebBackend) -> Result<PathBuf> {
    let (environment, binary, minimum) = match backend {
        WebBackend::Ttyd => ("RIMZ_TTYD_BIN", "ttyd", MIN_TTYD_VERSION),
        WebBackend::Gotty => ("RIMZ_GOTTY_BIN", "gotty", MIN_GOTTY_VERSION),
    };
    if let Some(path) = std::env::var_os(environment) {
        return Ok(PathBuf::from(path));
    }
    which::which(binary).map_err(|_| match backend {
        WebBackend::Ttyd => WebErr::MissingTtyd {
            minimum: minimum.to_string(),
        },
        WebBackend::Gotty => WebErr::MissingGotty {
            minimum: minimum.to_string(),
        },
    })
}

pub(super) fn version_at(program: &Path) -> Result<String> {
    let output = std::process::Command::new(program)
        .arg("--version")
        .output()
        .map_err(|source| WebErr::Io {
            path: program.to_path_buf(),
            source,
        })?;
    let text = if output.stdout.is_empty() {
        &output.stderr
    } else {
        &output.stdout
    };
    Ok(String::from_utf8_lossy(text).trim().to_owned())
}

pub(super) fn required_program_with_version(backend: WebBackend) -> Result<(PathBuf, String)> {
    let program = program(backend)?;
    let reported = version_at(&program)?;
    require_supported_version(backend, &reported)?;
    Ok((program, reported))
}

fn require_supported_version(backend: WebBackend, reported: &str) -> Result<Version> {
    let minimum = match backend {
        WebBackend::Ttyd => MIN_TTYD_VERSION,
        WebBackend::Gotty => MIN_GOTTY_VERSION,
    };
    let found = parse_version(backend, reported);
    let Some(found) = found else {
        return Err(too_old(backend, reported.to_owned(), minimum));
    };
    if found < minimum {
        return Err(too_old(backend, found.to_string(), minimum));
    }
    Ok(found)
}

fn too_old(backend: WebBackend, found: String, minimum: Version) -> WebErr {
    match backend {
        WebBackend::Ttyd => WebErr::TtydTooOld {
            found,
            minimum: minimum.to_string(),
        },
        WebBackend::Gotty => WebErr::GottyTooOld {
            found,
            minimum: minimum.to_string(),
        },
    }
}

fn parse_version(backend: WebBackend, reported: &str) -> Option<Version> {
    let prefix = match backend {
        WebBackend::Ttyd => "ttyd version ",
        WebBackend::Gotty => "gotty version ",
    };
    let version = reported
        .trim()
        .strip_prefix(prefix)?
        .split_whitespace()
        .next()?;
    let version = match backend {
        WebBackend::Ttyd => version,
        WebBackend::Gotty => version.strip_prefix('v').unwrap_or(version),
    };
    let mut components = version.splitn(3, '.');
    let major = components.next()?.parse().ok()?;
    let minor = components.next()?.parse().ok()?;
    let patch_and_suffix = components.next()?;
    let patch_len = patch_and_suffix
        .bytes()
        .take_while(u8::is_ascii_digit)
        .count();
    if patch_len == 0 {
        return None;
    }
    let (patch, suffix) = patch_and_suffix.split_at(patch_len);
    if !suffix.is_empty() && !matches!(suffix.as_bytes().first(), Some(b'-' | b'+')) {
        return None;
    }
    Some(Version::new(major, minor, patch.parse().ok()?))
}

pub(super) const fn process_label(backend: WebBackend) -> &'static str {
    match backend {
        WebBackend::Ttyd => "ttyd",
        WebBackend::Gotty => "gotty",
    }
}

pub(super) fn writable_argv(
    backend: WebBackend,
    program: &Path,
    rimz_exe: &Path,
    interface: IpAddr,
    port: u16,
    secret: &str,
    extra_args: &[String],
) -> CommandSpec {
    let mut spec = CommandSpec::new(program.display().to_string());
    match backend {
        WebBackend::Ttyd => {
            spec = spec
                .args(["-W", "-O", "-a", "-c"])
                .arg(format!("rimz:{secret}"))
                .args(["-i", &interface.to_string(), "-p"])
                .arg(port.to_string())
                .args(extra_args.iter().cloned());
        }
        WebBackend::Gotty => {
            spec = spec
                .args(["-w", "--permit-arguments", "-c"])
                .arg(format!("rimz:{secret}"))
                .args(["-a", &interface.to_string(), "-p"])
                .arg(port.to_string())
                .args(["--title-format", "RimZ"]);
        }
    }
    spec.arg(rimz_exe.display().to_string())
        .args(["web", "exec"])
}

pub(super) fn share_argv(
    backend: WebBackend,
    program: &Path,
    rimz_exe: &Path,
    interface: IpAddr,
    port: u16,
    extra_args: &[String],
) -> CommandSpec {
    let mut spec = CommandSpec::new(program.display().to_string());
    match backend {
        WebBackend::Ttyd => {
            spec = spec
                .args(["-O", "-a", "-i", &interface.to_string(), "-p"])
                .arg(port.to_string())
                .args(extra_args.iter().cloned());
        }
        WebBackend::Gotty => {
            spec = spec
                .args(["--permit-arguments", "-a", &interface.to_string(), "-p"])
                .arg(port.to_string())
                .args(["--title-format", "RimZ"]);
        }
    }
    spec.arg(rimz_exe.display().to_string())
        .args(["web", "exec", "--share"])
}

pub(super) fn client_profile(
    backend: WebBackend,
    config: &MachineConfig,
    program: &Path,
    version: &str,
) -> ttyd::client::ClientProfile {
    match backend {
        WebBackend::Ttyd => ttyd::client::profile(config, program, version),
        WebBackend::Gotty => ttyd::client::ClientProfile::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gotty_version_parser_matches_the_release_binary_and_rejects_old_versions() {
        assert_eq!(
            parse_version(WebBackend::Gotty, "gotty version v1.8.0"),
            Some(Version::new(1, 8, 0))
        );
        assert!(matches!(
            require_supported_version(WebBackend::Gotty, "gotty version v1.7.9"),
            Err(WebErr::GottyTooOld { found, minimum })
                if found == "1.7.9" && minimum == "1.8.0"
        ));
    }

    #[test]
    fn ttyd_version_gate_accepts_the_minimum_and_packaged_suffixes() {
        assert!(matches!(
            require_supported_version(WebBackend::Ttyd, "ttyd version 1.7.4"),
            Err(WebErr::TtydTooOld { found, minimum })
                if found == "1.7.4" && minimum == "1.7.5"
        ));
        assert_eq!(
            require_supported_version(WebBackend::Ttyd, "ttyd version 1.7.5")
                .expect("minimum ttyd version"),
            Version::new(1, 7, 5)
        );
        assert_eq!(
            parse_version(WebBackend::Ttyd, "ttyd version 1.7.7-1+deb13u1"),
            Some(Version::new(1, 7, 7))
        );
    }

    #[test]
    fn malformed_ttyd_versions_fail_with_the_reported_value() {
        for reported in ["", "ttyd 1.7.7", "ttyd version 1.7", "ttyd version current"] {
            assert!(matches!(
                require_supported_version(WebBackend::Ttyd, reported),
                Err(WebErr::TtydTooOld { found, minimum })
                    if found == reported && minimum == "1.7.5"
            ));
        }
    }

    #[test]
    fn gotty_writable_argv_uses_address_and_permits_url_arguments() {
        let spec = writable_argv(
            WebBackend::Gotty,
            Path::new("/tmp/gotty"),
            Path::new("/opt/rimz/bin/rimz"),
            "127.0.0.1".parse().expect("IP"),
            8200,
            "secret",
            &["ignored".to_owned()],
        );

        assert_eq!(
            spec.args,
            [
                "-w",
                "--permit-arguments",
                "-c",
                "rimz:secret",
                "-a",
                "127.0.0.1",
                "-p",
                "8200",
                "--title-format",
                "RimZ",
                "/opt/rimz/bin/rimz",
                "web",
                "exec"
            ]
        );
        assert!(!spec.args.iter().any(|arg| arg == "-W" || arg == "-O"));
    }

    #[test]
    fn gotty_share_argv_stays_read_only_and_unauthenticated() {
        let spec = share_argv(
            WebBackend::Gotty,
            Path::new("/tmp/gotty"),
            Path::new("/opt/rimz/bin/rimz"),
            "127.0.0.1".parse().expect("IP"),
            8201,
            &["ignored".to_owned()],
        );

        assert_eq!(
            spec.args,
            [
                "--permit-arguments",
                "-a",
                "127.0.0.1",
                "-p",
                "8201",
                "--title-format",
                "RimZ",
                "/opt/rimz/bin/rimz",
                "web",
                "exec",
                "--share"
            ]
        );
        assert!(!spec.args.iter().any(|arg| arg == "-w" || arg == "-c"));
        assert!(!spec.args.iter().any(|arg| arg == "-W" || arg == "-O"));
    }
}
