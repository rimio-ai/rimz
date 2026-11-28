//! Strict YAML frontmatter and the fields inherited along definition chains.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Deserializer, de::IgnoredAny};

use crate::agents::PermissionMode;
use crate::config::{IdleCompact, IdleCompactParseError};

use super::DefinitionErr;

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
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
    pub capabilities: Option<Vec<String>>,
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
pub(super) fn is_plus(entry: &str) -> bool {
    entry.trim_start().starts_with('+')
}

/// An entry less the one `+` that marks it added.
pub(super) fn added(entry: &str) -> &str {
    let entry = entry.trim();
    entry.strip_prefix('+').unwrap_or(entry).trim()
}

/// Fills `list` from `below` when unset. A `+` list adds its entries to `below`: the added
/// entries with a `key` `below` already holds replace every entry of `below` with that key, in
/// the place of the first, and the rest follow in written order. An added entry is matched only
/// against `below`, and an exact repeat collapses. Over a `+` list the result is still `+`,
/// pending the next list further down.
fn merge_list(list: &mut Option<Vec<String>>, below: Option<&Vec<String>>, key: fn(&str) -> &str) {
    let Some(below) = below else { return };
    let Some(written) = list
        .as_ref()
        .filter(|list| list.iter().any(|entry| is_plus(entry)))
    else {
        if list.is_none() {
            *list = Some(below.clone());
        }
        return;
    };
    let mut entries: Vec<&str> = Vec::new();
    for entry in written.iter().map(|entry| added(entry)) {
        if !entries.contains(&entry) {
            entries.push(entry);
        }
    }
    let pending = below.iter().any(|entry| is_plus(entry));
    let spell = |entry: &&str| {
        if pending {
            format!("+{entry}")
        } else {
            (*entry).to_owned()
        }
    };
    let mut merged = Vec::new();
    let mut replaced: Vec<&str> = Vec::new();
    for held in below {
        let held_key = key(added(held));
        if !entries.iter().any(|entry| key(entry) == held_key) {
            merged.push(held.clone());
        } else if !replaced.contains(&held_key) {
            replaced.push(held_key);
            merged.extend(
                entries
                    .iter()
                    .filter(|entry| key(entry) == held_key)
                    .map(spell),
            );
        }
    }
    merged.extend(
        entries
            .iter()
            .filter(|entry| !replaced.contains(&key(entry)))
            .map(spell),
    );
    *list = Some(merged);
}

/// A tool entry's base before `(`, in RimZ's spelling.
pub(super) fn tool_key(entry: &str) -> &str {
    crate::agents::canonical_tool_name(entry.split('(').next().unwrap_or("").trim())
}

