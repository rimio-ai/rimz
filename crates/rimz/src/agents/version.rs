//! Shared CLI version probing for agent adapters.
//!
//! Adapter-specific transports (statusline, app-server context, account files)
//! still win when they carry a fresher version. This module is the cheap
//! out-of-band fallback: run `<binary> --version`, capture stdout, and parse the
//! conventional leading version token where a caller needs ordering.

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
pub(crate) fn probe_cli_version(binary: &str) -> Option<String> {
    let output = Command::new(binary)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    normalize_cli_version_output(&String::from_utf8_lossy(&output.stdout))
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
    fn parses_leading_cli_version_token() {
        assert_eq!(
            "2.1.173 (Claude Code)".parse::<CliVersion>(),
            Ok(CliVersion::new(2, 1, 173))
        );
        assert_eq!("v2.1".parse::<CliVersion>(), Ok(CliVersion::new(2, 1, 0)));
    }

    #[test]
    fn orders_numeric_segments() {
        assert!(CliVersion::new(2, 1, 51) < CliVersion::new(2, 1, 157));
        assert!(CliVersion::new(2, 1, 157) < CliVersion::new(2, 1, 173));
        assert!(CliVersion::new(2, 10, 0) > CliVersion::new(2, 9, 9));
    }

    #[test]
    fn rejects_garbage_versions() {
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
    fn normalizes_probe_stdout_to_the_first_parseable_version() {
        assert_eq!(
            normalize_cli_version_output("codex-cli 0.139.0\n").as_deref(),
            Some("0.139.0")
        );
        assert_eq!(
            normalize_cli_version_output("2.1.173 (Claude Code)").as_deref(),
            Some("2.1.173")
        );
        assert_eq!(
            normalize_cli_version_output("not a version").as_deref(),
            Some("not a version")
        );
        assert_eq!(normalize_cli_version_output("   "), None);
    }
}
