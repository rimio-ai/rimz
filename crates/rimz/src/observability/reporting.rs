//! Off-box error reporting (Sentry).
//!
//! Reporting is off unless a DSN resolves from the `RIMZ_SENTRY_DSN` env or the
//! per-machine `[sentry]` config. When on, [`init`] returns a guard the binary
//! holds for the process lifetime — it flushes pending events on drop, which
//! covers the short-lived hook subprocesses — and [`sentry_tracing_layer`]
//! bridges the `tracing` subscriber so RimZ `warn!`/`error!`, including the
//! agent turn-error warning under the `rimz::agent::turn_error` target, becomes
//! a Sentry event (warning / error level mirrors the tracing level). Warnings
//! under [`SIDEBAR_HEALTH_TARGET`] stay local because the durable diagnostics
//! log already carries those sidebar refresh episodes.
//!
//! Events arrive with debug context: [`set_command_scope`] tags the scope with
//! the running command, build id, and (when a process serves one) the agent and
//! session, plus a structured `rimz` context grouping the same facts; callsites
//! add a searchable `tags.operation` and pass the error as `&dyn Error` so a
//! stacktrace and exception attach. `info!` lines become breadcrumbs, so a
//! warning arrives with the trail that led to it.
//!
//! Reporting is best-effort enrichment, not a precondition: a malformed DSN
//! logs the fix and stays off, and a network failure never blocks a RimZ path.
//! The hostname is withheld and PII is off by default — the telemetry surface
//! is documented in [`docs/guide/security.md`](../../../../docs/guide/security.md).

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use sentry::ClientInitGuard;
use sentry::protocol::Event;
use sentry::types::Dsn;
use sentry_tracing::EventFilter;
use tracing::{Level, Metadata};
use tracing_subscriber::Layer;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::registry::LookupSpan;

use super::{BREADCRUMB_TARGET, SIDEBAR_HEALTH_TARGET, ScopeFacts};
use crate::config::MachineConfig;
use crate::workspace::ENV_WORKSPACE_ID;

const ENV_DSN: &str = "RIMZ_SENTRY_DSN";
const ENV_ENVIRONMENT: &str = "RIMZ_SENTRY_ENVIRONMENT";

/// The tracing target the hook lifecycle emits agent-observed turn errors on —
/// provider rate-limit/overload conditions RimZ watches, not RimZ faults.
/// Events on this target carry `fault=agent`; every other reporting event carries
/// `fault=rimz`, so triage filters our bugs from upstream hiccups.
const AGENT_CONDITION_TARGET: &str = "rimz::agent::turn_error";

/// Per-fingerprint off-box budget: at most [`RATE_LIMIT_BURST`] events sharing a
/// fingerprint per [`RATE_LIMIT_WINDOW_MS`]. A `warn!` on a per-frame sidebar
/// path would otherwise flood Sentry with tens of thousands of identical events,
/// burying real signal and the quota; this caps the bleed without silencing it.
const RATE_LIMIT_WINDOW_MS: u64 = 60_000;
const RATE_LIMIT_BURST: u32 = 5;
/// Bound the limiter's live key set so an unexpectedly high-cardinality
/// fingerprint cannot grow the window map without limit; expired windows are
/// pruned once the map passes this many keys.
const RATE_LIMIT_MAX_KEYS: usize = 1024;

/// Outcome of [`init`], held by the binary for the process lifetime.
#[must_use = "drop flushes pending events; hold the guard for the process lifetime"]
pub enum Reporting {
    /// No DSN configured — Sentry is off and no client exists.
    Off,
    /// A DSN resolved and the client initialized.
    On(ClientInitGuard),
    /// A DSN resolved but failed to parse; reporting stays off. The message is
    /// logged by [`Reporting::report`] once the subscriber is live.
    InvalidDsn(String),
}

impl Reporting {
    /// Whether a Sentry client is active — drives whether the binary attaches
    /// [`sentry_tracing_layer`] to the subscriber.
    pub fn enabled(&self) -> bool {
        matches!(self, Self::On(_))
    }

    /// Surface a malformed-DSN misconfiguration. Called after the tracing
    /// subscriber is installed so the error reaches stderr / state logs.
    pub fn report(&self) {
        if let Self::InvalidDsn(error) = self {
            tracing::error!(
                target: "rimz::observability",
                error = %error,
                "invalid Sentry DSN; reporting disabled — fix [sentry] dsn in ~/.config/rimz/config.toml or set RIMZ_SENTRY_DSN",
            );
        }
    }
}

