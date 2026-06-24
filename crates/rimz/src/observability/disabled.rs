//! No-op reporting surface for builds without the `sentry` feature. Same shape
//! as the feature-enabled reporting module so the binary wires it identically.

use super::ScopeFacts;

/// Reporting is compiled out; the only outcome is "off".
#[must_use]
pub enum Reporting {
    Off,
}

impl Reporting {
    pub fn enabled(&self) -> bool {
        false
    }

    pub fn report(&self) {}
}

pub fn init() -> Reporting {
    Reporting::Off
}

pub fn set_command_scope(_facts: ScopeFacts<'_>) {}

pub fn sentry_tracing_layer<S>() -> impl tracing_subscriber::Layer<S>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    tracing_subscriber::layer::Identity::new()
}
