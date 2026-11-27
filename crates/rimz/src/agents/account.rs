//! Best-effort provider account probe contract.
//!
//! Account/plan facts are account-scoped, not session-scoped, and some never
//! ride the session context: Claude's saved tier comes from `claude auth status`,
//! with a bound live profile override, not its statusline. Adapters normalize
//! these out-of-band facts through account capabilities; this module owns
//! [`AccountProbe`] and the profile state the producer folds onto the dashboard.
//!
//! Producer-only: a probe may fork a subprocess, so the room host runs
//! it and publishes the result to the shared `accounts.json` cache (TTL'd,
//! single-flighted like the diff stats); consumer tabs read that cache and never
//! fork. A probe is a pure read — the cross-process memoization lives one layer
//! up, in the sidebar producer cache.
//!
//! A probe also detects a *logged-in but idle* provider — one with no active
//! session this run — so the dashboard can show substantive accounts and their
//! budgets between turns. Probe, live session, and usage read all merge into
//! one stored record per login, and every surface reads only that record.
//!
//! Best-effort by contract: a missing binary, a logged-out account, or
//! unparseable output yields no account. It never fails a snapshot — account is
//! enrichment, never correctness.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::model_refusal::{ModelRefusal, ModelRefusals};
use super::{AgentAccount, RoomLoginSet, find_definition};
use super::{
    AgentRateLimits, AgentTurnError, ProviderAccountScope, RateLimitWindow,
    context::RateLimitWindowKey,
};
use crate::RuntimePaths;
use crate::ids::LoginKey;

/// Informational account and CLI-version probes are best-effort enrichment.
/// Bound every subprocess so one installed but wedged CLI cannot hold the
/// shared account cache producer indefinitely.
pub(crate) const INFORMATIONAL_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// How long the producer trusts a *successful* provider-account map before it
/// re-probes. A subscription tier and login state change rarely, so a coarse
/// TTL keeps the `claude auth status` subprocess off the per-tick produce path
/// while still picking up a login or logout within a few minutes. A confident
/// logged-out answer rides this same window. An answer derived from sessions
/// (Pi's provider) changes at a session's start instead, so an adapter that
/// stamps its account inputs re-probes on a stamp change behind
/// `ACCOUNTS_RETRY_TTL`. Launch routing trusts an answer a failed attempt kept
/// for this same window.
pub(crate) const ACCOUNTS_TTL: Duration = Duration::from_secs(10 * 60);

#[cfg(test)]
mod tests;

/// The producer's published account probe state, keyed by login so one
/// transient failure retries without expiring every account's successful read.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountsCache {
    pub logins: BTreeMap<LoginKey, ProviderRecord>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProviderRecord {
    pub probed_at_ms: u64,
    #[serde(default, skip_serializing_if = "super::is_zero_u32")]
    pub consecutive_failures: u32,
    /// Start of the version read, aged independently of provisional retries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_probed_at_ms: Option<u64>,
    /// A failed probe backs off from `ACCOUNTS_RETRY_TTL` to `ACCOUNTS_TTL`;
    /// success rides `ACCOUNTS_TTL`, or the retry floor while provisional.
    /// A moved adapter stamp makes either outcome due behind the retry floor.
    pub ok: bool,
    /// The adapter's inputs stamp read before the probe that wrote this record.
    /// Absent for an adapter without one and on a record an older build wrote,
    /// both of which wait out the TTL. A later change to the stamp's encoding
    /// costs one probe each time binaries of both encodings alternate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs_stamp: Option<u64>,
    /// Probed account facts. `None` on an `ok` record is a logged-out result,
    /// but a logout can also carry the CLI version, so `login` decides.
    pub account: Option<AgentAccount>,
    /// The last answer a check got, kept across failed attempts: `ok`,
    /// `probed_at_ms` and `probe_error` describe the latest attempt alone.
    /// Absent when no check has answered and on a record an older build wrote,
    /// which `ProviderStatus::from_record` reads from `ok` and `account`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<RecordedLogin>,
    /// When the check that got `login` started. Absent on a record an older
    /// build wrote, whose answer dates from `probed_at_ms`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe_error: Option<AccountProbeError>,
    /// Last profile attempt and answer, independent of account availability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<AccountProfileState>,
    /// When the reading behind each shared account fact was produced. A
    /// record an older build wrote has none, which reads as oldest.
    #[serde(default, skip_serializing_if = "AccountStamps::is_empty")]
    pub stamps: AccountStamps,
    /// The backing credentials, other than the selected one, that this login's
    /// live sessions run on, each with the last instant (Unix ms) a room's pass
    /// saw a live session on it. The record is machine-wide, so a pass only adds
    /// or raises its own room's sessions and drops what no room has seen for
    /// `LIVE_CREDENTIAL_TTL`. Only these credentials show as alternates, while
    /// their stamp is younger than that TTL. An older build's list reads as
    /// never seen.
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "live_credentials_seen"
    )]
    pub live_credentials: BTreeMap<String, u64>,
}

/// How long a credential stays an alternate after a room's pass last saw a live
/// session on it: several heavy passes of an unwatched room, which fold at
/// least every 5 s (`crate::sidebar::timing::UNWATCHED_FOLD_INTERVAL`). A
/// pass raises a stamp only once it is a third of this old, so a steady
/// session rewrites the record at most every 20 s.
const LIVE_CREDENTIAL_TTL: Duration = Duration::from_secs(60);

/// `ProviderRecord::live_credentials` as a map, or an older build's list of
/// names, each read as never seen.
fn live_credentials_seen<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, u64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Stored {
        Seen(BTreeMap<String, u64>),
        Names(BTreeSet<String>),
    }
    Ok(match Stored::deserialize(deserializer)? {
        Stored::Seen(seen) => seen,
        Stored::Names(names) => names.into_iter().map(|name| (name, 0)).collect(),
    })
}

/// Reading instants, in Unix milliseconds, of the shared account facts a
/// [`ProviderRecord`] holds: the plan (with its display name), metering, and
/// the account id. Version needs none: the newest version wins.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AccountStamps {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_at_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metered_at_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id_at_ms: Option<u64>,
    /// The reading instant of the last identity switch or logout, never
    /// lowered, except that a live switch by a session whose earliest window is
    /// no older than the replaced account id's stamp sets it at that window,
    /// still never below an earlier floor. A live or usage reading produced
    /// before it describes the previous identity and applies nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub floor_at_ms: Option<u64>,
    /// Whether a probe stated the stored account id, whatever that probe's
    /// instant. A live reading that states another id switches the identity
    /// only while none did, and so does an older probe: a login whose probe
    /// cannot read the identity (an agy keyring install) learns it from its
    /// sessions alone.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub account_id_from_probe: bool,
}

/// Where an account reading comes from. Only a probe reads the login's own
/// credentials, so a live reading switches the record to another identity only
/// where no probe stated the stored one, and a usage reading never does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadingSource {
    Probe,
    Live,
    Usage,
}