/// Initialize Sentry when a DSN resolves. Best-effort: a config that fails to
/// load is treated as absent (the invoked command surfaces config errors), and
/// a DSN that fails to parse yields [`Reporting::InvalidDsn`] rather than a
/// panic or a degraded surface.
pub fn init() -> Reporting {
    let config = MachineConfig::load_lenient();
    let Some((dsn, environment)) = resolve_from(
        env_nonempty(ENV_DSN),
        env_nonempty(ENV_ENVIRONMENT),
        &config,
    ) else {
        return Reporting::Off;
    };
    let dsn = match dsn.parse::<sentry::types::Dsn>() {
        Ok(dsn) => dsn,
        Err(err) => return Reporting::InvalidDsn(err.to_string()),
    };
    let guard = sentry::init(client_options(dsn, environment));
    tag_scope();
    Reporting::On(guard)
}

/// Build the [`sentry::ClientOptions`] for a resolved DSN and environment. Split
/// from [`init`] so the before-send enrichment — hostname stripping, the fault
/// tag, stable fingerprinting, and the per-fingerprint rate limit — is covered
/// by tests driving a recording transport.
fn client_options(dsn: Dsn, environment: String) -> sentry::ClientOptions {
    sentry::ClientOptions {
        dsn: Some(dsn),
        release: release(),
        environment: Some(environment.into()),
        // External reporting withholds personal data by default; strip the
        // hostname the contexts integration would otherwise attach.
        send_default_pii: false,
        // Attach a stacktrace to error/warning events that carry an exception
        // (a callsite's `error = &err as &dyn Error`) so a report names where it
        // came from, not just what failed. Free with the `backtrace` feature.
        attach_stacktrace: true,
        // Mark RimZ frames in-app and the Sentry crates out-of-app so Sentry
        // picks the RimZ callsite as the culprit instead of a `tracing`/`sentry`
        // internal. The payoff lands once debug files are uploaded per release.
        in_app_include: vec!["rimz"],
        in_app_exclude: vec!["sentry", "tracing"],
        before_send: Some(before_send()),
        ..Default::default()
    }
}

/// The Sentry release for this process: `rimz@<build id>`, the digest of the
/// running executable the diagnostics log and the `build` tag already stamp, so
/// one identity tracks regressions, makes `resolve --in-next-release` reopen on
/// a real new build, and keys uploaded debug files. Falls back to the crate
/// version when the binary cannot be digested.
fn release() -> Option<Cow<'static, str>> {
    match crate::build_id::current() {
        Some(build) => Some(Cow::Owned(format!("rimz@{build}"))),
        None => sentry::release_name!(),
    }
}

/// The before-send hook: strip the hostname, then on reporting events (those
/// carrying a tracing target) add the `fault` tag, pin a stable fingerprint, and
/// rate-limit per fingerprint. Non-reporting events — a panic, a manual capture —
/// keep Sentry's default grouping and are never throttled.
fn before_send() -> Arc<dyn Fn(Event<'static>) -> Option<Event<'static>> + Send + Sync> {
    let limiter = Arc::new(Mutex::new(RateLimiter::new()));
    let base = Instant::now();
    Arc::new(move |mut event: Event<'static>| {
        event.server_name = None;
        let Some(logger) = event.logger.clone() else {
            return Some(event);
        };
        event
            .tags
            .insert("fault".to_owned(), fault_for(&logger).to_owned());
        let parts = fingerprint_components(
            &logger,
            event.tags.get("operation").map(String::as_str),
            event.message.as_deref(),
        );
        let key = hash_parts(&parts);
        event.fingerprint = parts.into_iter().map(Cow::Owned).collect::<Vec<_>>().into();
        let now_ms = u64::try_from(base.elapsed().as_millis()).unwrap_or(u64::MAX);
        if limiter
            .lock()
            .expect("sentry rate-limiter lock")
            .over_budget(key, now_ms)
        {
            return None;
        }
        Some(event)
    })
}

