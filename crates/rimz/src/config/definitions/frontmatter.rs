//! Strict YAML frontmatter and the fields inherited along definition chains.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Deserializer, de::IgnoredAny};

use crate::agents::PermissionMode;
use crate::config::{IdleCompact, IdleCompactParseError};

use super::DefinitionErr;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(super) struct AgentFrontmatter {
    #[serde(rename = "name")]
    _name: Option<String>,
    pub agent: Option<String>,
    pub isolation: Option<crate::config::Isolation>,
    pub empty_dir: Option<bool>,
    pub description: Option<String>,
    pub model: Option<String>,
    pub tier: Option<String>,
    pub mode: Option<PermissionMode>,
    pub effort: Option<String>,
    pub fast: Option<crate::agents::Toggle>,
    pub code_mode: Option<crate::agents::Toggle>,
    #[serde(default, deserialize_with = "token_count")]
    pub auto_compact: Option<String>,
    #[serde(default, deserialize_with = "number_text")]
    pub budget: Option<String>,
    #[serde(default, deserialize_with = "number_text")]
    pub timeout: Option<String>,
    pub keep_warm: Option<crate::config::KeepWarmPolicy>,
    #[serde(default, deserialize_with = "idle_compact")]
    pub idle_compact: Option<IdleCompact>,
    #[serde(default, deserialize_with = "smart_compact")]
    pub smart_compact: Option<crate::config::FlipCompact>,
    pub model_reminder: Option<bool>,
    #[serde(default, deserialize_with = "list")]
    pub traits: Option<Vec<String>>,
    #[serde(default, deserialize_with = "list")]
    pub tools: Option<Vec<String>>,
    #[serde(default, deserialize_with = "list")]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default, deserialize_with = "list")]
    pub subagents: Option<Vec<String>>,
    #[serde(default, deserialize_with = "list")]
    pub skills: Option<Vec<String>>,
    #[serde(default, deserialize_with = "list")]
    pub mcps: Option<Vec<String>>,
}

/// Whether a list entry adds to the inherited list rather than replacing it.
fn is_plus(entry: &str) -> bool {
    entry.trim_start().starts_with('+')
}

/// Fills `list` from `below` when unset. A `+` list adds its entries to `below`: an entry
/// whose `key` `below` already holds replaces it in place, and the rest follow in written
/// order. Over a `+` list the result is still `+`, pending the next list further down.
fn merge_list(list: &mut Option<Vec<String>>, below: Option<&Vec<String>>, key: fn(&str) -> &str) {
    let Some(below) = below else { return };
    let Some(added) = list
        .as_ref()
        .filter(|list| list.iter().any(|entry| is_plus(entry)))
    else {
        if list.is_none() {
            *list = Some(below.clone());
        }
        return;
    };
    let pending = below.iter().any(|entry| is_plus(entry));
    let bare = |entry: &str| entry.trim().trim_start_matches('+').trim().to_owned();
    let mut merged = below.clone();
    for entry in added {
        let entry = bare(entry);
        let written = if pending {
            format!("+{entry}")
        } else {
            entry.clone()
        };
        match merged
            .iter_mut()
            .find(|held| key(&bare(held)) == key(&entry))
        {
            Some(held) => *held = written,
            None => merged.push(written),
        }
    }
    *list = Some(merged);
}

/// A tool entry's base before `(`, in RimZ's spelling.
fn tool_key(entry: &str) -> &str {
    crate::agents::canonical_tool_name(entry.split('(').next().unwrap_or("").trim())
}

/// An `mcps` entry's name before its load mark.
fn marked_name(entry: &str) -> &str {
    entry
        .rsplit_once(':')
        .map_or(entry, |(name, _)| name)
        .trim()
}

/// Fills each listed field `$to` leaves unset from `$from`.
macro_rules! fill {
    ($to:ident, $from:ident; $($field:ident),* $(,)?) => { $(
        if $to.$field.is_none() {
            $to.$field = $from.$field.clone();
        }
    )* };
}

impl AgentFrontmatter {
    pub(super) fn inherit(&mut self, parent: &Self) {
        if self.model.is_none() && self.tier.is_none() {
            self.model.clone_from(&parent.model);
            self.tier.clone_from(&parent.tier);
        }
        fill!(self, parent;
            isolation,
            empty_dir,
            mode,
            effort,
            fast,
            code_mode,
            auto_compact,
            budget,
            timeout,
            model_reminder,
            keep_warm,
            idle_compact,
            smart_compact
        );
        self.merge_lists(parent);
    }

