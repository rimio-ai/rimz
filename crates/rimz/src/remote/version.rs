//! Client/host version-skew classification for remote attaches.

/// The numeric components RimZ uses to judge remote compatibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

impl Version {
    fn parse(input: &str) -> Option<Self> {
        let core = input.split(['-', '+']).next()?;
        let mut components = core.split('.');
        let version = Self {
            major: components.next()?.parse().ok()?,
            minor: components.next()?.parse().ok()?,
            patch: components.next()?.parse().ok()?,
        };
        components.next().is_none().then_some(version)
    }
}

/// The first numeric component on which the remote client and host differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skew {
    Match,
    Patch,
    Minor,
    Major,
    Unparseable,
}

pub fn classify(client: &str, host: &str) -> Skew {
    if client == host {
        return Skew::Match;
    }
    let (Some(client), Some(host)) = (Version::parse(client), Version::parse(host)) else {
        return Skew::Unparseable;
    };
    if client.major != host.major {
        Skew::Major
    } else if client.minor != host.minor {
        Skew::Minor
    } else if client.patch != host.patch {
        Skew::Patch
    } else {
        Skew::Match
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_the_first_differing_numeric_component() {
        assert_eq!(classify("0.5.0", "0.5.0"), Skew::Match);
        assert_eq!(classify("0.5.0", "0.5.1"), Skew::Patch);
        assert_eq!(classify("0.5.0", "0.4.9"), Skew::Minor);
        assert_eq!(classify("1.5.0", "1.4.9"), Skew::Minor);
        assert_eq!(classify("1.0.0", "0.9.9"), Skew::Major);
        assert_eq!(classify("0.4.10", "0.4.9"), Skew::Patch);
    }

    #[test]
    fn accepts_prerelease_and_build_suffixes() {
        assert_eq!(classify("0.5.0-dev", "0.5.0+release"), Skew::Match);
        assert_eq!(classify("0.5.1-dev+abc", "0.5.0"), Skew::Patch);
    }

    #[test]
    fn unreadable_versions_are_unparseable_unless_identical() {
        for version in ["dev", "", "1.2"] {
            assert_eq!(classify(version, "0.5.0"), Skew::Unparseable);
            assert_eq!(classify(version, version), Skew::Match);
        }
    }
}