/// Classify a reporting event by its tracing target: an agent-observed condition
/// (provider rate-limit/overload) versus a RimZ fault.
fn fault_for(logger: &str) -> &'static str {
    if logger == AGENT_CONDITION_TARGET {
        "agent"
    } else {
        "rimz"
    }
}

/// Stable Sentry grouping key for a reporting event: a namespace, the tracing
/// target, the `operation` tag, and the static message. The unsymbolicated
/// release stack varies frame-to-frame and splits one callsite across groups;
/// pinning the fingerprint to these stable facts collapses it back to one issue.
/// Error-carrying callsites move their text into the exception, leaving no
/// message — `target` plus the low-cardinality `operation` keeps them grouped.
fn fingerprint_components(
    logger: &str,
    operation: Option<&str>,
    message: Option<&str>,
) -> Vec<String> {
    let mut parts = Vec::with_capacity(4);
    parts.push("rimz".to_owned());
    parts.push(logger.to_owned());
    if let Some(operation) = operation {
        parts.push(format!("op:{operation}"));
    }
    if let Some(message) = message {
        parts.push(format!("msg:{message}"));
    }
    parts
}

fn hash_parts(parts: &[String]) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for part in parts {
        part.hash(&mut hasher);
    }
    hasher.finish()
}

/// Fixed-window per-fingerprint rate limiter for the off-box channel.
struct RateLimiter {
    windows: HashMap<u64, Window>,
}

struct Window {
    start_ms: u64,
    count: u32,
}

impl RateLimiter {
    fn new() -> Self {
        Self {
            windows: HashMap::new(),
        }
    }

    /// Record one event for `key` at `now_ms` and report whether it exceeds the
    /// window budget (and so should be dropped).
    fn over_budget(&mut self, key: u64, now_ms: u64) -> bool {
        if self.windows.len() > RATE_LIMIT_MAX_KEYS {
            self.windows
                .retain(|_, window| now_ms.saturating_sub(window.start_ms) < RATE_LIMIT_WINDOW_MS);
        }
        let window = self.windows.entry(key).or_insert(Window {
            start_ms: now_ms,
            count: 0,
        });
        if now_ms.saturating_sub(window.start_ms) >= RATE_LIMIT_WINDOW_MS {
            window.start_ms = now_ms;
            window.count = 0;
        }
        window.count += 1;
        window.count > RATE_LIMIT_BURST
    }
}

/// Tag the active scope with the pinned workspace so a machine-global DSN still
/// filters per project in Sentry.
fn tag_scope() {
    if let Some(workspace) = env_nonempty(ENV_WORKSPACE_ID) {
        sentry::configure_scope(|scope| scope.set_tag("workspace", workspace));
    }
}

/// Tag the active scope with the command and build id and attach a structured
/// `rimz` context grouping the command, build, session, and agent, so every
/// event this process reports inherits them. The hub is process-global, so one
/// call near dispatch covers the long-lived `sidebar serve` and each short-lived
/// hook subprocess alike.
pub fn set_command_scope(facts: ScopeFacts<'_>) {
    use sentry::protocol::{Context, Value};

    let build = crate::build_id::current_if_ready();
    sentry::configure_scope(|scope| {
        scope.set_tag("command", facts.command);
        if let Some(build) = build {
            scope.set_tag("build", build);
        }
        let mut ctx = std::collections::BTreeMap::<String, Value>::new();
        ctx.insert("command".into(), facts.command.into());
        if let Some(build) = build {
            ctx.insert("build".into(), build.into());
        }
        if let Some(session) = facts.session {
            ctx.insert("session".into(), session.into());
        }
        if let Some(agent) = facts.agent {
            ctx.insert("agent".into(), agent.into());
        }
        scope.set_context("rimz", Context::Other(ctx));
    });
}

/// The Sentry reporting layer layer: `warn!`/`error!` become Sentry events and an `info!`
/// on [`BREADCRUMB_TARGET`] becomes a breadcrumb attached to the next event, so
/// a warning arrives with the trail that led to it. The `INFO` level filter
/// keeps the global max-level hint at `INFO` — `debug!`/`trace!` are still never
/// constructed — so the breadcrumb trail stays a cold-path concern (the
/// `sidebar serve` hot loop emits no `info!`).
pub fn sentry_tracing_layer<S>() -> impl Layer<S>
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    sentry_tracing::layer()
        .event_filter(event_filter)
        .with_filter(LevelFilter::INFO)
}

