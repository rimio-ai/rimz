//! Shared CLI version probing for agent adapters.
//!
//! Adapter-specific transports (statusline, app-server context, account files)
//! still win when they carry a fresher version. This module is the cheap
//! out-of-band fallback: run `<binary> --version`, capture both streams, and
//! parse the conventional leading version token where a caller needs ordering.
//! Agent CLI releases have disagreed on the stream, so the probe reads both.

use std::ffi::OsStr;
use std::process::{Command, Stdio};
use std::str::FromStr;

/// A simple three-part CLI version. Agent CLIs do not need semver metadata for
/// Rimz's gates; ordered numeric major/minor/patch is the contract.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CliVersion {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl CliVersion {
    pub const fn new(major: u64, minor: u64, patch: u64) -> Self {
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

/// Run `<binary> --version` with captured stdio. Any failure is an absent
/// version, not account truth or a launch precondition on its own.
pub(crate) fn probe_cli_version(binary: impl AsRef<OsStr>) -> Option<String> {
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
    cli_version_from_streams(
        &String::from_utf8_lossy(&output.stdout),
        &String::from_utf8_lossy(&output.stderr),
    )
}

/// Pick the version from a `--version` probe's two streams. Scan both for the
/// first parseable version token so older Pi releases that used stderr remain
/// compatible with current releases that use stdout.
fn cli_version_from_streams(stdout: &str, stderr: &str) -> Option<String> {
    normalize_cli_version_output(&format!("{stdout}\n{stderr}"))
}

fn normalize_cli_version_output(output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return None;
    }
    trimmed
        .split_whitespace()
        .find_map(|token| token.parse::<CliVersion>().ok())
        .map(|version| version.to_string())
        .or_else(|| Some(trimmed.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn picks_version_from_either_stream_and_falls_back_to_raw() {
        // Claude and Codex print `--version` to stdout; the first parseable token wins.
        assert_eq!(
            cli_version_from_streams("2.1.173 (Claude Code)\n", "").as_deref(),
            Some("2.1.173")
        );
        assert_eq!(
            cli_version_from_streams("codex-cli 0.139.0\n", "").as_deref(),
            Some("0.139.0")
        );
        // Older Pi releases printed `--version` to stderr; keep accepting it.
        assert_eq!(
            cli_version_from_streams("", "0.78.1\n").as_deref(),
            Some("0.78.1")
        );
        // No parseable token falls back to the trimmed raw string; nothing at all
        // is an absent version.
        assert_eq!(
            cli_version_from_streams("not a version", "").as_deref(),
            Some("not a version")
        );
        assert_eq!(cli_version_from_streams("", ""), None);
    }
}