    /// Fills what a definition's chain leaves unset from its kind base, below the whole chain.
    /// The base's description, traits, and launch tuning (effort, fast, code-mode, auto-compact)
    /// stay its own: the tuning fills at launch below written values and follows the final kind.
    /// `delegates` skips the base's `subagents` for a chain that lists the `Agent` tool.
    pub(super) fn inherit_base(&mut self, base: &Self, namespace: &str, delegates: bool) {
        let mut base = base.clone().for_namespace(namespace);
        if delegates {
            base.subagents = None;
        }
        if self.model.is_none() && self.tier.is_none() {
            self.model.clone_from(&base.model);
        }
        fill!(self, base;
            isolation,
            empty_dir,
            mode,
            budget,
            timeout,
            model_reminder,
            keep_warm,
            idle_compact,
            smart_compact
        );
        self.merge_lists(&base);
    }

    /// Fills each list key left unset from `below`, and adds a `+` list to the list below it.
    fn merge_lists(&mut self, below: &Self) {
        merge_list(&mut self.tools, below.tools.as_ref(), tool_key);
        merge_list(
            &mut self.allowed_tools,
            below.allowed_tools.as_ref(),
            str::trim,
        );
        merge_list(&mut self.skills, below.skills.as_ref(), str::trim);
        merge_list(&mut self.mcps, below.mcps.as_ref(), marked_name);
        merge_list(&mut self.subagents, below.subagents.as_ref(), str::trim);
    }