impl AccountStamps {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Whether a reading produced at `read_at_ms` may replace a fact stamped
/// `stamp`: an unstamped fact is oldest, and a tie applies.
fn reading_is_current(stamp: Option<u64>, read_at_ms: u64) -> bool {
    stamp.is_none_or(|stamp| read_at_ms >= stamp)
}

/// Fold one reading's shared account facts into `account`, the one merge every
/// source goes through (probe, live session, usage read). A live or usage
/// reading produced before the record's floor (its last identity switch or
/// logout) describes the previous identity and applies nothing; a probe reads
/// the login's credentials as they stand, so the floor never holds it back. A
/// fact changes only when the reading states it, was produced no earlier than
/// the stored fact, and differs from it: an equal value moves no stamp, so a
/// repeated reading writes nothing. An absent value, or an empty account id, is
/// no reading. Version takes the newest version.
///
/// Only a probe reads the login's own credentials, so a probe switches
/// identity: a current probe that states a different account id, profile
/// owner, metering, or selected credential starts the shared facts again from
/// that reading and raises the floor to its instant, never below an earlier
/// floor. A probe that states an account id is authoritative for it whatever
/// its instant: one stating the stored id marks it probed
/// (`AccountStamps::account_id_from_probe`), and one stating another id
/// switches even when older than the stored id, unless a probe stated that
/// one. A current live reading that states another account id switches the
/// same way while no probe stated the stored id. Otherwise a live or usage
/// reading that states another account id, profile owner or usage owner
/// applies nothing. One that names a credential other than the selected one
/// only fills that credential's empty plan, unless the reading's usage owner
/// differs from the credential's own. True when anything changed.
pub(crate) fn merge_account_reading(
    account: &mut AgentAccount,
    stamps: &mut AccountStamps,
    reading: &AgentAccount,
    read_at_ms: u64,
    source: ReadingSource,
) -> bool {
    if source != ReadingSource::Probe && stamps.floor_at_ms.is_some_and(|floor| read_at_ms < floor)
    {
        return false;
    }
    let stated_id = |account: &AgentAccount| account.account_id.clone().filter(|id| !id.is_empty());
    let other_id = states_other(stated_id(account).as_ref(), stated_id(reading).as_ref());
    let other_owner = states_other(
        account.profile_fingerprint.as_ref(),
        reading.profile_fingerprint.as_ref(),
    );
    let other_credential =
        account.sub_provider != reading.sub_provider || account.scope != reading.scope;
    let before = (account.clone(), stamps.clone());
    let live_switch = source == ReadingSource::Live
        && other_id
        && !stamps.account_id_from_probe
        && reading_is_current(stamps.account_id_at_ms, read_at_ms);
    if source != ReadingSource::Probe {
        if states_other(account.sub_provider.as_ref(), reading.sub_provider.as_ref())
            || (!reading.scope.is_kind_wide() && reading.scope != account.scope)
        {
            return fill_credential_plan(account, reading);
        }
        if (other_id && !live_switch)
            || other_owner
            || states_other(account.account_key.as_ref(), reading.account_key.as_ref())
        {
            return false;
        }
    } else if other_id
        && stamps.account_id_from_probe
        && !reading_is_current(stamps.account_id_at_ms, read_at_ms)
    {
        return false;
    }
    let held_facts = stamps.plan_at_ms.is_some()
        || stamps.metered_at_ms.is_some()
        || stamps.account_id_at_ms.is_some();
    let switched = live_switch
        || source == ReadingSource::Probe
            && (other_id
                || other_owner
                || (held_facts && other_credential)
                || (reading_is_current(stamps.metered_at_ms, read_at_ms)
                    && states_other(account.metered.as_ref(), reading.metered.as_ref())));
    if switched {
        account.plan = None;
        account.plan_name = None;
        account.metered = None;
        account.account_id = None;
        // A credential mtime can predate an earlier logout's floor.
        *stamps = AccountStamps {
            floor_at_ms: Some(
                stamps
                    .floor_at_ms
                    .map_or(read_at_ms, |floor| floor.max(read_at_ms)),
            ),
            ..AccountStamps::default()
        };
    }
    if let Some(plan) = reading
        .plan
        .as_deref()
        .map(str::trim)
        .filter(|plan| !plan.is_empty())
        && reading_is_current(stamps.plan_at_ms, read_at_ms)
    {
        // A reading of the same tier without a display name (a usage read)
        // keeps the name a richer source gave it.
        let plan_name = if reading.plan_name.is_some() || account.plan.as_deref() != Some(plan) {
            reading.plan_name.clone()
        } else {
            account.plan_name.clone()
        };
        if account.plan.as_deref() != Some(plan) || account.plan_name != plan_name {
            account.plan = Some(plan.to_owned());
            account.plan_name = plan_name;
            stamps.plan_at_ms = Some(read_at_ms);
        }
    }
    if let Some(metered) = reading.metered
        && reading_is_current(stamps.metered_at_ms, read_at_ms)
        && account.metered != Some(metered)
    {
        account.metered = Some(metered);
        stamps.metered_at_ms = Some(read_at_ms);
    }
    if let Some(account_id) = stated_id(reading) {
        if account.account_id.as_ref() == Some(&account_id) {
            // A probe states the login's own identity whatever its instant.
            stamps.account_id_from_probe |= source == ReadingSource::Probe;
        } else if reading_is_current(stamps.account_id_at_ms, read_at_ms) {
            account.account_id = Some(account_id);
            stamps.account_id_at_ms = Some(read_at_ms);
            stamps.account_id_from_probe = source == ReadingSource::Probe;
        }
    }
    account.version = super::version::newest_version(
        account
            .version
            .as_deref()
            .into_iter()
            .chain(reading.version.as_deref()),
    )
    .map(str::to_owned);
    (account.clone(), stamps.clone()) != before
}

/// A live or usage reading of a non-selected credential: its plan fills that
/// credential's entry when the entry has none, and reaches nothing else.
fn fill_credential_plan(account: &mut AgentAccount, reading: &AgentAccount) -> bool {
    let Some(plan) = reading
        .plan
        .as_deref()
        .map(str::trim)
        .filter(|plan| !plan.is_empty())
    else {
        return false;
    };
    let Some(credential) = account.credentials.values_mut().find(|credential| {
        credential.plan.is_none()
            && !states_other(
                credential.account_key.as_ref(),
                reading.account_key.as_ref(),
            )
            && match reading.sub_provider.as_deref() {
                Some(provider) => credential.sub_provider.as_deref() == Some(provider),
                None => credential.scope == reading.scope,
            }
    }) else {
        return false;
    };
    credential.plan = Some(plan.to_owned());
    credential.plan_name.clone_from(&reading.plan_name);
    true
}

/// Whether a reading states a value other than the stored one; an absent value
/// on either side states nothing.
fn states_other<T: PartialEq>(stored: Option<&T>, read: Option<&T>) -> bool {
    matches!((stored, read), (Some(stored), Some(read)) if stored != read)
}

impl ProviderRecord {
    /// Merge one reading into this record's account, creating it when the
    /// record holds none. See [`merge_account_reading`].
    pub(crate) fn merge_reading(
        &mut self,
        reading: &AgentAccount,
        read_at_ms: u64,
        source: ReadingSource,
    ) -> bool {
        let created = self.account.is_none();
        let account = self.account.get_or_insert_default();
        let changed = merge_account_reading(account, &mut self.stamps, reading, read_at_ms, source);
        if created && !changed {
            self.account = None;
        }
        changed
    }

