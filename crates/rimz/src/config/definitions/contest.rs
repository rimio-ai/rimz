//! Which side gets a RimZ capability whose native tool a definition's lists also claim: a named
//! native tool outranks a blanket RimZ grant, and two named entries refuse the definition
//! (`crate::agents::contest`). Each message names the file of the chain that holds each entry.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::agents::{self, Contest, RimzClaim, ToolSet};
use crate::config::McpName;
use crate::mcp::catalog::{Capability, SERVER_NAME};

use super::frontmatter::{self, AgentFrontmatter};

/// A capability a definition leaves to the provider's own tool, with the warning that says so.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeWin {
    pub capability: Capability,
    pub message: String,
    /// The definition's own file, which a check reports the warning against.
    pub path: PathBuf,
}

/// Where a layer of a definition's chain comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Origin {
    File(PathBuf),
    BuiltIn(String),
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File(path) => write!(f, "{}", path.display()),
            Self::BuiltIn(kind) => write!(f, "built-in kind base {kind}"),
        }
    }
}

/// One layer of a definition's chain: its file, the chain member it is there, and the lists it
/// writes itself.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Layer {
    origin: Origin,
    member: String,
    tools: Option<Vec<String>>,
    capabilities: Option<Vec<String>>,
    mcps: Option<Vec<String>>,
    skills: Option<Vec<String>>,
    subagents: Option<Vec<String>>,
}

impl Layer {
    fn new(fm: &AgentFrontmatter, member: &str, origin: Origin) -> Self {
        Self {
            origin,
            member: member.to_owned(),
            tools: fm.tools.clone(),
            capabilities: fm.capabilities.clone(),
            mcps: fm.mcps.clone(),
            skills: fm.skills.clone(),
            subagents: fm.subagents.clone(),
        }
    }

    fn list(&self, key: &str) -> Option<&[String]> {
        match key {
            "tools" => self.tools.as_deref(),
            "capabilities" => self.capabilities.as_deref(),
            "mcps" => self.mcps.as_deref(),
            "skills" => self.skills.as_deref(),
            _ => self.subagents.as_deref(),
        }
    }
}

/// The layers of a definition's chain that may write a list, nearest first, so a message can
/// name the file whose own list holds an entry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ListWriters(Vec<Layer>);

impl ListWriters {
    pub(super) fn own(fm: &AgentFrontmatter, member: &str, path: &Path) -> Self {
        Self(vec![Layer::new(
            fm,
            member,
            Origin::File(path.to_path_buf()),
        )])
    }

    /// Adds `parent`'s layers below these, each chain member once, as a chain names each member
    /// once; two rows of one TOML file are two members.
    pub(super) fn inherit(&mut self, parent: &Self) {
        for layer in &parent.0 {
            if !self
                .0
                .iter()
                .any(|held| held.origin == layer.origin && held.member == layer.member)
            {
                self.0.push(layer.clone());
            }
        }
    }

    /// These layers above the kind base `base`, from `path` or, with none, built in.
    pub(super) fn above_base(
        &self,
        kind: &str,
        base: &AgentFrontmatter,
        path: Option<&Path>,
    ) -> Self {
        let origin = path.map_or_else(
            || Origin::BuiltIn(kind.to_owned()),
            |path| Origin::File(path.to_path_buf()),
        );
        let mut writers = self.clone();
        writers.0.push(Layer::new(base, kind, origin));
        writers
    }

    /// The nearest layer whose own `key` list holds an entry `names` matches, walking down
    /// through `+` lists; a plain list ends the walk, as does a layer that writes the other of
    /// `tools` and `capabilities`, which fill as one choice. A `subagents` list is held by the
    /// nearest layer that writes one.
    fn holder(&self, key: &str, names: impl Fn(&str) -> bool) -> Option<&Layer> {
        for layer in &self.0 {
            let Some(list) = layer.list(key) else {
                let other = match key {
                    "tools" => layer.capabilities.is_some(),
                    "capabilities" => layer.tools.is_some(),
                    _ => false,
                };
                if other {
                    return None;
                }
                continue;
            };
            if key == "subagents" || list.iter().any(|entry| names(frontmatter::added(entry))) {
                return Some(layer);
            }
            if !list.iter().any(|entry| frontmatter::is_plus(entry)) {
                return None;
            }
        }
        None
    }

