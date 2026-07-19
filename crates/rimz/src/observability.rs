//! Off-box error reporting, behind the `sentry` build feature.
//!
//! The reporting code (the `sentry`/`sentry-tracing` trees) compiles only with
//! `--features sentry`; shipped binaries build without it and report nothing.
//! [`BREADCRUMB_TARGET`], [`SIDEBAR_HEALTH_TARGET`], and [`ScopeFacts`] are
//! always present so the callsites that seed breadcrumbs, mark local-only
//! sidebar health warnings, and command scope need no `#[cfg]`. With the
//! feature on, `reporting` is the live impl; off, `disabled` is a no-op with
//! the same surface, so `main.rs` and `cli/mod.rs` are identical in both builds.

/// The target a deliberate breadcrumb seed emits under. Only an `info!` on this
/// target becomes a Sentry breadcrumb, so the trail is the curated set of
/// cold-path steps — never an arbitrary `info!` field (a socket path, a cwd)
/// the privacy boundary keeps off-box.
pub const BREADCRUMB_TARGET: &str = "rimz::trail";

/// The target for sidebar health warnings that are already recorded locally as
/// diagnostics. The Sentry reporting layer ignores this target to keep hot-loop refresh
/// flaps on-box.
pub const SIDEBAR_HEALTH_TARGET: &str = "rimz::sidebar::health";

/// Low-cardinality facts the cli layer knows after parsing the command line.
/// Values stay free of arguments and free-form text so they make stable Sentry
/// facets.
pub struct ScopeFacts<'a> {
    /// The resolved command, e.g. `"sidebar serve"` or `"agents refresh-context"`.
    pub command: &'a str,
    /// The agent session the process acts on, when exactly one is known.
    pub session: Option<&'a str>,
    /// The agent kind the process serves, when the command implies one.
    pub agent: Option<&'a str>,
}

#[cfg(feature = "sentry")]
mod reporting;
#[cfg(feature = "sentry")]
pub use reporting::{Reporting, init, sentry_tracing_layer, set_command_scope};

#[cfg(not(feature = "sentry"))]
mod disabled;
#[cfg(not(feature = "sentry"))]
pub use disabled::{Reporting, init, sentry_tracing_layer, set_command_scope};