    /// Record that this room's live sessions run on `providers` at `now_ms`:
    /// add each credential other than the selected one, raise a stamp a third
    /// of `LIVE_CREDENTIAL_TTL` old, and drop what no room has seen for the
    /// TTL. Another room's credentials stay. True when anything changed.
    pub(crate) fn see_live_credentials<'a>(
        &mut self,
        providers: impl IntoIterator<Item = &'a str>,
        now_ms: u64,
    ) -> bool {
        let selected = self
            .account
            .as_ref()
            .and_then(|account| account.sub_provider.clone());
        let before = self.live_credentials.clone();
        self.live_credentials.retain(|provider, seen| {
            live_credential_is_fresh(*seen, now_ms) && selected.as_ref() != Some(provider)
        });
        let raise_after_ms = LIVE_CREDENTIAL_TTL.as_millis() as u64 / 3;
        for provider in providers {
            if selected.as_deref() == Some(provider) {
                continue;
            }
            let seen = self
                .live_credentials
                .entry(provider.to_owned())
                .or_default();
            if now_ms.saturating_sub(*seen) >= raise_after_ms {
                *seen = now_ms;
            }
        }
        self.live_credentials != before
    }
}

fn live_credential_is_fresh(seen_ms: u64, now_ms: u64) -> bool {
    now_ms.saturating_sub(seen_ms) < LIVE_CREDENTIAL_TTL.as_millis() as u64
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AccountProfileState {
    pub attempted_at_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<AccountProfileAnswer>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountProfileAnswer {
    pub owner: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_name: Option<String>,
    pub read_at_ms: u64,
}

impl std::fmt::Debug for AccountProfileAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountProfileAnswer")
            .field("owner", &"<redacted>")
            .field("basis", &self.basis)
            .field("plan", &self.plan)
            .field("plan_name", &self.plan_name)
            .field("read_at_ms", &self.read_at_ms)
            .finish()
    }
}

/// A confident probe's login outcome as `ProviderRecord` stores it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordedLogin {
    LoggedIn,
    LoggedOut,
}

/// The last answer a login check got, and when the check that got it started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KnownLogin {
    pub login: RecordedLogin,
    pub at_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderStatus {
    LoggedIn,
    LoggedOut,
    Unavailable,
}

impl ProviderRecord {
    /// The account every display reads: the record's facts, with only the
    /// credentials live sessions run on beside the selected one. A live
    /// credential the inventory does not list shows as a bare entry; a record
    /// with no inventory (an older build's) classifies none.
    pub fn display_account(&self) -> Option<AgentAccount> {
        let mut account = self.account.clone()?;
        if account.credentials.is_empty() {
            return Some(account);
        }
        let now_ms = crate::utils::time::unix_now_ms();
        let live: Vec<&String> = self
            .live_credentials
            .iter()
            .filter(|(_, seen)| live_credential_is_fresh(**seen, now_ms))
            .map(|(provider, _)| provider)
            .collect();
        account
            .credentials
            .retain(|provider, _| live.contains(&provider));
        for provider in live {
            account
                .credentials
                .entry(provider.clone())
                .or_insert_with(|| AgentAccount {
                    sub_provider: Some(provider.clone()),
                    ..AgentAccount::default()
                });
        }
        Some(account)
    }

    /// An older build's `ok` record whose account carries nothing but the CLI
    /// version: it wrote those bytes for a logout and for a login the provider
    /// reported no facts about.
    pub fn login_is_ambiguous(&self) -> bool {
        self.ok
            && self.login.is_none()
            && self.account.as_ref().is_some_and(|account| {
                let facts = AgentAccount {
                    version: None,
                    ..account.clone()
                };
                account.version.is_some() && facts == AgentAccount::default()
            })
    }

    /// The last answer a check got, whether or not the latest attempt failed.
    pub fn known_login(&self) -> Option<KnownLogin> {
        let login = match ProviderStatus::from_record(Some(self)) {
            ProviderStatus::LoggedIn => RecordedLogin::LoggedIn,
            ProviderStatus::LoggedOut => RecordedLogin::LoggedOut,
            ProviderStatus::Unavailable if !self.ok => self.login?,
            ProviderStatus::Unavailable => return None,
        };
        Some(KnownLogin {
            login,
            at_ms: self.login_at_ms.unwrap_or(self.probed_at_ms),
        })
    }
}

impl ProviderStatus {
    /// The recorded login when the probe stored one. A record without one
    /// reads from `ok` and `account` as an older build wrote them, and its
    /// ambiguous shape is unknown rather than a guess.
    pub fn from_record(record: Option<&ProviderRecord>) -> Self {
        let Some(record) = record.filter(|record| record.ok) else {
            return Self::Unavailable;
        };
        match record.login {
            Some(RecordedLogin::LoggedIn) => Self::LoggedIn,
            Some(RecordedLogin::LoggedOut) => Self::LoggedOut,
            None if record.login_is_ambiguous() => Self::Unavailable,
            None if record.account.is_some() => Self::LoggedIn,
            None => Self::LoggedOut,
        }
    }

    /// What launch routing reads: the latest attempt's answer, or the answer
    /// a failed attempt kept while it is no older than `ACCOUNTS_TTL` at
    /// `now_ms`; older, it is unknown.
    pub fn at_launch(record: Option<&ProviderRecord>, now_ms: u64) -> Self {
        let Some(record) = record.filter(|record| !record.ok) else {
            return Self::from_record(record);
        };
        record
            .known_login()
            .filter(|known| now_ms.saturating_sub(known.at_ms) <= ACCOUNTS_TTL.as_millis() as u64)
            .map_or(Self::Unavailable, |known| known.login.into())
    }
}

impl From<RecordedLogin> for ProviderStatus {
    fn from(login: RecordedLogin) -> Self {
        match login {
            RecordedLogin::LoggedIn => Self::LoggedIn,
            RecordedLogin::LoggedOut => Self::LoggedOut,
        }
    }
}

/// Read the producer's published account cache, or an empty cache on a cold,
/// corrupt, or old-schema file. Read-only and fork-free.
pub(crate) fn read_accounts_cache(path: &Path) -> AccountsCache {
    crate::disk::atomic::read_json_cache(path)
}

/// Exact identity of one provider account whose authoritative usage may drive
/// launch-time controls.
///
/// The account key is an opaque, non-secret fingerprint. Its value stays out
/// of `Debug` and user-facing output; equality is the only control operation.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderAccountBinding {
    scope: ProviderAccountScope,
    account_key: String,
}

