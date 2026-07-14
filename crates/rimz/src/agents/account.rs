//! Best-effort provider account probe contract.
//!
//! Account/plan facts are account-scoped, not session-scoped, and some never
//! ride the session context: Claude's subscription tier comes from `claude auth
//! status`, not its statusline. Each adapter probes those out-of-band facts in
//! its own `account.rs` ([`AgentAdapter::probe_account`]); this module owns the
//! shared [`AccountProbe`] outcome the sidebar producer folds onto the provider
//! dashboard.
//!
//! Producer-only: a probe may fork a subprocess, so the elected producer runs
//! it and publishes the result to the shared `accounts.json` cache (TTL'd,
//! single-flighted like the diff stats); consumer tabs read that cache and never
//! fork. A probe is a pure read — the cross-process memoization lives one layer
//! up, in [`crate::sidebar::cache`]'s producer cache.
//!
//! A probe also detects a *logged-in but idle* provider — one with no active
//! session this run — so the dashboard can show substantive accounts and their
//! budgets between turns. A live session's richer context still wins where both
//! exist.
//!
//! Best-effort by contract: a missing binary, a logged-out account, or
//! unparseable output yields no account. It never fails a snapshot — account is
//! enrichment, never correctness.
//!
//! [`AgentAdapter::probe_account`]: super::AgentAdapter::probe_account

use std::path::Path;

use super::ProviderAccountScope;

/// Cache identity of the credentials behind one provider usage probe.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountUsageIdentity {
    pub scope: ProviderAccountScope,
    pub account_key: Option<String>,
    pub credentials_stamp: Option<u64>,
}

/// Best-effort credential-file mtime for provider usage ranking.
pub(crate) fn file_mtime_ms(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
}

/// The outcome of an out-of-band account probe. The three arms drive the
/// producer's cache TTL: a `Found` or `LoggedOut` answer is authoritative and
/// rides the long success TTL, while `Unavailable` — a binary that would not run,
/// a non-zero exit, an unreadable file — is a transient failure the producer
/// retries on the short failure TTL instead of pinning the dashboard empty for
/// the full success window.
#[derive(Debug)]
pub enum AccountProbe {
    /// A logged-in account with the identity, plan, and metering facts its
    /// provider exposes.
    Found(super::AgentAccount),
    /// The probe ran and authoritatively found no login (logged out, or an auth
    /// file naming no credential). Cache it like a success: it changes about never.
    LoggedOut,
    /// The probe could not complete — the binary is missing, it exited non-zero,
    /// or its file was unreadable. Retry soon; absence here is not logged-out.
    Unavailable,
}
