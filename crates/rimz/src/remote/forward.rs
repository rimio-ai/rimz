//! Local TCP forwards carried by remote SSH connections.

use std::fmt;
use std::num::ParseIntError;
use std::str::FromStr;

/// One loopback-only SSH local forward.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortForward {
    pub local: u16,
    pub remote: u16,
}

/// How a `[LOCAL:]REMOTE` forward can fail to parse or merge.
#[derive(Debug, thiserror::Error)]
pub enum PortForwardError {
    #[error("port forward is empty; expected `[LOCAL:]REMOTE` — e.g. `3000` or `8080:3000`")]
    Empty,
    #[error(
        "port forward `{spec}` has invalid port `{port}` ({source}); expected ports from 1 to 65535 in `[LOCAL:]REMOTE` — e.g. `3000` or `8080:3000`"
    )]
    InvalidPort {
        spec: String,
        port: String,
        #[source]
        source: ParseIntError,
    },
    #[error(
        "port forward `{0}` uses port 0; choose ports from 1 to 65535 in `[LOCAL:]REMOTE` — e.g. `3000` or `8080:3000`"
    )]
    ZeroPort(String),
    #[error(
        "port forward `{0}` has too many `:` parts; expected `[LOCAL:]REMOTE` — e.g. `3000` or `8080:3000`"
    )]
    TooManySegments(String),
    #[error(
        "local forward port {0} is claimed by different remote ports; remove one forward or choose another local port with `--forward LOCAL:REMOTE`"
    )]
    LocalPortConflict(u16),
}

impl FromStr for PortForward {
    type Err = PortForwardError;

    fn from_str(spec: &str) -> Result<Self, Self::Err> {
        if spec.is_empty() {
            return Err(PortForwardError::Empty);
        }
        let mut parts = spec.split(':');
        let Some(first) = parts.next() else {
            return Err(PortForwardError::Empty);
        };
        let second = parts.next();
        if parts.next().is_some() {
            return Err(PortForwardError::TooManySegments(spec.to_owned()));
        }
        let (local, remote) = match second {
            Some(remote) => (parse_port(spec, first)?, parse_port(spec, remote)?),
            None => {
                let port = parse_port(spec, first)?;
                (port, port)
            }
        };
        Ok(Self { local, remote })
    }
}

impl fmt::Display for PortForward {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.local == self.remote {
            write!(f, "{}", self.local)
        } else {
            write!(f, "{}:{}", self.local, self.remote)
        }
    }
}

fn parse_port(spec: &str, port: &str) -> Result<u16, PortForwardError> {
    let parsed = port
        .parse::<u16>()
        .map_err(|source| PortForwardError::InvalidPort {
            spec: spec.to_owned(),
            port: port.to_owned(),
            source,
        })?;
    if parsed == 0 {
        return Err(PortForwardError::ZeroPort(spec.to_owned()));
    }
    Ok(parsed)
}

/// Build the OpenSSH argv pairs for loopback-only local forwards.
pub fn ssh_args(forwards: &[PortForward]) -> Vec<String> {
    let mut args = Vec::with_capacity(forwards.len() * 2);
    for forward in forwards {
        args.push("-L".to_owned());
        args.push(format!(
            "127.0.0.1:{}:127.0.0.1:{}",
            forward.local, forward.remote
        ));
    }
    args
}

/// Combine persistent and per-invocation forwards in declaration order.
pub fn merged(
    alias: &[PortForward],
    cli: &[PortForward],
) -> Result<Vec<PortForward>, PortForwardError> {
    let mut merged: Vec<PortForward> = Vec::with_capacity(alias.len() + cli.len());
    for forward in alias.iter().chain(cli) {
        match merged.iter().find(|known| known.local == forward.local) {
            Some(known) if known == forward => {}
            Some(_) => return Err(PortForwardError::LocalPortConflict(forward.local)),
            None => merged.push(*forward),
        }
    }
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_specs_and_rejects_invalid_specs() {
        for (spec, expected) in [
            (
                "3000",
                PortForward {
                    local: 3000,
                    remote: 3000,
                },
            ),
            (
                "8080:3000",
                PortForward {
                    local: 8080,
                    remote: 3000,
                },
            ),
            (
                "1:65535",
                PortForward {
                    local: 1,
                    remote: 65535,
                },
            ),
        ] {
            let parsed: PortForward = spec.parse().unwrap();
            assert_eq!(parsed, expected, "{spec}");
            assert_eq!(parsed.to_string().parse::<PortForward>().unwrap(), expected);
        }

        for (spec, expected) in [
            ("", "empty"),
            ("nope", "invalid port"),
            ("3000:nope", "invalid port"),
            ("65536", "invalid port"),
            ("0", "port 0"),
            ("3000:0", "port 0"),
            ("3000:4000:5000", "too many"),
        ] {
            let error = spec.parse::<PortForward>().unwrap_err().to_string();
            assert!(error.contains(expected), "{spec}: {error}");
        }
    }

    #[test]
    fn display_uses_the_canonical_short_form() {
        assert_eq!("03000".parse::<PortForward>().unwrap().to_string(), "3000");
        assert_eq!(
            "08080:03000".parse::<PortForward>().unwrap().to_string(),
            "8080:3000"
        );
    }

    #[test]
    fn merge_deduplicates_pairs_and_rejects_local_conflicts() {
        let same: PortForward = "3000".parse().unwrap();
        let extra: PortForward = "8080:3001".parse().unwrap();
        assert_eq!(merged(&[same], &[same, extra]).unwrap(), vec![same, extra]);

        let conflict: PortForward = "3000:4000".parse().unwrap();
        assert!(matches!(
            merged(&[same], &[conflict]),
            Err(PortForwardError::LocalPortConflict(3000))
        ));
    }
}
