use serde::{Deserialize, Serialize};

/// Off-box error reporting target. Lives in the per-machine tier so it never
/// rides the shared, trust-gated `.rimz/config.toml` surface — a clone never
/// inherits a DSN. The reporting code is a dev-only build feature
/// (`--features sentry`); without it, this section is inert. With the feature
/// enabled and no `dsn` set, Sentry stays off and RimZ makes no network calls;
/// the [`crate::observability`] module reads this section (and the
/// `RIMZ_SENTRY_DSN` / `RIMZ_SENTRY_ENVIRONMENT` overrides) at startup.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct SentryConfig {
    /// Sentry DSN. Empty or unset disables reporting.
    pub dsn: Option<String>,
    /// Deployment environment tag (e.g. `dev`, `production`). Defaults to
    /// `production` when unset.
    pub environment: Option<String>,
}