impl std::fmt::Debug for ProviderAccountBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderAccountBinding")
            .field("scope", &self.scope)
            .field("account_key", &"<redacted>")
            .finish()
    }
}

impl ProviderAccountBinding {
    fn new(scope: ProviderAccountScope, account_key: String) -> Option<Self> {
        (!account_key.trim().is_empty()).then_some(Self { scope, account_key })
    }

    pub fn scope(&self) -> &ProviderAccountScope {
        &self.scope
    }

    pub(crate) fn account_key(&self) -> &str {
        &self.account_key
    }

    #[doc(hidden)]
    #[cfg(any(test, feature = "testkit"))]
    pub fn decode(value: &str) -> Option<Self> {
        serde_json::from_str(value)
            .ok()
            .filter(|binding: &Self| !binding.account_key.trim().is_empty())
    }

    fn display_label(&self, kind: &str) -> String {
        if let Some(label) =
            find_definition(kind).and_then(|adapter| adapter.account_label(&self.scope))
        {
            return label;
        }
        match self.scope.sub_provider_parts() {
            Some((provider, variant)) => format!("{kind} {provider} {variant}"),
            None => kind.to_owned(),
        }
    }
}

/// Account-binding resolution for one managed launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManagedLaunchState {
    /// Final cwd, environment, model, and argv still need compilation.
    PendingResolution,
    /// This adapter has no exact managed-account selection.
    Unsupported,
    /// Exact selection applies, but current inputs prove no account.
    Unresolved,
    /// Current inputs prove one exact provider account.
    Bound(ProviderAccountBinding),
}

impl ManagedLaunchState {
    pub fn binding(&self) -> Option<&ProviderAccountBinding> {
        match self {
            Self::Bound(binding) => Some(binding),
            Self::PendingResolution | Self::Unsupported | Self::Unresolved => None,
        }
    }

    pub(crate) fn exact_account_applies(&self) -> bool {
        matches!(self, Self::Unresolved | Self::Bound(_))
    }
}

/// Cache identity of the credentials behind one provider usage probe.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountUsageIdentity {
    pub scope: ProviderAccountScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials_stamp: Option<u64>,
}

impl AccountUsageIdentity {
    pub(super) fn binding(&self) -> Option<ProviderAccountBinding> {
        ProviderAccountBinding::new(self.scope.clone(), self.account_key.clone()?)
    }
}

/// Provider-account subscription capacity for one agent kind. Kind-wide
/// windows drive general harness policy; exact bindings opt a managed launch
/// into account-specific controls. Named and durationless quotas remain
/// display data in the cache. The login's model refusals ride along, so every
/// reader vetoes a refused model as it vetoes a spent model sub-cap.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProviderCapacity {
    pub(crate) windows: Vec<RateLimitWindow>,
    pacing_max_mins: Option<u32>,
    refusals: BTreeMap<String, ModelRefusal>,
}

impl ProviderCapacity {
    /// Read one kind-wide capacity from the shared provider cache. The login's model refusals
    /// load apart from its usage entry, so a refusal still vetoes while that entry is missing.
    /// A window observed before the login's identity floor describes the previous account and
    /// is left out; an unstamped one stays.
    pub fn read(runtime: &RuntimePaths, key: &LoginKey) -> Option<Self> {
        let cache = read_rate_limits_cache(&RateLimitsCache::shared_path(runtime));
        let floor_ms = identity_floor_ms(runtime, key);
        let windows = cache
            .entries
            .get(&key.clone().into())
            .filter(|entry| entry.scope.is_kind_wide())
            .map(|entry| since_floor(entry.limits.windows.clone(), floor_ms));
        let refusals = ModelRefusals::read(&runtime.shared_model_refusals_path()).login(key);
        if windows.is_none() && refusals.is_empty() {
            return None;
        }
        Some(Self {
            windows: windows.unwrap_or_default(),
            pacing_max_mins: None,
            refusals,
        })
    }

    /// Read capacity only when the cache belongs to this exact provider
    /// account. Bound Qwen pacing uses its sliding 5-hour/7-day windows while
    /// all authoritative windows remain available to the exhaustion gate.
    /// Windows from before the login's identity floor are left out, as in
    /// [`Self::read`].
    fn read_bound(
        runtime: &RuntimePaths,
        key: &LoginKey,
        binding: &ProviderAccountBinding,
    ) -> Option<Self> {
        let cache = read_rate_limits_cache(&RateLimitsCache::shared_path(runtime));
        let entry = cache
            .entries
            .get(&AccountUsageKey::new(key.clone(), binding.scope.clone()))?;
        entry_matches_binding(entry, binding).then(|| Self {
            windows: since_floor(
                entry
                    .bound_limits
                    .as_ref()
                    .unwrap_or(&entry.limits)
                    .windows
                    .clone(),
                identity_floor_ms(runtime, key),
            ),
            pacing_max_mins: Some(7 * 24 * 60),
            refusals: BTreeMap::new(),
        })
    }

    /// Read all kind-wide capacities from the shared provider cache once, plus a refusal-only
    /// capacity for each login in use whose refusals stand without a usage entry. Windows from
    /// before a login's identity floor are left out, as in [`Self::read`].
    pub(crate) fn read_all(
        runtime: &RuntimePaths,
        logins: &RoomLoginSet,
    ) -> BTreeMap<LoginKey, Self> {
        let in_use = logins.keys_in_use();
        let mut refusals = ModelRefusals::read(&runtime.shared_model_refusals_path()).logins;
        let accounts = read_accounts_cache(&runtime.shared_accounts_path());
        let mut capacities: BTreeMap<_, _> =
            read_rate_limits_cache(&RateLimitsCache::shared_path(runtime))
                .entries
                .into_iter()
                .filter(|(key, entry)| {
                    key.scope.is_kind_wide()
                        && entry.scope.is_kind_wide()
                        && in_use.contains(&key.login)
                })
                .map(|(key, entry)| {
                    let refusals = refusals.remove(&key.login).unwrap_or_default();
                    let floor_ms = accounts
                        .logins
                        .get(&key.login)
                        .and_then(|record| record.stamps.floor_at_ms);
                    (
                        key.login,
                        Self {
                            windows: since_floor(entry.limits.windows, floor_ms),
                            pacing_max_mins: None,
                            refusals,
                        },
                    )
                })
                .collect();
        // A login with refusals and no usage entry still carries its veto.
        for (login, refusals) in refusals {
            if in_use.contains(&login) {
                capacities.insert(
                    login,
                    Self {
                        refusals,
                        ..Self::default()
                    },
                );
            }
        }
        capacities
    }

    pub(crate) fn from_windows(windows: Vec<RateLimitWindow>) -> Self {
        Self {
            windows,
            pacing_max_mins: None,
            refusals: BTreeMap::new(),
        }
    }

    /// This reading with `model`'s refusal recorded at `at`, as a test fixture writes it.
    #[cfg(test)]
    pub(crate) fn with_refusal(mut self, model: &str, at: Timestamp, label: &str) -> Self {
        self.refusals.insert(
            super::model_refusal::model_key(model),
            ModelRefusal {
                refused_at: at,
                succeeded_at: None,
                label: Some(label.to_owned()),
            },
        );
        self
    }