/// An `mcps` or `capabilities` entry's name before its load mark.
pub(super) fn marked_name(entry: &str) -> &str {
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
    /// `tools` and `capabilities` fill as one choice: a layer that writes either, `+` list
    /// included, takes neither from below, except that its `+` list adds to its own key.
    fn merge_lists(&mut self, below: &Self) {
        if self.tools.is_none() && self.capabilities.is_none() {
            self.tools.clone_from(&below.tools);
            self.capabilities.clone_from(&below.capabilities);
        } else if self.tools.is_some() {
            merge_list(&mut self.tools, below.tools.as_ref(), tool_key);
        } else {
            merge_list(
                &mut self.capabilities,
                below.capabilities.as_ref(),
                marked_name,
            );
        }
        merge_list(
            &mut self.allowed_tools,
            below.allowed_tools.as_ref(),
            str::trim,
        );
        merge_list(&mut self.skills, below.skills.as_ref(), str::trim);
        merge_list(&mut self.mcps, below.mcps.as_ref(), marked_name);
        merge_list(&mut self.subagents, below.subagents.as_ref(), str::trim);
    }

    fn lists(&self) -> [(&'static str, Option<&Vec<String>>); 6] {
        [
            ("tools", self.tools.as_ref()),
            ("capabilities", self.capabilities.as_ref()),
            ("allowed-tools", self.allowed_tools.as_ref()),
            ("skills", self.skills.as_ref()),
            ("mcps", self.mcps.as_ref()),
            ("subagents", self.subagents.as_ref()),
        ]
    }

    /// Refuses a layer that writes both `capabilities:` and `tools:`, and a written list that
    /// mixes `+` entries with plain ones, holds a bare `+` or an entry with two, or, in
    /// `capabilities` or `mcps`, adds one name under two marks.
    pub(super) fn check_plus(&self) -> Result<(), String> {
        if self.tools.is_some() && self.capabilities.is_some() {
            return Err(
                "sets both `capabilities:` and `tools:`; keep one: `capabilities:` names what \
                 the agent may do, `tools:` the provider's own tool names"
                    .to_owned(),
            );
        }
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
            if !list.iter().any(|entry| is_plus(entry)) {
                continue;
            }
            if let Some(entry) = list.iter().find(|entry| added(entry).starts_with('+')) {
                return Err(format!(
                    "sets `{key}:` with `{}`, which starts with `+` after its own; write one `+` \
                     and the entry it adds",
                    entry.trim()
                ));
            }
            if !matches!(key, "capabilities" | "mcps") {
                continue;
            }
            for (at, entry) in list.iter().enumerate() {
                let entry = added(entry);
                if list[..at]
                    .iter()
                    .map(|earlier| added(earlier))
                    .any(|earlier| earlier != entry && marked_name(earlier) == marked_name(entry))
                {
                    return Err(format!(
                        "lists `{}` in `{key}:` with two different marks; keep one",
                        marked_name(entry)
                    ));
                }
            }
        }
        Ok(())
    }

    /// Settles each `+` list nothing below added to, once the whole chain and the kind base are
    /// in: `skills` and `mcps` stay absent, which already means everything, their names kept in
    /// the [`AddedNames`] returned, and `tools`, `capabilities`, and `allowed-tools` have no
    /// list to add to. A pending `subagents` list stays for `finish_profile`, which refuses it
    /// where a plain list is refused before it reads it as absent.
    pub(super) fn settle_plus(&mut self, kind: &str) -> Result<AddedNames, String> {
        for (key, list) in self.lists() {
            if list.is_some_and(|list| list.iter().any(|entry| is_plus(entry)))
                && matches!(key, "tools" | "capabilities" | "allowed-tools")
            {
                return Err(format!(
                    "sets `{key}:` with `+` entries, but neither its chain nor the {kind} kind \
                     base lists `{key}`; write the full list"
                ));
            }
        }
        let mut added_names = AddedNames::default();
        for (list, names) in [
            (&mut self.skills, &mut added_names.skills),
            (&mut self.mcps, &mut added_names.mcps),
        ] {
            if list
                .as_ref()
                .is_some_and(|list| list.iter().any(|entry| is_plus(entry)))
            {
                *names = list
                    .take()
                    .map(|list| list.iter().map(|entry| added(entry).to_owned()).collect());
            }
        }
        Ok(added_names)
    }

    /// A kind base's or direct parent's fields less those `namespace` drops: a subagent takes no
    /// `idle-compact`, `keep-warm`, or `subagents`, and a direct profile no `timeout`.
    pub(super) fn for_namespace(mut self, namespace: &str) -> Self {
        if namespace == "subagents" {
            self.idle_compact = None;
            self.keep_warm = None;
            self.subagents = None;
        } else {
            self.timeout = None;
        }
        self
    }
}

/// The entries of each `skills` and `mcps` `+` list [`AgentFrontmatter::settle_plus`] left
/// absent, less their `+`, which still take every name check a plain list of that key takes.
#[derive(Default)]
pub(super) struct AddedNames {
    pub skills: Option<Vec<String>>,
    pub mcps: Option<Vec<String>>,
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
    pub capabilities: Option<Vec<String>>,
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
            capabilities: self.capabilities.clone(),
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