    fn lists(&self) -> [(&'static str, Option<&Vec<String>>); 5] {
        [
            ("tools", self.tools.as_ref()),
            ("allowed-tools", self.allowed_tools.as_ref()),
            ("skills", self.skills.as_ref()),
            ("mcps", self.mcps.as_ref()),
            ("subagents", self.subagents.as_ref()),
        ]
    }

    /// Refuses a written list that mixes `+` entries with plain ones, or holds a bare `+`.
    pub(super) fn check_plus(&self) -> Result<(), String> {
        for (key, list) in self.lists() {
            let Some(list) = list else { continue };
            if list.iter().any(|entry| entry.trim() == "+") {
                return Err(format!(
                    "sets `{key}:` with a bare `+`; write `+` and the entry it adds"
                ));
            }
            if list.iter().any(|entry| is_plus(entry)) && !list.iter().all(|entry| is_plus(entry)) {
                return Err(format!(
                    "sets `{key}:` mixing `+` entries with plain ones; write every entry with `+` \
                     to add to the inherited list, or none to replace it"
                ));
            }
        }
        Ok(())
    }

    /// Settles each `+` list nothing below added to, once the whole chain and the kind base are
    /// in: `skills`, `mcps`, and `subagents` stay absent, which already means everything, and
    /// `tools` and `allowed-tools` have no list to add to.
    pub(super) fn settle_plus(&mut self, kind: &str) -> Result<(), String> {
        for (key, list) in self.lists() {
            if list.is_some_and(|list| list.iter().any(|entry| is_plus(entry)))
                && matches!(key, "tools" | "allowed-tools")
            {
                return Err(format!(
                    "sets `{key}:` with `+` entries, but neither its chain nor the {kind} kind \
                     base lists `{key}`; write the full list"
                ));
            }
        }
        for list in [&mut self.skills, &mut self.mcps, &mut self.subagents] {
            if list
                .as_ref()
                .is_some_and(|list| list.iter().any(|entry| is_plus(entry)))
            {
                *list = None;
            }
        }
        Ok(())
    }

    /// A kind base's fields less those `namespace` refuses: a subagent takes no
    /// `idle-compact` or `subagents`, and a direct profile no `timeout`.
    pub(super) fn for_namespace(mut self, namespace: &str) -> Self {
        if namespace == "subagents" {
            self.idle_compact = None;
            self.subagents = None;
        } else {
            self.timeout = None;
        }
        self
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TeamFrontmatter {
    #[serde(rename = "keep-warm")]
    pub keep_warm: Option<crate::config::KeepWarmPolicy>,
    pub name: Option<String>,
    #[serde(rename = "description")]
    _description: Option<String>,
    pub layout: Option<String>,
    pub leader: Option<String>,
    #[serde(default, deserialize_with = "list")]
    pub stages: Option<Vec<String>>,
    #[serde(default, deserialize_with = "list")]
    pub traits: Option<Vec<String>>,
    #[serde(default, deserialize_with = "list")]
    pub consensus: Option<Vec<String>>,
    #[serde(default, deserialize_with = "blank_as_empty")]
    pub roles: Vec<RoleFrontmatter>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(super) struct RoleFrontmatter {
    pub agent: String,
    pub role: Option<String>,
    #[serde(default, deserialize_with = "list")]
    pub owns: Option<Vec<String>>,
    pub model: Option<String>,
    pub tier: Option<String>,
    pub mode: Option<PermissionMode>,
    pub effort: Option<String>,
    pub fast: Option<crate::agents::Toggle>,
    #[serde(default, deserialize_with = "token_count")]
    pub auto_compact: Option<String>,
    #[serde(default, deserialize_with = "flip_compact")]
    pub flip_compact: Option<String>,
    #[serde(default, deserialize_with = "idle_compact")]
    pub idle_compact: Option<IdleCompact>,
    #[serde(default, deserialize_with = "smart_compact")]
    pub smart_compact: Option<crate::config::FlipCompact>,
    #[serde(default, deserialize_with = "number_text")]
    pub budget: Option<String>,
    pub keep_warm: Option<crate::config::KeepWarmPolicy>,
    pub model_reminder: Option<bool>,
    #[serde(default, deserialize_with = "list")]
    pub traits: Option<Vec<String>>,
    #[serde(default, deserialize_with = "list")]
    pub tools: Option<Vec<String>>,
    #[serde(default, deserialize_with = "list")]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default, deserialize_with = "list")]
    pub subagents: Option<Vec<String>>,
    #[serde(default, deserialize_with = "list")]
    pub skills: Option<Vec<String>>,
    #[serde(default, deserialize_with = "list")]
    pub mcps: Option<Vec<String>>,
    #[serde(default, deserialize_with = "list")]
    pub signals: Option<Vec<SignalFrontmatter>>,
    #[serde(rename = "meka", default, deserialize_with = "retired_role_meka")]
    _meka: (),
}

impl RoleFrontmatter {
    pub(super) fn overlay(&self) -> AgentFrontmatter {
        AgentFrontmatter {
            model: self.model.clone(),
            tier: self.tier.clone(),
            mode: self.mode,
            effort: self.effort.clone(),
            fast: self.fast,
            auto_compact: self.auto_compact.clone(),
            idle_compact: self.idle_compact,
            smart_compact: self.smart_compact,
            budget: self.budget.clone(),
            model_reminder: self.model_reminder,
            tools: self.tools.clone(),
            allowed_tools: self.allowed_tools.clone(),
            subagents: self.subagents.clone(),
            skills: self.skills.clone(),
            mcps: self.mcps.clone(),
            ..AgentFrontmatter::default()
        }
    }
}

fn retired_role_meka<'de, D: Deserializer<'de>>(deserializer: D) -> Result<(), D::Error> {
    IgnoredAny::deserialize(deserializer)?;
    Err(serde::de::Error::custom(
        "role still sets `meka:`; its model decides the runtime",
    ))
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum SignalFrontmatter {
    Selector(String),
    Binding(SignalBindingFrontmatter),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SignalBindingFrontmatter {
    pub signal: String,
    #[serde(rename = "match")]
    pub matches: Option<BTreeMap<String, String>>,
    pub prompt: Option<String>,
}

fn list<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<Vec<T>>, D::Error> {
    blank_as_empty(deserializer).map(Some)
}

/// serde-saphyr refuses YAML null for a sequence, so a blank `key:` reads as
/// an empty list here and reaches the definition's own check.
fn blank_as_empty<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Vec<T>, D::Error> {
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
}

/// `off` or a token floor; YAML reads a bare `off` as `false`, so `false`
/// means `off` and `true` names no floor.
fn idle_compact<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<IdleCompact>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Setting {
        Boolean(bool),
        Text(String),
        Integer(u64),
    }
    let invalid = || serde::de::Error::custom(IdleCompactParseError);
    let raw = match Setting::deserialize(deserializer).map_err(|_| invalid())? {
        Setting::Boolean(false) => return Ok(Some(IdleCompact::Off)),
        Setting::Boolean(true) => return Err(invalid()),
        Setting::Text(text) => text,
        Setting::Integer(count) => count.to_string(),
    };
    raw.parse().map(Some).map_err(|_| invalid())
}

fn flip_compact<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Count {
        Boolean(bool),
        Text(String),
        Integer(u64),
    }
    let invalid = || {
        serde::de::Error::custom(
            "flip-compact takes a token count such as '120k', a percentage such as '70%', or 'off'",
        )
    };
    Ok(Some(
        match Count::deserialize(deserializer).map_err(|_| invalid())? {
            Count::Boolean(false) => "off".to_owned(),
            Count::Boolean(true) => return Err(invalid()),
            Count::Text(text) => text,
            Count::Integer(count) => count.to_string(),
        },
    ))
}

fn smart_compact<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<crate::config::FlipCompact>, D::Error> {
    let invalid = || {
        serde::de::Error::custom(
            "smart-compact takes a token count such as '200k', a percentage such as '70%', or 'off'",
        )
    };
    let Some(raw) = flip_compact(deserializer).map_err(|_| invalid())? else {
        return Ok(None);
    };
    if raw.eq_ignore_ascii_case("off") {
        return Ok(Some(crate::config::FlipCompact::Off));
    }
    crate::store::message::AutoCompact::parse(&raw)
        .map(|line| Some(crate::config::FlipCompact::Threshold(line)))
        .map_err(|_| invalid())
}

fn token_count<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Count {
        Text(String),
        Integer(u64),
    }
    Ok(Some(match Count::deserialize(deserializer)? {
        Count::Text(text) => text,
        Count::Integer(count) => count.to_string(),
    }))
}

fn number_text<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Number {
        Text(String),
        Integer(i64),
        Float(f64),
    }
    Ok(Some(match Number::deserialize(deserializer)? {
        Number::Text(text) => text,
        Number::Integer(number) => number.to_string(),
        Number::Float(number) => number.to_string(),
    }))
}

pub(crate) fn split<'a>(path: &Path, text: &'a str) -> Result<(&'a str, &'a str), DefinitionErr> {
    let mut lines = text.split_inclusive('\n');
    let first = lines.next().unwrap_or("");
    if first.trim_end_matches(['\r', '\n']) != "---" {
        return Err(DefinitionErr::new(path, "is missing YAML frontmatter"));
    }
    let mut offset = first.len();
    for line in lines {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            return Ok((&text[first.len()..offset], &text[offset + line.len()..]));
        }
        offset += line.len();
    }
    Err(DefinitionErr::new(
        path,
        "never closes its frontmatter with `---`",
    ))
}