    /// The login's record for `model`, standing or cleared. A version-less key (a bare family
    /// alias such as `fable`) stands for whichever version the provider serves, on either side:
    /// a version-less name reads its own entry and every recorded version of its family, and a
    /// versioned name its own entry and its family's version-less one. Among those the standing
    /// record wins, then the newest refusal; the exact entry gets no precedence.
    pub(crate) fn refusal(&self, model: Option<&str>) -> Option<&ModelRefusal> {
        let key = super::model_refusal::parsed_model_key(model?)?;
        self.refusals
            .iter()
            .filter(|(recorded, _)| {
                super::model_display::ParsedModel::parse(recorded)
                    .match_key()
                    .is_some_and(|recorded| recorded.covers(&key))
            })
            .map(|(_, refusal)| refusal)
            .max_by_key(|refusal| (refusal.stands(), refusal.refused_at))
    }

    /// Whether the provider refuses `model` on this login: its record stands.
    pub fn model_refused(&self, model: Option<&str>) -> bool {
        self.refusal(model).is_some_and(ModelRefusal::stands)
    }

    pub(crate) fn projected_windows(
        &self,
        now: Timestamp,
    ) -> impl Iterator<Item = RateLimitWindow> + '_ {
        self.windows
            .iter()
            .cloned()
            .map(move |window| window.projected_at(now))
    }

    /// The unscoped window of `span`, projected at `now`.
    /// Model sub-caps share their parent's duration and never match; of two
    /// unscoped windows with one span, the last is read, as the longest-window
    /// choice reads it.
    pub(crate) fn window_of_span(
        &self,
        span: WindowSpan,
        now: Timestamp,
    ) -> Option<RateLimitWindow> {
        self.windows
            .iter()
            .rfind(|window| window.scope.is_none() && window.duration_mins == Some(span.minutes()))
            .cloned()
            .map(|window| window.projected_at(now))
    }

    /// Current observation of the longest duration-bearing window.
    pub(crate) fn longest_window_observation(&self, now: Timestamp) -> Option<RateLimitWindow> {
        Some(self.duration_window()?.clone().projected_at(now))
    }

    /// Latest reset among subscription windows spent now with a future reset.
    pub fn latest_spent_window_reset(&self, now: Timestamp) -> Option<Timestamp> {
        self.latest_spent_window(now)?.resets_at
    }

    /// The subscription window spent now whose future reset is latest.
    pub(crate) fn latest_spent_window(&self, now: Timestamp) -> Option<RateLimitWindow> {
        self.projected_duration_windows(now)
            .filter(|window| window.spent_with_future_reset(now))
            .max_by_key(|window| window.resets_at)
    }

    /// Whether every subscription window is lifted, reads 0%, or has not
    /// started, since a fresh window reports about 1%. A lifted window has no
    /// enforced clock a redemption could start. An enforced window without a
    /// percentage, or no window at all, is unknown rather than unused.
    pub(crate) fn known_unused(&self, now: Timestamp) -> bool {
        let mut windows = self.projected_duration_windows(now).peekable();
        windows.peek().is_some()
            && windows.all(|window| {
                window.lifted
                    || window
                        .used_percentage
                        .is_some_and(|used| used == 0 || window.not_started(now))
            })
    }

    /// Latest reset of the account windows and this model's sub-caps.
    pub fn latest_spent_window_reset_for_model(
        &self,
        now: Timestamp,
        model: Option<&str>,
    ) -> Option<Timestamp> {
        self.model_windows(model)
            .filter(|window| window.is_spent())
            .filter_map(|window| window.resets_at.filter(|reset| *reset > now))
            .chain(self.latest_spent_window_reset(now))
            .max()
    }

    /// Model sub-caps can veto otherwise available account capacity.
    pub(crate) fn subscription_budget_available_for_model(
        &self,
        now: Timestamp,
        model: Option<&str>,
    ) -> bool {
        self.subscription_budget_available(now)
            && !self.model_refused(model)
            && self
                .model_windows(model)
                .all(|window| !window_spent_unreset(window, now))
    }

    /// Whether this reading of `agent`'s own login has a spent window blocking its model at
    /// `now`, the test the keep-warm ping reads: a 5-hour or weekly window at 100% with its reset
    /// ahead or unknown, or a spent sub-cap or a standing refusal for the agent's current model
    /// ([`super::AgentState::current_model`]). Paid extra usage is not read,
    /// so a window it would carry still counts as spent. A window with no known percentage is
    /// not spent.
    pub(crate) fn window_spent(&self, agent: &super::AgentState, now: Timestamp) -> bool {
        self.subscription_exhausted_for_model(now, agent.current_model())
    }

    /// [`ProviderCapacity::window_spent`] on `agent`'s login, read now from `runtime`'s shared
    /// capacity cache: the recheck the ping helper asks before it pings. No reading, or an
    /// unreadable cache, is not spent.
    pub(crate) fn login_window_spent(
        runtime: &RuntimePaths,
        agent: &super::AgentState,
        now: Timestamp,
    ) -> bool {
        Self::read(runtime, &agent.login_key())
            .is_some_and(|capacity| capacity.window_spent(agent, now))
    }

    /// Whether this reading of `agent`'s login holds a compaction back at `now`, the test idle
    /// compaction, smart compaction and compact-first read: the agent's model sub-cap is spent,
    /// or a 5-hour or weekly window is spent (as [`ProviderCapacity::window_spent`] reads it) and
    /// `paid_carries`, asked only then, finds no usable paid extra usage on the login. Unlike the
    /// ping's test, usable paid extra usage lets the compaction go.
    pub(crate) fn compaction_blocked(
        &self,
        agent: &super::AgentState,
        now: Timestamp,
        paid_carries: impl FnOnce() -> bool,
    ) -> bool {
        self.model_exhausted(now, agent.current_model())
            || (self.subscription_exhausted_for_model(now, None) && !paid_carries())
    }

    /// `ProviderCapacity::compaction_blocked` on `agent`'s login, read now from `runtime`'s
    /// shared capacity and credits caches: the recheck a compaction helper asks before it
    /// compacts, and compact-first asks before it types its command. No capacity reading is not
    /// blocked; no credits entry, an unreadable credits cache, or a lapsed entitlement carries
    /// nothing.
    pub fn login_compaction_blocked(
        runtime: &RuntimePaths,
        agent: &super::AgentState,
        now: Timestamp,
    ) -> bool {
        let login = agent.login_key();
        Self::read(runtime, &login).is_some_and(|capacity| {
            capacity.compaction_blocked(agent, now, || {
                super::credits::read_credits_cache(&runtime.shared_credits_path())
                    .paid_usage_carries(&login, now)
            })
        })
    }

    pub(crate) fn subscription_exhausted_for_model(
        &self,
        now: Timestamp,
        model: Option<&str>,
    ) -> bool {
        self.duration_windows()
            .any(|window| window_spent_unreset(window, now))
            || self.model_exhausted(now, model)
    }

    /// Whether `model` cannot run on this login now: a spent sub-cap, or a standing refusal.
    pub(crate) fn model_exhausted(&self, now: Timestamp, model: Option<&str>) -> bool {
        self.model_refused(model)
            || self
                .model_windows(model)
                .any(|window| window_spent_unreset(window, now))
    }

    /// Whether a usage read confirms that the provider refuses `model` on this login, from a
    /// limit `marker` that names no refill: every covering window (the unscoped duration
    /// windows and the model's sub-caps) was observed after the marker's `at` and no later than
    /// [`super::model_refusal::CONFIRM_WINDOW`] after it, none reads spent, and none was
    /// renewed after `at` (`renewed_after`), a window the stopped turn never ran in: its reset
    /// crossed between `at` and the read, or a started sliding window that began after `at`
    /// (a fixed period's start is not dated). A marker naming its refill is a quota stop, and a
    /// login with no windows never confirms.
    pub(crate) fn confirms_refusal(&self, model: &str, marker: &AgentTurnError) -> bool {
        let at = marker.at;
        let Ok(deadline) = at.checked_add(super::model_refusal::CONFIRM_WINDOW) else {
            return false;
        };
        if marker.refill_at.is_some() {
            return false;
        }
        let mut covering = self.covering_windows(Some(model)).peekable();
        covering.peek().is_some()
            && covering.all(|window| {
                !window.is_spent()
                    && window.observed_at.is_some_and(|observed| {
                        observed > at
                            && observed <= deadline
                            && !renewed_after(window, observed, at)
                    })
            })
    }

    /// Whether a usage read has seen this login for `model` since `at`: every covering window
    /// was observed after it. `None` when the login has no covering window to read; with no
    /// model, the unscoped windows alone cover.
    pub(crate) fn read_since(&self, model: Option<&str>, at: Timestamp) -> Option<bool> {
        let mut covering = self.covering_windows(model).peekable();
        covering.peek()?;
        Some(covering.all(|window| window.observed_at.is_some_and(|observed| observed > at)))
    }

    /// The windows a request on `model` spends: the unscoped duration windows and the model's
    /// sub-caps.
    fn covering_windows(&self, model: Option<&str>) -> impl Iterator<Item = &RateLimitWindow> {
        self.duration_windows().chain(self.model_windows(model))
    }

    fn model_windows(&self, model: Option<&str>) -> impl Iterator<Item = &RateLimitWindow> {
        let model = model.and_then(|model| {
            crate::agents::model_display::ParsedModel::parse(launch_model_id(model)).match_key()
        });
        self.windows.iter().filter(move |window| {
            let Some(scope) = window
                .scope
                .as_ref()
                .and_then(|scope| scope.id.strip_prefix("model:"))
                .and_then(|scope| {
                    crate::agents::model_display::ParsedModel::parse(scope).match_key()
                })
            else {
                return false;
            };
            window.duration_mins.is_some_and(|mins| mins > 0)
                && model
                    .as_ref()
                    .is_some_and(|model| model.matches_scope(&scope))
        })
    }

    /// Whether a known subscription reading currently has capacity.
    fn subscription_budget_available(&self, now: Timestamp) -> bool {
        let mut has_known_available = false;
        for window in self.projected_duration_windows(now) {
            if window_spent_unreset(&window, now) {
                return false;
            }
            has_known_available |= window.used_percentage.is_some() && !window.is_spent();
        }
        has_known_available
    }

    /// Earliest future reset among currently exhausted authoritative windows.
    fn spent_window(&self, now: Timestamp) -> Option<RateLimitWindow> {
        self.windows
            .iter()
            .filter(|window| {
                window.scope.is_none()
                    && window.is_spent()
                    && window.resets_at.is_some_and(|reset| reset > now)
            })
            .min_by_key(|window| window.resets_at)
            .cloned()
    }

    fn duration_window(&self) -> Option<&RateLimitWindow> {
        self.duration_windows()
            .max_by_key(|window| window.duration_mins)
    }

    fn duration_windows(&self) -> impl Iterator<Item = &RateLimitWindow> {
        self.windows.iter().filter(|window| {
            window.scope.is_none()
                && window.duration_mins.is_some_and(|mins| {
                    mins > 0 && self.pacing_max_mins.is_none_or(|maximum| mins <= maximum)
                })
        })
    }

    fn projected_duration_windows(
        &self,
        now: Timestamp,
    ) -> impl Iterator<Item = RateLimitWindow> + '_ {
        self.duration_windows()
            .cloned()
            .map(move |window| window.projected_at(now))
    }
}