    /// Every file of the chain, for a message whose entry no single layer holds.
    fn files(&self) -> String {
        let mut files: Vec<String> = Vec::new();
        for layer in &self.0 {
            let file = layer.origin.to_string();
            if !files.contains(&file) {
                files.push(file);
            }
        }
        format!("one of {}", files.join(", "))
    }
}

/// The lowering inputs one contest reads: the definition's merged lists, after its kind base,
/// `+` settling, and seat edits, and the layers that wrote them.
pub(super) struct Contested<'a> {
    pub kind: &'a str,
    pub namespace: &'a str,
    pub own: &'a Path,
    pub fm: &'a AgentFrontmatter,
    pub writers: &'a ListWriters,
}

/// One claim of a contest: each entry of its list that makes it, from the layer whose own list
/// holds them where one does, else from the merged list.
struct Entry<'a> {
    key: &'static str,
    texts: Vec<String>,
    layer: Option<&'a Layer>,
}

impl Contested<'_> {
    /// The claim `merged`'s entries that `names` matches make, where any does.
    fn entry(
        &self,
        key: &'static str,
        merged: &[String],
        names: impl Fn(&str) -> bool,
    ) -> Option<Entry<'_>> {
        if !merged.iter().any(|entry| names(frontmatter::added(entry))) {
            return None;
        }
        let layer = self.writers.holder(key, &names);
        let list = layer.and_then(|layer| layer.list(key)).unwrap_or(merged);
        let mut texts: Vec<String> = Vec::new();
        for text in list.iter().map(|entry| frontmatter::added(entry)) {
            if names(text) && !texts.iter().any(|held| held == text) {
                texts.push(text.to_owned());
            }
        }
        Some(Entry { key, texts, layer })
    }

    fn file(&self, entry: &Entry<'_>) -> String {
        entry
            .layer
            .map_or_else(|| self.writers.files(), |layer| layer.origin.to_string())
    }

    fn is_own(&self, entry: &Entry<'_>) -> bool {
        entry
            .layer
            .is_some_and(|layer| layer.origin == Origin::File(self.own.to_path_buf()))
    }

    /// " of <file>" for an entry outside the definition's own file.
    fn of(&self, entry: &Entry<'_>) -> String {
        if self.is_own(entry) {
            String::new()
        } else {
            format!(" of {}", self.file(entry))
        }
    }

    /// The edit that drops `entry`'s claim in its file: remove every entry of the list that
    /// makes it, or delete the `key:` line where a `+` list would be left empty, since `key: []`
    /// would replace what the chain below writes. With `literal`, a list in the definition's own
    /// file shows itself before and after.
    fn fix(&self, entry: &Entry<'_>, literal: bool) -> String {
        let (key, file) = (entry.key, self.file(entry));
        let remove = format!("remove {} from `{key}:` in {file}", quoted(&entry.texts));
        let Some(list) = entry.layer.and_then(|layer| layer.list(key)) else {
            return remove;
        };
        let before: Vec<&str> = list.iter().map(|text| text.trim()).collect();
        let after: Vec<&str> = before
            .iter()
            .copied()
            .filter(|text| {
                !entry
                    .texts
                    .iter()
                    .any(|held| held == frontmatter::added(text))
            })
            .collect();
        if after.is_empty() && list.iter().any(|text| frontmatter::is_plus(text)) {
            return format!("delete the `{key}:` line in {file}");
        }
        if !literal || !self.is_own(entry) {
            return remove;
        }
        let render = |entries: &[&str]| format!("`{key}: [{}]`", entries.join(", "));
        format!("{remove} ({} becomes {})", render(&before), render(&after))
    }

    /// The entry that names the native tool `capability` stands in for, where the lists name it
    /// (`crate::agents::native_claim` over the tool bases they lower to).
    fn native(&self, capability: Capability) -> Option<Entry<'_>> {
        if let Some(written) = self.fm.capabilities.as_deref() {
            let word = |entry: &str| {
                super::agent::WORDS
                    .iter()
                    .find(|(word, _)| *word == frontmatter::marked_name(entry))
                    .map_or(&[][..], |(_, tools)| *tools)
            };
            let bases: Vec<String> = written
                .iter()
                .flat_map(|entry| word(frontmatter::added(entry)))
                .map(|tool| (*tool).to_owned())
                .collect();
            let tool = agents::native_claim(self.kind, &bases, capability)?;
            return self.entry("capabilities", written, |entry| word(entry).contains(&tool));
        }
        let written = self.fm.tools.as_deref()?;
        let bases = ToolSet::parse(written).ok()?;
        let tool = agents::native_claim(self.kind, bases.bases(), capability)?;
        self.entry("tools", written, |entry| {
            frontmatter::tool_key(entry) == tool
        })
    }

    /// How the RimZ lists reach `capability`: a named entry before a blanket one, which is
    /// `rimz` in `capabilities:`, `*` or `rimz` in `mcps`, or no `mcps` at all (no entry).
    fn rimz(&self, capability: Capability) -> Option<(RimzClaim, Option<Entry<'_>>)> {
        let name = capability.name();
        let find = |key: &'static str, list: Option<&[String]>, names: &dyn Fn(&str) -> bool| {
            self.entry(key, list?, names)
        };
        if let Some(written) = self.fm.capabilities.as_deref() {
            let named = |entry: &str| frontmatter::marked_name(entry) == name;
            let blanket = |entry: &str| frontmatter::marked_name(entry) == SERVER_NAME;
            return find("capabilities", Some(written), &named)
                .map(|entry| (RimzClaim::Named, Some(entry)))
                .or_else(|| {
                    find("capabilities", Some(written), &blanket)
                        .map(|entry| (RimzClaim::Blanket, Some(entry)))
                });
        }
        let mcp = |entry: &str| entry.trim().parse::<McpName>().ok();
        let named_mcp =
            |entry: &str| mcp(entry).and_then(|entry| entry.capability()) == Some(capability);
        let named_skill = |entry: &str| entry.trim() == name;
        if let Some(entry) = find("mcps", self.fm.mcps.as_deref(), &named_mcp)
            .or_else(|| find("skills", self.fm.skills.as_deref(), &named_skill))
        {
            return Some((RimzClaim::Named, Some(entry)));
        }
        if capability == Capability::Subagents && self.fm.subagents.is_some() {
            return Some((
                RimzClaim::Named,
                Some(Entry {
                    key: "subagents",
                    texts: Vec::new(),
                    layer: self.writers.holder("subagents", |_| true),
                }),
            ));
        }
        let Some(written) = self.fm.mcps.as_deref() else {
            return Some((RimzClaim::Blanket, None));
        };
        let blanket = |entry: &str| mcp(entry).is_some_and(|entry| entry.serves_every_capability());
        find("mcps", Some(written), &blanket).map(|entry| (RimzClaim::Blanket, Some(entry)))
    }
}

