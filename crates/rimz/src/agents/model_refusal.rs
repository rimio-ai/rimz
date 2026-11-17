//! Models a provider refuses on one account with no reset coming.
//!
//! A refusal record is keyed by login and model and lives in the machine-wide
//! `model_refusals.v1.json` provider cache beside the rate-limit windows. The
//! room host's heavy pass writes it (`sidebar::refresh`) once a usage read
//! shows the account unspent after a limit marker, and raises its success
//! stamp on the first clean turn on that model and account after it.
//! [`super::ProviderCapacity`] loads a login's records, so every capacity
//! reader (pings, compaction, launch routing, restart, auto-continue) vetoes
//! the model while its record stands.
//!
//! Every stamp only rises, so writers that each read, raise, and write under
//! the file's lock end equal in either order. A missing or unreadable file
//! reads as no records, and entries are never deleted.

use std::collections::BTreeMap;
use std::path::Path;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::ids::LoginKey;

/// One model's refusal on one login.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRefusal {
    /// The newest limit marker the refusal was recorded or raised from.
    pub refused_at: Timestamp,
    /// The start of the newest clean turn on this model and login after a refusal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub succeeded_at: Option<Timestamp>,
    /// The provider's own sentence, from the marker that last raised `refused_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl ModelRefusal {
    /// Whether no clean turn has started since the newest refusal.
    pub fn stands(&self) -> bool {
        self.succeeded_at
            .is_none_or(|succeeded| self.refused_at >= succeeded)
    }
}

/// Every recorded refusal, by login and then by [`model_key`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ModelRefusals {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) logins: BTreeMap<LoginKey, BTreeMap<String, ModelRefusal>>,
}

impl ModelRefusals {
    pub(crate) fn read(path: &Path) -> Self {
        crate::disk::atomic::read_json_cache(path)
    }

    pub(crate) fn login(&self, login: &LoginKey) -> BTreeMap<String, ModelRefusal> {
        self.logins.get(login).cloned().unwrap_or_default()
    }
}

/// Whether `key` names a version of `family`, a version-less key such as `fable`: the
/// family's name, a space, then a version number (`fable 5.2`).
pub(crate) fn is_version_of(key: &str, family: &str) -> bool {
    key.strip_prefix(family)
        .and_then(|rest| rest.strip_prefix(' '))
        .is_some_and(|version| version.starts_with(|c: char| c.is_ascii_digit()))
}

/// The key a refusal is recorded under: a launch alias resolved to its model
/// id, then the model's display name, lowercased. A slug, a display name and
/// a name with a context-window suffix share one key; sibling versions do not.
pub fn model_key(model: &str) -> String {
    super::model_display::model_label_parts(super::account::launch_model_id(model.trim()))
        .0
        .to_lowercase()
}