impl super::AgentState {
    /// When the limit this agent stopped on refills, ahead of `now`: the latest of the refill
    /// its displayed stop states ([`AgentTurnError::refill_at`]) and the resets of `model`'s
    /// spent windows in `capacity`
    /// ([`ProviderCapacity::latest_spent_window_reset_for_model`]). A stated refill needs no
    /// usage reading and counts whatever the agent's model, while its stop is displayed.
    pub fn limit_refill(
        &self,
        capacity: Option<&ProviderCapacity>,
        model: Option<&str>,
        now: Timestamp,
    ) -> Option<Timestamp> {
        let stated = self
            .displayed_turn_error(now)
            .and_then(|(_, marker)| marker.refill_at)
            .filter(|refill| *refill > now);
        let spent =
            capacity.and_then(|capacity| capacity.latest_spent_window_reset_for_model(now, model));
        stated.max(spent)
    }

    /// Whether the refill this agent's displayed stop states has passed and `capacity` does not
    /// show `model` blocked ([`ProviderCapacity::subscription_exhausted_for_model`]: a spent
    /// window whose reset is ahead or unknown, a spent sub-cap, or a standing refusal). A reading
    /// with no known-available window blocks nothing, and neither does no reading.
    pub(crate) fn stated_refill_lifted(
        &self,
        capacity: Option<&ProviderCapacity>,
        model: Option<&str>,
        now: Timestamp,
    ) -> bool {
        self.displayed_turn_error(now)
            .and_then(|(_, marker)| marker.refill_at)
            .is_some_and(|refill| refill <= now)
            && capacity
                .is_none_or(|capacity| !capacity.subscription_exhausted_for_model(now, model))
    }
}

/// The model id a catalog launch alias names (`opus`), or `model` itself.
pub(super) fn launch_model_id(model: &str) -> &str {
    super::all_definitions()
        .flat_map(|definition| definition.spec().launch.definitions.models)
        .find(|entry| entry.name == model)
        .map_or(model, |entry| entry.id)
}

/// The instant (Unix ms) of the login's last identity switch or logout.
fn identity_floor_ms(runtime: &RuntimePaths, key: &LoginKey) -> Option<u64> {
    read_accounts_cache(&runtime.shared_accounts_path())
        .logins
        .get(key)
        .and_then(|record| record.stamps.floor_at_ms)
}