pub(super) fn parse<T: serde::de::DeserializeOwned>(
    path: &Path,
    yaml: &str,
) -> Result<T, DefinitionErr> {
    let keys: BTreeMap<String, IgnoredAny> = serde_saphyr::from_str(yaml).map_err(|error| {
        DefinitionErr::new(
            path,
            format!("frontmatter is not a mapping or is malformed: {error}"),
        )
    })?;
    for (key, message) in [
        (
            "soul",
            "still selects `soul:`; put its prompt body in the definition",
        ),
        (
            "meka",
            "still sets `meka:`; name the runtime base in `agent:`",
        ),
        (
            "signals",
            "sets `signals:`, which rimz reads on a team role; declare the binding on the role that receives the event",
        ),
        (
            "flip-compact",
            "sets `flip-compact:`, which rimz reads on a team role; a solo seat flips no stage",
        ),
    ] {
        if keys.contains_key(key) {
            return Err(DefinitionErr::new(path, message));
        }
    }
    serde_saphyr::from_str(yaml)
        .map_err(|error| DefinitionErr::new(path, format!("malformed frontmatter: {error}")))
}

pub(super) fn description(path: &Path, text: Option<&str>) -> Result<String, DefinitionErr> {
    let text = text.unwrap_or("").trim();
    if text.is_empty() {
        return Err(DefinitionErr::new(path, "has no non-empty `description:`"));
    }
    if text.contains(['\n', '\r']) {
        return Err(DefinitionErr::new(
            path,
            "has a multi-line `description:`; keep it to one line",
        ));
    }
    Ok(text.to_owned())
}