fn event_filter(metadata: &Metadata<'_>) -> EventFilter {
    classify(*metadata.level(), metadata.target())
}

/// `warn!`/`error!` map to Sentry events (the event level mirrors the tracing
/// level), except sidebar health warnings under [`SIDEBAR_HEALTH_TARGET`] which
/// are already durable local diagnostics; an `info!` on [`BREADCRUMB_TARGET`]
/// maps to a breadcrumb; everything else is ignored, so an unmarked `info!`
/// never carries its fields off-box.
fn classify(level: Level, target: &str) -> EventFilter {
    match level {
        Level::ERROR | Level::WARN if target == SIDEBAR_HEALTH_TARGET => EventFilter::Ignore,
        Level::ERROR | Level::WARN => EventFilter::Event,
        Level::INFO if target == BREADCRUMB_TARGET => EventFilter::Breadcrumb,
        _ => EventFilter::Ignore,
    }
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|value| !value.is_empty())
}

/// Resolve `(dsn, environment)` with env overriding the per-machine config.
/// Empty strings count as unset; environment defaults by build profile via
/// [`default_environment`].
fn resolve_from(
    env_dsn: Option<String>,
    env_environment: Option<String>,
    config: &MachineConfig,
) -> Option<(String, String)> {
    let dsn = env_dsn
        .or_else(|| config.sentry.dsn.clone())
        .filter(|value| !value.is_empty())?;
    let environment = env_environment
        .or_else(|| config.sentry.environment.clone())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default_environment().to_owned());
    Some((dsn, environment))
}

/// The default deployment environment when neither env nor config sets one: an
/// installed release reports as `production`; dev, profiling, and CI builds
/// report as `development`, so the production dashboard stays clear of
/// contributor noise.
fn default_environment() -> &'static str {
    environment_for_build_profile(option_env!("RIMZ_BUILD_PROFILE"))
}