/// `windows` without those observed before `floor_ms`, which describe the
/// previous account; an unstamped window stays.
fn since_floor(mut windows: Vec<RateLimitWindow>, floor_ms: Option<u64>) -> Vec<RateLimitWindow> {
    windows.retain(|window| {
        floor_ms
            .zip(window.observed_at)
            .is_none_or(|(floor, observed)| observed.as_millisecond() >= floor as i64)
    });
    windows
}

fn entry_matches_binding(entry: &RateLimitCacheEntry, binding: &ProviderAccountBinding) -> bool {
    &entry.scope == binding.scope() && entry.account_key.as_deref() == Some(binding.account_key())
}

#[doc(hidden)]
pub fn provider_budget_gate(
    runtime: &RuntimePaths,
    key: &LoginKey,
    binding: &ProviderAccountBinding,
    now: Timestamp,
) -> Option<String> {
    let window = ProviderCapacity::read_bound(runtime, key, binding)?.spent_window(now)?;
    let reset = window.resets_at?;
    let used_percentage = window.used_percentage.unwrap_or(100);
    let window_label = window
        .duration_mins
        .map(crate::theme::fmt::window_duration_label)
        .unwrap_or_else(|| "quota".to_owned());
    Some(format!(
        "{} {window_label} window exhausted ({used_percentage}% used); resets at {reset}",
        binding.display_label(key.kind.as_str()),
    ))
}

/// A subscription window length that loop triggers name: `5h` or `7d`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum WindowSpan {
    FiveHour,
    SevenDay,
}

impl WindowSpan {
    pub const fn minutes(self) -> u32 {
        match self {
            Self::FiveHour => 5 * 60,
            Self::SevenDay => 7 * 24 * 60,
        }
    }
}

impl std::fmt::Display for WindowSpan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::FiveHour => "5h",
            Self::SevenDay => "7d",
        })
    }
}

impl std::str::FromStr for WindowSpan {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw {
            "5h" => Ok(Self::FiveHour),
            "7d" => Ok(Self::SevenDay),
            other => Err(format!("unknown window span `{other}`; use 5h or 7d")),
        }
    }
}

fn window_spent_unreset(window: &RateLimitWindow, now: Timestamp) -> bool {
    window.is_spent() && window.resets_at.is_none_or(|reset| reset > now)
}

/// Whether `window`, read at `observed`, is a window renewed after `at`: its reset falls after
/// `at` and no later than the read, so it refilled in between, or it is a started sliding window
/// whose reset minus its duration is later than `at`. A window that has not started cannot be
/// dated by its start (the provider slides its reset ahead until the first billable token), nor
/// can a fixed period (its nominal duration is not its length) or a window with no duration, so
/// none of these reads as renewed unless its reset was crossed.
fn renewed_after(window: &RateLimitWindow, observed: Timestamp, at: Timestamp) -> bool {
    let Some(reset) = window.resets_at else {
        return false;
    };
    if reset > at && reset <= observed {
        return true;
    }
    let Some(mins) = window.duration_mins else {
        return false;
    };
    !window.fixed_period
        && !window.not_started(observed)
        && reset
            .checked_sub(jiff::SignedDuration::from_secs(i64::from(mins) * 60))
            .is_ok_and(|start| start > at)
}

/// Producer-published per-provider rate-limit windows.
const RATE_LIMITS_CACHE_VERSION: u32 = 7;

/// One credential's usage under a login; kind-wide keys keep the login's text.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AccountUsageKey {
    pub login: LoginKey,
    pub scope: ProviderAccountScope,
}

#[derive(Debug, thiserror::Error)]
#[error("invalid usage key `{0}`; expected `<kind>@<name>[/<provider>/<variant>]`")]
pub struct InvalidAccountUsageKey(String);

impl AccountUsageKey {
    pub fn new(login: LoginKey, scope: ProviderAccountScope) -> Self {
        Self { login, scope }
    }

    pub(super) fn deserialize_map<'de, D, V>(deserializer: D) -> Result<BTreeMap<Self, V>, D::Error>
    where
        D: serde::Deserializer<'de>,
        V: Deserialize<'de>,
    {
        let entries = BTreeMap::<String, V>::deserialize(deserializer)?;
        Ok(entries
            .into_iter()
            .filter_map(|(key, value)| Some((key.parse().ok()?, value)))
            .collect())
    }
}

impl From<LoginKey> for AccountUsageKey {
    fn from(login: LoginKey) -> Self {
        Self::new(login, ProviderAccountScope::KindWide)
    }
}

impl std::str::FromStr for AccountUsageKey {
    type Err = InvalidAccountUsageKey;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut parts = value.split('/');
        let invalid = || InvalidAccountUsageKey(value.to_owned());
        let login = parts
            .next()
            .ok_or_else(invalid)?
            .parse()
            .map_err(|_| invalid())?;
        let scope = match (parts.next(), parts.next(), parts.next()) {
            (None, None, None) => ProviderAccountScope::KindWide,
            (Some(provider), Some(variant), None)
                if !provider.is_empty() && !variant.is_empty() =>
            {
                ProviderAccountScope::sub_provider(provider, variant)
            }
            _ => return Err(invalid()),
        };
        Ok(Self::new(login, scope))
    }
}

impl TryFrom<String> for AccountUsageKey {
    type Error = InvalidAccountUsageKey;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<AccountUsageKey> for String {
    fn from(value: AccountUsageKey) -> Self {
        value.to_string()
    }
}

impl std::fmt::Display for AccountUsageKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.login)?;
        if let ProviderAccountScope::SubProvider { provider, variant } = &self.scope {
            write!(f, "/{provider}/{variant}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitsCache {
    pub version: u32,
    pub refreshed_at_ms: u64,
    #[serde(
        default,
        deserialize_with = "AccountUsageKey::deserialize_map",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub entries: BTreeMap<AccountUsageKey, RateLimitCacheEntry>,
}

impl RateLimitsCache {
    /// Shared cache file for this build's rate-limit format.
    ///
    /// Keep incompatible builds on separate files so their whole-cache writes
    /// cannot erase one another's entries.
    pub fn shared_path(runtime: &RuntimePaths) -> PathBuf {
        runtime.shared_rate_limits_path(RATE_LIMITS_CACHE_VERSION)
    }

    pub(crate) fn shared_lock(runtime: &RuntimePaths) -> PathBuf {
        runtime.shared_rate_limits_lock(RATE_LIMITS_CACHE_VERSION)
    }
}

impl Default for RateLimitsCache {
    fn default() -> Self {
        Self {
            version: RATE_LIMITS_CACHE_VERSION,
            refreshed_at_ms: 0,
            entries: BTreeMap::new(),
        }
    }
}

#[derive(Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateLimitCacheEntry {
    /// Last early reset spotted in an authoritative account reading, in Unix ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_spotted_at_ms: Option<u64>,
    #[serde(default)]
    pub scope: ProviderAccountScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_key: Option<String>,
    #[serde(default)]
    pub limits: AgentRateLimits,
    /// Authoritative windows tied to `account_key` for exact-account controls.
    /// Display readers use fused `limits`; bound controls prefer this copy so a
    /// scope-only live reading cannot acquire the cached credential identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_limits: Option<AgentRateLimits>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<PendingRefill>,
    /// When this credential's display first went unknown, marking the open episode so
    /// the authoritative refresh it forces runs once rather than every frame.
    /// Cleared the moment a usable window paints again, re-arming the next
    /// episode. Absent in caches written before the marker existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown_since_ms: Option<u64>,
}