/// Refuses two named entries that claim one capability; returns each capability a named native
/// tool takes from a blanket grant, none on a subagent definition, which keeps neither native
/// tool at launch.
pub(super) fn settle(contested: &Contested<'_>) -> Result<Vec<NativeWin>, String> {
    let mut wins = Vec::new();
    for capability in Capability::ALL {
        let Some(entry) = contested.native(capability) else {
            continue;
        };
        let rimz = contested.rimz(capability);
        let native = agents::native_tool(capability);
        match agents::contest(native, rimz.as_ref().map(|(claim, _)| *claim)) {
            Some(Contest::Refused) => {
                let Some((_, Some(listed))) = &rimz else {
                    continue;
                };
                return Err(refusal(contested, capability, &entry, listed));
            }
            Some(Contest::NativeWins) if contested.namespace != "subagents" => {
                let source = rimz.as_ref().and_then(|(_, entry)| entry.as_ref());
                wins.push(NativeWin {
                    capability,
                    message: warning(contested, capability, &entry, source),
                    path: contested.own.to_path_buf(),
                });
            }
            _ => {}
        }
    }
    Ok(wins)
}

/// What the launch uses in place of `capability`, and the verb a RimZ fix offers.
fn outcome(capability: Capability) -> (&'static str, &'static str) {
    if capability == Capability::Ask {
        ("the native question tool", "ask")
    } else {
        ("native delegation", "delegate")
    }
}

