//! Machine/container pressure-stall readings, preferring the cgroup namespace root.

use std::path::{Path, PathBuf};

/// A resource with a kernel pressure-stall reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Resource {
    Cpu,
    Io,
    Memory,
}

impl Resource {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Io => "io",
            Self::Memory => "memory",
        }
    }
}

/// The `some` line of one pressure file, in percent of wall time stalled.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Pressure {
    pub(crate) avg10: f32,
    pub(crate) avg60: f32,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum PressureErr {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{}: no `some` line", .0.display())]
    MissingSome(PathBuf),
}

/// Parse the `some` line shared by cgroup and proc pressure files.
fn parse_pressure(text: &str) -> Option<Pressure> {
    let mut fields = text
        .lines()
        .find_map(|line| line.strip_prefix("some "))?
        .split_ascii_whitespace();
    let mut average = |key: &str| {
        fields
            .next()?
            .strip_prefix(key)?
            .strip_prefix('=')?
            .parse::<f32>()
            .ok()
    };
    Some(Pressure {
        avg10: average("avg10")?,
        avg60: average("avg60")?,
    })
}

/// Read the namespace-wide cgroup PSI, falling back to proc per resource.
pub(crate) fn read(resource: Resource) -> Result<Pressure, PressureErr> {
    #[cfg(any(test, feature = "testkit"))]
    if let Some(root) = std::env::var_os("RIMZ_TEST_PRESSURE_ROOT") {
        return read_from(Path::new(&root), resource);
    }
    read_from(Path::new("/"), resource)
}

fn read_from(root: &Path, resource: Resource) -> Result<Pressure, PressureErr> {
    let cgroup = root
        .join("sys/fs/cgroup")
        .join(format!("{}.pressure", resource.name()));
    read_file(cgroup).or_else(|_| read_file(root.join("proc/pressure").join(resource.name())))
}

fn read_file(path: PathBuf) -> Result<Pressure, PressureErr> {
    match std::fs::read_to_string(&path) {
        Ok(text) => parse_pressure(&text).ok_or(PressureErr::MissingSome(path)),
        Err(source) => Err(PressureErr::Io { path, source }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressure_parses_the_some_line_of_real_proc_text() {
        let text = "some avg10=22.92 avg60=23.06 avg300=31.40 total=1234567\n\
                    full avg10=0.00 avg60=1.50 avg300=2.00 total=99\n";
        assert_eq!(
            parse_pressure(text),
            Some(Pressure {
                avg10: 22.92,
                avg60: 23.06
            })
        );
        // `/proc/pressure/cpu` on an older kernel carries only the `some` line.
        assert_eq!(
            parse_pressure("some avg10=0.00 avg60=0.10 avg300=0.05 total=5\n"),
            Some(Pressure {
                avg10: 0.0,
                avg60: 0.1
            })
        );
        assert_eq!(parse_pressure("full avg10=0.00 avg60=0.00\n"), None);
        assert_eq!(parse_pressure(""), None);
    }

    #[test]
    fn source_prefers_parsable_cgroup_then_proc_per_resource() {
        let cgroup = "some avg10=34.00 avg60=18.00 avg300=1.00 total=1\n";
        let proc = "some avg10=52.00 avg60=27.00 avg300=1.00 total=1\n";
        let cases = [
            (Some(cgroup), Some(proc), Some((34.0, 18.0))),
            (None, Some(proc), Some((52.0, 27.0))),
            (
                Some("full avg10=0.00 avg60=0.00\n"),
                Some(proc),
                Some((52.0, 27.0)),
            ),
            (
                Some("some avg10=bad avg60=0.00\n"),
                Some(proc),
                Some((52.0, 27.0)),
            ),
            (None, None, None),
        ];
        let mut actual = Vec::new();
        let mut expected = Vec::new();
        for resource in [Resource::Memory, Resource::Cpu, Resource::Io] {
            for (cgroup, proc, reading) in cases {
                let root = tempfile::tempdir().unwrap();
                let cgroup_dir = root.path().join("sys/fs/cgroup");
                let proc_dir = root.path().join("proc/pressure");
                std::fs::create_dir_all(&cgroup_dir).unwrap();
                std::fs::create_dir_all(&proc_dir).unwrap();
                if let Some(text) = cgroup {
                    std::fs::write(
                        cgroup_dir.join(format!("{}.pressure", resource.name())),
                        text,
                    )
                    .unwrap();
                }
                if let Some(text) = proc {
                    std::fs::write(proc_dir.join(resource.name()), text).unwrap();
                }
                let result = read_from(root.path(), resource);
                actual.push(result.as_ref().ok().map(|p| (p.avg10, p.avg60)));
                expected.push(reading);
                if let Err(error) = result {
                    let path = proc_dir.join(resource.name());
                    assert!(
                        error.to_string().starts_with(&path.display().to_string()),
                        "{error}"
                    );
                }
            }
        }
        assert_eq!(actual, expected);
    }
}