impl std::fmt::Debug for RateLimitCacheEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RateLimitCacheEntry")
            .field("reset_spotted_at_ms", &self.reset_spotted_at_ms)
            .field("scope", &self.scope)
            .field(
                "account_key",
                &self.account_key.as_ref().map(|_| "<redacted>"),
            )
            .field("limits", &self.limits)
            .field("bound_limits", &self.bound_limits)
            .field("pending", &self.pending)
            .field("unknown_since_ms", &self.unknown_since_ms)
            .finish()
    }
}

/// A best-effort refill awaiting confirmation by rate-limit fusion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingRefill {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_mins: Option<u32>,
    pub used_percentage: u8,
    pub first_seen_at: Timestamp,
}

impl PendingRefill {
    pub(crate) fn key(&self) -> RateLimitWindowKey {
        self.scope_id.as_ref().map_or_else(
            || RateLimitWindowKey::Duration(self.duration_mins),
            |scope| RateLimitWindowKey::Scope(scope.clone()),
        )
    }
}

/// Read the provider capacity cache, cold-dropping corrupt or unknown versions.
pub(crate) fn read_rate_limits_cache(path: &Path) -> RateLimitsCache {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .filter(|cache: &RateLimitsCache| cache.version == RATE_LIMITS_CACHE_VERSION)
        .unwrap_or_default()
}

/// Latest cached window observation since the login's identity floor, falling
/// back to the credits reading.
pub fn cached_usage_read_at(runtime: &RuntimePaths, key: &LoginKey) -> Option<Timestamp> {
    let floor_ms = identity_floor_ms(runtime, key);
    read_rate_limits_cache(&RateLimitsCache::shared_path(runtime))
        .entries
        .into_iter()
        .filter(|(usage_key, _)| usage_key.login == *key)
        .flat_map(|(_, entry)| {
            since_floor(entry.limits.windows, floor_ms)
                .into_iter()
                .filter_map(|window| window.observed_at)
        })
        .max()
        .or_else(|| {
            let cache = crate::agents::credits::read_credits_cache(&runtime.shared_credits_path());
            cache
                .logins
                .get(key)
                .into_iter()
                .chain(
                    cache
                        .scoped
                        .iter()
                        .filter(|(usage_key, _)| usage_key.login == *key)
                        .map(|(_, entry)| entry),
                )
                .filter_map(|entry| i64::try_from(entry.observed_at_ms).ok())
                .filter_map(|ms| Timestamp::from_millisecond(ms).ok())
                .max()
        })
}

/// Best-effort credential-file mtime for provider usage ranking.
pub(super) fn file_mtime_ms(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
}

/// The outcome of an out-of-band account probe. A settled `Found` or `LoggedOut`
/// answer rides the producer's long success TTL. A provisional `Found` retries
/// on the short TTL. `Unavailable` backs off from the short TTL to the success
/// TTL across consecutive failures, retaining the last-known account facts.
#[derive(Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "account facts move directly into the cache without another allocation"
)]
pub enum AccountProbe {
    /// A logged-in account with the identity, plan, and metering facts its
    /// provider exposes.
    Found(super::AgentAccount),
    /// The probe ran and authoritatively found no login (logged out, or an auth
    /// file naming no credential). Cache it like a success: it changes about never.
    LoggedOut,
    /// The probe could not complete — the binary is missing, it exited non-zero,
    /// or its file was unreadable. Back off; absence here is not logged-out.
    Unavailable(AccountProbeError),
}

/// Secret-free cause of an unsuccessful account probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountProbeError {
    SpawnFailed,
    TimedOut,
    ExitNonZero(Option<i32>),
    UnparseableOutput,
    ReadFailed,
    SourceUnavailable,
}

impl std::fmt::Display for AccountProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SpawnFailed => f.write_str("could not start"),
            Self::TimedOut => f.write_str("timed out"),
            Self::ExitNonZero(Some(code)) => write!(f, "exited {code}"),
            Self::ExitNonZero(None) => f.write_str("terminated by signal"),
            Self::UnparseableOutput => f.write_str("unparseable output"),
            Self::ReadFailed => f.write_str("could not read credentials"),
            Self::SourceUnavailable => f.write_str("account source unavailable"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RedemptionCode {
    Reset,
    NothingToReset,
    NoCredit,
    AlreadyRedeemed,
    Unknown,
    Cooldown,
}

impl RedemptionCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reset => "reset",
            Self::NothingToReset => "nothing_to_reset",
            Self::NoCredit => "no_credit",
            Self::AlreadyRedeemed => "already_redeemed",
            Self::Unknown => "unknown",
            Self::Cooldown => "cooldown",
        }
    }
}

pub struct ResetCreditOffer {
    pub capacity: Option<ProviderCapacity>,
    pub credits: super::ResetCredits,
    pub extra_credits: Option<super::ExtraCredits>,
    pub hold: Option<RedeemHold>,
    action: Box<dyn ResetCreditAction>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedeemHold {
    pub code: RedemptionCode,
    pub reason: String,
}

impl ResetCreditOffer {
    pub fn new(
        capacity: Option<ProviderCapacity>,
        credits: super::ResetCredits,
        extra_credits: Option<super::ExtraCredits>,
        action: impl ResetCreditAction + 'static,
    ) -> Self {
        Self {
            capacity,
            credits,
            extra_credits,
            hold: None,
            action: Box::new(action),
        }
    }

    /// Spend the provider-owned credit only after the caller has durably
    /// recorded its attempt.
    pub(crate) fn consume(self, request_id: &str) -> Result<ResetCreditResult, String> {
        self.action.consume(request_id)
    }
}

pub trait ResetCreditAction: Send {
    fn consume(self: Box<Self>, request_id: &str) -> Result<ResetCreditResult, String>;
}

pub struct ResetCreditResult {
    pub outcome: RedemptionCode,
    pub windows_reset: i64,
    pub refreshed: Option<(super::AccountUsageIdentity, super::AccountUsageSnapshot)>,
    pub refresh_error: Option<String>,
}

/// Read `kind`'s provider-owned reset-credit offer. The caller judges it and
/// owns the durable reservation that must precede its `consume`.
pub(crate) fn prepare_reset_credit_redemption(
    kind: &str,
    login_env: &std::collections::BTreeMap<String, String>,
) -> Result<ResetCreditOffer, String> {
    super::find_definition(kind)
        .ok_or_else(|| format!("{kind} does not support reset-credit redemption"))?
        .prepare_reset_credit(login_env)
}