fn warning(
    contested: &Contested<'_>,
    capability: Capability,
    native: &Entry<'_>,
    source: Option<&Entry<'_>>,
) -> String {
    let source = match source {
        None => "RimZ's default (no `mcps`)".to_owned(),
        Some(entry) => {
            let at = if entry.layer.map(|layer| &layer.origin)
                == native.layer.map(|layer| &layer.origin)
            {
                String::new()
            } else {
                format!(" of {}", contested.file(entry))
            };
            format!("{} in `{}:`{at}", quoted(&entry.texts), entry.key)
        }
    };
    let (outcome, verb) = outcome(capability);
    format!(
        "{file} lists {entries} in `{key}:` and gets `{name}` from {source}; using \
         {outcome}. To {verb} through RimZ instead, {fix}",
        file = contested.file(native),
        entries = quoted(&native.texts),
        key = native.key,
        name = capability.name(),
        fix = contested.fix(native, true),
    )
}

fn refusal(
    contested: &Contested<'_>,
    capability: Capability,
    native: &Entry<'_>,
    listed: &Entry<'_>,
) -> String {
    let (_, verb) = outcome(capability);
    let (claim, fix) = if listed.key == "subagents" {
        (
            format!("sets `subagents:`{}", contested.of(listed)),
            format!("drop `subagents:` from {}", contested.file(listed)),
        )
    } else {
        (
            format!(
                "names {} in `{}:`{}",
                quoted(&listed.texts),
                listed.key,
                contested.of(listed)
            ),
            contested.fix(listed, false),
        )
    };
    format!(
        "lists {entries} in `{key}:`{at} and {claim}; each says how this seat {verb}s, so keep \
         the one it uses: {native_fix}, or {fix}",
        entries = quoted(&native.texts),
        key = native.key,
        at = contested.of(native),
        native_fix = contested.fix(native, false),
    )
}

/// `texts` as a message names them: each in backticks, joined by `and`.
fn quoted(texts: &[String]) -> String {
    let quoted: Vec<String> = texts.iter().map(|text| format!("`{text}`")).collect();
    match quoted.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => quoted.concat(),
    }
}

/// The warning for a launch that leaves `capability` to the native tool where no loaded
/// definition's own lists say so, as when a launch pairs one layer's lists with another's
/// `mcps`. It names the entry in the shape the request carries: the capability's word in
/// `capabilities:` for a `granted` request, else its tool in `tools:`. It points at the
/// profile's own file and chain where a definition stands behind the launch, and at the kind
/// base, by its `base` file where it has one. `source` names the blanket grant RimZ reaches the
/// capability through.
pub(crate) fn launch_warning(
    kind: &str,
    capability: Capability,
    source: &str,
    granted: bool,
    profile: Option<(&Path, &[String])>,
    base: Option<&Path>,
) -> String {
    let tool = agents::native_tool(capability).unwrap_or_default();
    let word = super::agent::WORDS
        .iter()
        .find(|(_, tools)| tools.contains(&tool))
        .map(|(word, _)| *word);
    let (entry, key) = match word {
        Some(word) if granted => (word, "capabilities"),
        _ => (tool, "tools"),
    };
    let (outcome, verb) = outcome(capability);
    let base = base.map_or_else(
        || format!("the {kind} kind base"),
        |path| path.display().to_string(),
    );
    let place = match profile {
        Some((path, chain)) => format!(
            "{} or the file of its chain ({}, then {base}) that lists it",
            path.display(),
            chain.join(" → ")
        ),
        None => format!("the launch's profile or {base}"),
    };
    format!(
        "this launch lists `{entry}` in `{key}:` and gets `{name}` from {source}; using \
         {outcome}. To {verb} through RimZ instead, remove `{entry}` from `{key}:` in \
         {place}",
        name = capability.name(),
    )
}