fn environment_for_build_profile(profile: Option<&str>) -> &'static str {
    if profile == Some("release") {
        "production"
    } else {
        "development"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(dsn: Option<&str>, environment: Option<&str>) -> MachineConfig {
        let mut config = MachineConfig::default();
        config.sentry.dsn = dsn.map(ToOwned::to_owned);
        config.sentry.environment = environment.map(ToOwned::to_owned);
        config
    }

    #[test]
    fn unset_dsn_resolves_to_none() {
        assert!(resolve_from(None, None, &MachineConfig::default()).is_none());
        // An empty config value is also "off".
        assert!(resolve_from(None, None, &config_with(Some(""), None)).is_none());
    }

    #[test]
    fn config_dsn_defaults_environment_by_build_profile() {
        let (dsn, environment) = resolve_from(
            None,
            None,
            &config_with(Some("https://k@o1.ingest.sentry.io/2"), None),
        )
        .expect("dsn resolves");
        assert_eq!(dsn, "https://k@o1.ingest.sentry.io/2");
        assert_eq!(environment, default_environment());
        // The suite builds under the dev profile, so the default is development.
        assert_eq!(environment, "development");
    }

    #[test]
    fn build_profile_maps_release_only_to_production() {
        assert_eq!(environment_for_build_profile(Some("release")), "production");
        assert_eq!(
            environment_for_build_profile(Some("profiling")),
            "development"
        );
        assert_eq!(environment_for_build_profile(Some("debug")), "development");
        assert_eq!(environment_for_build_profile(None), "development");
    }

    #[test]
    fn env_overrides_config_dsn_and_environment() {
        let config = config_with(Some("https://config@o1.ingest.sentry.io/2"), Some("prod"));
        let (dsn, environment) = resolve_from(
            Some("https://env@o9.ingest.sentry.io/9".to_owned()),
            Some("dev".to_owned()),
            &config,
        )
        .expect("dsn resolves");
        assert_eq!(dsn, "https://env@o9.ingest.sentry.io/9");
        assert_eq!(environment, "dev");
    }

    #[test]
    fn config_environment_used_when_env_absent() {
        let config = config_with(Some("https://k@o1.ingest.sentry.io/2"), Some("staging"));
        let (_, environment) = resolve_from(None, None, &config).expect("dsn resolves");
        assert_eq!(environment, "staging");
    }

    #[test]
    fn classify_maps_levels_and_gates_breadcrumbs_to_the_trail_target() {
        // WARN/ERROR are events regardless of target.
        assert!(classify(Level::ERROR, "rimz::anything").contains(EventFilter::Event));
        assert!(classify(Level::WARN, "rimz::anything").contains(EventFilter::Event));
        assert!(classify(Level::ERROR, SIDEBAR_HEALTH_TARGET).is_empty());
        assert!(classify(Level::WARN, SIDEBAR_HEALTH_TARGET).is_empty());
        // INFO is a breadcrumb only on the dedicated trail target.
        let trail = classify(Level::INFO, BREADCRUMB_TARGET);
        assert!(trail.contains(EventFilter::Breadcrumb));
        assert!(!trail.contains(EventFilter::Event));
        // An unmarked INFO (e.g. a broker socket-path log) is ignored — its
        // fields never ride off-box as breadcrumb data. (`EventFilter::Ignore`
        // is the empty flag set.)
        assert!(classify(Level::INFO, "rimz::agents::codex::broker").is_empty());
        // DEBUG/TRACE are always ignored, even on the trail target.
        assert!(classify(Level::DEBUG, BREADCRUMB_TARGET).is_empty());
        assert!(classify(Level::TRACE, "rimz::anything").is_empty());
    }

    #[test]
    fn release_is_build_id_qualified() {
        let release = release().expect("the test binary is digestible");
        assert!(release.starts_with("rimz@"), "release was {release:?}");
        assert_eq!(release.strip_prefix("rimz@"), crate::build_id::current());
    }

    #[test]
    fn fault_for_splits_agent_conditions_from_rimz() {
        assert_eq!(fault_for(AGENT_CONDITION_TARGET), "agent");
        assert_eq!(fault_for("rimz::agent::lifecycle"), "rimz");
        assert_eq!(fault_for("rimz::observability"), "rimz");
    }

    #[test]
    fn fingerprint_collapses_a_callsite_across_stacks() {
        let with_op =
            fingerprint_components("rimz::agent::lifecycle", Some("codex.spawn"), Some("boom"));
        assert_eq!(
            with_op,
            vec![
                "rimz".to_owned(),
                "rimz::agent::lifecycle".to_owned(),
                "op:codex.spawn".to_owned(),
                "msg:boom".to_owned(),
            ],
        );
        // Same facts → same group, regardless of which stack produced them.
        assert_eq!(
            with_op,
            fingerprint_components("rimz::agent::lifecycle", Some("codex.spawn"), Some("boom")),
        );
        // A different message is a different group; operation is optional.
        assert_ne!(
            with_op,
            fingerprint_components("rimz::agent::lifecycle", Some("codex.spawn"), Some("other")),
        );
        assert_eq!(
            fingerprint_components("rimz::x", None, Some("m")),
            vec!["rimz".to_owned(), "rimz::x".to_owned(), "msg:m".to_owned()],
        );
    }

    #[test]
    fn rate_limiter_caps_a_hot_key_then_reopens_next_window() {
        let mut limiter = RateLimiter::new();
        let key = 7;
        // The first burst of events in the window pass.
        for i in 0..RATE_LIMIT_BURST {
            assert!(
                !limiter.over_budget(key, 0),
                "event {i} within the burst passes"
            );
        }
        // The rest of the window is dropped.
        assert!(
            limiter.over_budget(key, 10),
            "burst+1 in the same window drops"
        );
        assert!(
            limiter.over_budget(key, RATE_LIMIT_WINDOW_MS - 1),
            "still dropping until the window closes",
        );
        // A new window reopens the budget; an independent key has its own.
        assert!(
            !limiter.over_budget(key, RATE_LIMIT_WINDOW_MS),
            "the next window passes again",
        );
        assert!(
            !limiter.over_budget(99, RATE_LIMIT_WINDOW_MS),
            "an independent key has an independent budget",
        );
    }

    use std::time::Duration;

    use tracing_subscriber::layer::SubscriberExt;

    /// Records every captured event so a test can assert what reached Sentry.
    struct Recorder {
        events: Arc<Mutex<Vec<sentry::protocol::Event<'static>>>>,
    }

    impl sentry::Transport for Recorder {
        fn send_envelope(&self, envelope: sentry::Envelope) {
            for item in envelope.items() {
                if let sentry::protocol::EnvelopeItem::Event(event) = item {
                    self.events
                        .lock()
                        .expect("recorder lock")
                        .push(event.clone());
                }
            }
        }
    }

    // Sentry's hub is process-global; nextest isolates each test in its own
    // process, so installing a client here does not leak into sibling tests.
    #[test]
    fn warn_and_error_route_to_sentry_at_matching_levels() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let recorder: Arc<Recorder> = Arc::new(Recorder {
            events: events.clone(),
        });
        let _guard = sentry::init(sentry::ClientOptions {
            dsn: Some(
                "https://public@example.com/1"
                    .parse()
                    .expect("test dsn parses"),
            ),
            transport: Some(Arc::new(recorder)),
            ..Default::default()
        });

        let subscriber = tracing_subscriber::registry().with(sentry_tracing_layer());
        tracing::subscriber::with_default(subscriber, || {
            // INFO on the trail target is a breadcrumb: it rides along on the
            // next captured event rather than becoming an event of its own.
            tracing::info!(target: BREADCRUMB_TARGET, "about to end the turn");
            // Stands in for the agent turn-error warning emitted by the hook
            // lifecycle — agent-generated, reported at warning level.
            tracing::warn!(
                target: "rimz::agent::turn_error",
                class = "PausedRateLimit",
                "agent turn ended on a provider error",
            );
            tracing::error!("a rimz failure");
        });
        sentry::Hub::current()
            .client()
            .expect("client installed")
            .flush(Some(Duration::from_secs(1)));

        let captured = events.lock().expect("recorder lock");
        let levels: Vec<sentry::Level> = captured.iter().map(|event| event.level).collect();
        assert_eq!(
            levels,
            vec![sentry::Level::Warning, sentry::Level::Error],
            "agent warning then rimz error, info is a breadcrumb not an event",
        );
        let warning = &captured[0];
        assert!(
            warning
                .breadcrumbs
                .values
                .iter()
                .any(|crumb| crumb.message.as_deref() == Some("about to end the turn")),
            "the info line rode along on the warning as a breadcrumb",
        );
    }

    // Sentry's hub is process-global; nextest isolates each test in its own
    // process, so the scope set here does not leak into sibling tests.
    #[test]
    fn enriched_scope_and_fields_reach_sentry() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let recorder: Arc<Recorder> = Arc::new(Recorder {
            events: events.clone(),
        });
        let _guard = sentry::init(sentry::ClientOptions {
            dsn: Some(
                "https://public@example.com/1"
                    .parse()
                    .expect("test dsn parses"),
            ),
            transport: Some(Arc::new(recorder)),
            attach_stacktrace: true,
            ..Default::default()
        });

        set_command_scope(ScopeFacts {
            command: "codex refresh-context",
            session: Some("ses_x"),
            agent: Some("codex"),
        });

        let subscriber = tracing_subscriber::registry().with(sentry_tracing_layer());
        tracing::subscriber::with_default(subscriber, || {
            let err = std::io::Error::other("boom");
            tracing::warn!(
                tags.operation = "oauth_usage",
                tags.provider = "codex",
                error = &err as &dyn std::error::Error,
                "OAuth account usage fetch failed",
            );
        });
        sentry::Hub::current()
            .client()
            .expect("client installed")
            .flush(Some(Duration::from_secs(1)));

        let captured = events.lock().expect("recorder lock");
        let event = captured
            .iter()
            .find(|event| event.level == sentry::Level::Warning)
            .expect("warning captured");
        // Scope tag for the command; `tags.`-prefixed field promoted to a tag.
        assert_eq!(
            event.tags.get("command").map(String::as_str),
            Some("codex refresh-context")
        );
        assert_eq!(
            event.tags.get("operation").map(String::as_str),
            Some("oauth_usage")
        );
        assert_eq!(
            event.tags.get("provider").map(String::as_str),
            Some("codex")
        );
        // `error = &dyn Error` attaches an exception (and, with attach_stacktrace, a stack).
        assert!(
            !event.exception.values.is_empty(),
            "dyn Error attaches an exception"
        );
        // The structured `rimz` context carries session and agent.
        match event.contexts.get("rimz") {
            Some(sentry::protocol::Context::Other(map)) => {
                assert_eq!(
                    map.get("session").and_then(|value| value.as_str()),
                    Some("ses_x")
                );
                assert_eq!(
                    map.get("agent").and_then(|value| value.as_str()),
                    Some("codex")
                );
            }
            other => panic!("missing or unexpected rimz context: {other:?}"),
        }
    }

    // Sentry's hub is process-global; nextest isolates each test in its own
    // process, so installing a client here does not leak into sibling tests.
    #[test]
    fn before_send_tags_fault_fingerprints_and_rate_limits() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let recorder: Arc<Recorder> = Arc::new(Recorder {
            events: events.clone(),
        });
        // Drive the real before_send (fault tag, fingerprint, rate limit)
        // through a recording transport.
        let mut options = client_options(
            "https://public@example.com/1"
                .parse()
                .expect("test dsn parses"),
            "test".to_owned(),
        );
        options.transport = Some(Arc::new(recorder));
        let _guard = sentry::init(options);

        let subscriber = tracing_subscriber::registry().with(sentry_tracing_layer());
        tracing::subscriber::with_default(subscriber, || {
            // An agent-observed condition is tagged fault=agent.
            tracing::warn!(
                target: AGENT_CONDITION_TARGET,
                class = "PausedRateLimit",
                "agent paused on a provider error",
            );
            // A hot RimZ callsite fired past the burst: fault=rimz, one
            // fingerprint, and only the first burst reaches the transport.
            for _ in 0..(RATE_LIMIT_BURST + 3) {
                tracing::warn!(
                    target: "rimz::agent::lifecycle",
                    "subagent names a parent with no row",
                );
            }
        });
        sentry::Hub::current()
            .client()
            .expect("client installed")
            .flush(Some(Duration::from_secs(1)));

        let captured = events.lock().expect("recorder lock");
        let agent = captured
            .iter()
            .find(|event| event.logger.as_deref() == Some(AGENT_CONDITION_TARGET))
            .expect("agent condition captured");
        assert_eq!(agent.tags.get("fault").map(String::as_str), Some("agent"));

        let rimz: Vec<_> = captured
            .iter()
            .filter(|event| event.logger.as_deref() == Some("rimz::agent::lifecycle"))
            .collect();
        assert_eq!(
            rimz.len(),
            RATE_LIMIT_BURST as usize,
            "the over-budget repeats were dropped before send",
        );
        assert!(
            rimz.iter()
                .all(|event| event.tags.get("fault").map(String::as_str) == Some("rimz")),
            "RimZ faults are tagged fault=rimz",
        );
        assert!(
            rimz.iter().all(|event| event
                .fingerprint
                .iter()
                .any(|part| part.as_ref() == "rimz::agent::lifecycle")),
            "the callsite is pinned to a stable fingerprint",
        );
    }

    #[test]
    fn sidebar_crash_report_carries_signal_stderr_and_stable_fingerprint() {
        let mut event = sentry::protocol::Event {
            level: sentry::Level::Error,
            logger: Some("rimz::sidebar::crash".into()),
            message: Some("sidebar render worker terminated abnormally".into()),
            ..Default::default()
        };
        event
            .tags
            .insert("operation".to_owned(), "sidebar.render_crash".into());
        event
            .extra
            .insert("signal".to_owned(), serde_json::Value::from(6));
        event
            .extra
            .insert("stderr".to_owned(), "rimz test sidebar worker abort".into());

        let event = before_send()(event).expect("event kept");

        assert_eq!(event.level, sentry::Level::Error);
        assert_eq!(event.tags.get("fault").map(String::as_str), Some("rimz"));
        assert_eq!(
            event.tags.get("operation").map(String::as_str),
            Some("sidebar.render_crash")
        );
        assert_eq!(
            event.extra.get("stderr").and_then(|value| value.as_str()),
            Some("rimz test sidebar worker abort")
        );
        assert_eq!(
            event.extra.get("signal").and_then(|value| value.as_u64()),
            Some(6)
        );
        for expected in [
            "rimz::sidebar::crash",
            "op:sidebar.render_crash",
            "msg:sidebar render worker terminated abnormally",
        ] {
            assert!(
                event
                    .fingerprint
                    .iter()
                    .any(|part| part.as_ref() == expected),
                "fingerprint missing {expected}: {:?}",
                event.fingerprint
            );
        }
    }
}
