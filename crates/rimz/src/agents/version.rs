//! Shared CLI version probing for agent adapters.
//!
//! Adapter-specific transports (statusline, app-server context, account files)
//! report a version per live session. This module is the cheap out-of-band
//! source: run `<binary> --version`, capture both streams, and parse the
//! conventional leading version token where a caller needs ordering. Where
//! several sources disagree, `newest_version` picks the one to show.
//! Agent CLI releases have disagreed on the stream, so the probe reads both.

use std::ffi::OsStr;
use std::process::{Command, Stdio};
use std::str::FromStr;

/// A simple three-part CLI version. Agent CLIs do not need semver metadata for
/// RimZ's gates; ordered numeric major/minor/patch is the contract.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CliVersion {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl CliVersion {
    pub(super) const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

impl std::fmt::Display for CliVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum VersionParseErr {
    #[error("missing version token")]
    Empty,
    #[error("expected two or three numeric dot-separated version segments")]
    SegmentCount,
    #[error("version segment `{segment}` is not a number")]
    Number { segment: String },
}

impl FromStr for CliVersion {
    type Err = VersionParseErr;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let token = input
            .split_whitespace()
            .next()
            .ok_or(VersionParseErr::Empty)?;
        let token = token.strip_prefix('v').unwrap_or(token);
        if token.is_empty() {
            return Err(VersionParseErr::Empty);
        }

        let parts = token.split('.').collect::<Vec<_>>();
        if !(2..=3).contains(&parts.len()) {
            return Err(VersionParseErr::SegmentCount);
        }
        let parse = |segment: &str| {
            segment.parse::<u64>().map_err(|_| VersionParseErr::Number {
                segment: segment.to_owned(),
            })
        };
        Ok(Self {
            major: parse(parts[0])?,
            minor: parse(parts[1])?,
            patch: parts.get(2).map_or(Ok(0), |segment| parse(segment))?,
        })
    }
}

/// The newest of several reported versions of one CLI, returned as written;
/// blank candidates are ignored. A string that parses as a [`CliVersion`]
/// outranks one that does not, the greater `CliVersion` wins, and the greater
/// string breaks what remains, so the answer never depends on input order.
pub(crate) fn newest_version<'a>(candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    candidates
        .into_iter()
        .filter(|candidate| !candidate.trim().is_empty())
        .max_by_key(|candidate| (candidate.parse::<CliVersion>().ok(), *candidate))
}

/// Run `<binary> --version` with captured stdio. Any failure is an absent
/// version, not account truth or a launch precondition on its own.
pub(super) fn probe_cli_version(binary: impl AsRef<OsStr>) -> Option<String> {
    probe_cli_version_with(binary, conventional_cli_version)
}

/// Run `<binary> --version` and pass its two output streams to the adapter's
/// parser. Keeping the streams separate lets branded CLIs recognize their own
/// banner without scanning unrelated release or upgrade prose.
pub(super) fn probe_cli_version_with(
    binary: impl AsRef<OsStr>,
    parse: impl FnOnce(&str, &str) -> Option<String>,
) -> Option<String> {
    let mut command = Command::new(binary);
    command.arg("--version").stdin(Stdio::null());
    let output = crate::proc::run_bounded_output(
        &mut command,
        crate::agents::account::INFORMATIONAL_PROBE_TIMEOUT,
    )
    .ok()?;
    if output.timed_out || !output.status.success() {
        return None;
    }
    parse(
        &String::from_utf8_lossy(&output.stdout),
        &String::from_utf8_lossy(&output.stderr),
    )
}

/// Pick the version from a `--version` probe's two streams. Scan both for the
/// first parseable version token so older Pi releases that used stderr remain
/// compatible with current releases that use stdout.
pub(super) fn conventional_cli_version(stdout: &str, stderr: &str) -> Option<String> {
    stdout
        .split_whitespace()
        .chain(stderr.split_whitespace())
        .find_map(|token| token.parse::<CliVersion>().ok())
        .map(|version| version.to_string())
}

#[derive(Debug, thiserror::Error)]
#[error(
    "{kind} {found} at {path} is older than {minimum}, the first release with {flags}, which RimZ passes on every launch; upgrade {display_name}"
)]
pub struct LaunchVersionErr {
    kind: &'static str,
    display_name: &'static str,
    path: std::path::PathBuf,
    found: CliVersion,
    minimum: CliVersion,
    flags: Box<str>,
}

