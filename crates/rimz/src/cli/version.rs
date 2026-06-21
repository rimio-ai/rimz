//! Build version surfaced by every human-facing CLI report.

pub(crate) const VERSION: &str = match option_env!("RIMZ_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_starts_with_package_version() {
        assert!(VERSION.starts_with(env!("CARGO_PKG_VERSION")), "{VERSION}");
    }
}