/// Check the resolved launch binary, abstaining when its version is unreadable.
pub fn check_launch_version_floor(
    adapter: &super::AgentDefinition,
    path: &std::path::Path,
) -> Result<(), LaunchVersionErr> {
    if adapter.min_version().is_none() {
        return Ok(());
    }
    let found = probe_cli_version(path).and_then(|version| version.parse().ok());
    check_launch_version(adapter, path, found)
}

fn check_launch_version(
    adapter: &super::AgentDefinition,
    path: &std::path::Path,
    found: Option<CliVersion>,
) -> Result<(), LaunchVersionErr> {
    if let Some((minimum, found)) = adapter.min_version().zip(found)
        && found < minimum
    {
        return Err(LaunchVersionErr {
            kind: adapter.spec().kind,
            display_name: adapter.spec().display_name,
            path: path.to_owned(),
            found,
            minimum,
            flags: adapter.spec().launch.fixed_args.join(" ").into_boxed_str(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_version_floor_refuses_only_known_old_versions() {
        let codex = crate::agents::find_definition("codex").unwrap();
        let path = std::path::Path::new("/opt/bin/codex");
        for raw in [None, Some("0.156.0"), Some("0.159.2"), Some("0.156.0-beta")] {
            assert!(check_launch_version(codex, path, raw.and_then(|v| v.parse().ok())).is_ok());
        }
        let error = check_launch_version(codex, path, Some(CliVersion::new(0, 155, 9)))
            .expect_err("old Codex must refuse");
        let message = error.to_string();
        assert!(message.contains("0.155.9"));
        assert!(message.contains("0.156.0"));
        assert!(message.contains("/opt/bin/codex"));
        assert!(message.contains("upgrade Codex"));
        let claude = crate::agents::find_definition("claude").unwrap();
        assert!(check_launch_version(claude, path, Some(CliVersion::new(0, 0, 1))).is_ok());
    }

    #[test]
    fn from_str_parses_leading_token_and_rejects_garbage() {
        assert_eq!(
            "2.1.173 (Claude Code)".parse::<CliVersion>(),
            Ok(CliVersion::new(2, 1, 173))
        );
        assert_eq!("v2.1".parse::<CliVersion>(), Ok(CliVersion::new(2, 1, 0)));

        assert_eq!("".parse::<CliVersion>(), Err(VersionParseErr::Empty));
        assert_eq!(
            "2".parse::<CliVersion>(),
            Err(VersionParseErr::SegmentCount)
        );
        assert!(matches!(
            "2.x.0".parse::<CliVersion>(),
            Err(VersionParseErr::Number { .. })
        ));
    }

    #[test]
    fn orders_numeric_segments() {
        assert!(CliVersion::new(2, 1, 51) < CliVersion::new(2, 1, 157));
        assert!(CliVersion::new(2, 1, 157) < CliVersion::new(2, 1, 173));
        assert!(CliVersion::new(2, 10, 0) > CliVersion::new(2, 9, 9));
    }

    #[test]
    fn picks_conventional_version_from_either_stream_and_abstains_on_raw_prose() {
        // Claude and Codex print `--version` to stdout; the first parseable token wins.
        assert_eq!(
            conventional_cli_version("2.1.173 (Claude Code)\n", "").as_deref(),
            Some("2.1.173")
        );
        assert_eq!(
            conventional_cli_version("codex-cli 0.139.0\n", "").as_deref(),
            Some("0.139.0")
        );
        // Older Pi releases printed `--version` to stderr; keep accepting it.
        assert_eq!(
            conventional_cli_version("", "0.78.1\n").as_deref(),
            Some("0.78.1")
        );
        assert_eq!(conventional_cli_version("not a version", ""), None);
        assert_eq!(conventional_cli_version("", ""), None);
    }

    #[test]
    fn newest_version_is_order_independent_and_returned_as_written() {
        let both_orders = |a: &'static str, b: &'static str| {
            let forward = newest_version([a, b]);
            assert_eq!(forward, newest_version([b, a]), "{a} against {b}");
            forward
        };
        assert_eq!(both_orders("2.1.99", "2.1.100"), Some("2.1.100"));
        assert_eq!(both_orders("0.161.0-alpha.1", "0.160.1"), Some("0.160.1"));
        assert_eq!(both_orders("nightly", "canary"), Some("nightly"));
        assert_eq!(both_orders("v2.1", "2.1.0"), Some("v2.1"));
        assert_eq!(both_orders("", "  "), None);
        assert_eq!(
            newest_version(["", "v2.1.7 (Claude Code)", " "]),
            Some("v2.1.7 (Claude Code)")
        );
        assert_eq!(newest_version([]), None);
    }
}
