//! Chain resolution and profile materialization for both definition namespaces.

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::agents::{self, ManualSkill, PresetField, ToolSet};
use crate::config::tiers::{DefinitionRenders, ModelTier, TierPreference, TierProvenance};
use crate::config::{Isolation, Profile, PromptSource, SkillName};
use crate::mcp::catalog::{Capability, LoadMode, SERVER_NAME};

use super::frontmatter::AgentFrontmatter;
use super::{
    BuildBase, DefinitionCause, DefinitionErr, LoadContext, LoadScope, LoadedDefinitions,
    Namespace, SkillCatalog, SkillCheck, SubagentCheck, frontmatter, traits,
};

/// Retained Markdown definition state for rebuilding a stored profile row under an override,
/// target kind, or model without rereading disk.
#[derive(Clone, Debug)]
pub struct DefinitionSource {
    inner: Arc<DefinitionSourceInner>,
}

impl PartialEq for DefinitionSource {
    fn eq(&self, other: &Self) -> bool {
        if Arc::ptr_eq(&self.inner, &other.inner) {
            return true;
        }
        let (a, b) = (self.inner.as_ref(), other.inner.as_ref());
        a.name == b.name
            && a.path == b.path
            && a.namespace == b.namespace
            && a.chain == b.chain
            && a.frontmatter == b.frontmatter
            && a.crafts == b.crafts
            && a.natural_kind == b.natural_kind
            && a.natural_variant == b.natural_variant
            && a.preference == b.preference
            && a.runtime_kind == b.runtime_kind
            && a.seat == b.seat
            && a.context == b.context
    }
}

impl Eq for DefinitionSource {}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum MemoBase {
    None,
    Runtime(String, Option<&'static str>),
    KindBase(String, Option<&'static str>),
    Definition(Vec<String>, PathBuf),
    Toml(String, Option<&'static str>, Option<String>),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct MemoKey {
    base: MemoBase,
    target_kind: Option<String>,
    model: Option<String>,
}

impl MemoKey {
    fn new(base: Option<BuildBase<'_>>, target_kind: Option<&str>, model: Option<&str>) -> Self {
        let base = match base {
            None => MemoBase::None,
            Some(BuildBase::Runtime(rt)) => {
                MemoBase::Runtime(rt.kind().to_owned(), rt.variant.map(|v| v.name))
            }
            Some(BuildBase::Profile(profile)) if profile.kind_base => {
                MemoBase::KindBase(profile.agent.clone(), profile.variant.map(|v| v.name))
            }
            Some(BuildBase::Profile(profile)) => match &profile.definition {
                Some(def) => MemoBase::Definition(def.inner.chain.clone(), def.inner.path.clone()),
                None => {
                    let rt = agents::find_runtime(&profile.agent);
                    let kind = rt.map_or(profile.agent.as_str(), |rt| rt.kind());
                    let variant = profile.variant.or_else(|| rt.and_then(|rt| rt.variant));
                    MemoBase::Toml(
                        kind.to_owned(),
                        variant.map(|v| v.name),
                        profile.model.clone(),
                    )
                }
            },
        };
        Self {
            base,
            target_kind: target_kind.map(str::to_owned),
            model: model.map(str::to_owned),
        }
    }
}

#[derive(Debug)]
struct DefinitionSourceInner {
    name: String,
    path: PathBuf,
    namespace: &'static str,
    chain: Vec<String>,
    frontmatter: AgentFrontmatter,
    crafts: Vec<PromptSource>,
    natural_kind: String,
    natural_variant: Option<&'static agents::Variant>,
    preference: TierPreference,
    runtime_kind: Option<String>,
    seat: Option<SeatInfo>,
    context: Arc<LoadContext>,
    memo: Mutex<BTreeMap<MemoKey, Result<Profile, DefinitionErr>>>,
}

enum CraftState<'a> {
    Pending {
        traits: &'a BTreeMap<String, crate::packs::ClaimResolution>,
        body: &'a str,
        trait_names: &'a [String],
        cached: Option<Result<String, DefinitionErr>>,
    },
    Rendered,
}

fn lists_agent_tool(fm: &AgentFrontmatter) -> bool {
    fm.tools
        .as_deref()
        .and_then(|tools| {
            let bare: Vec<String> = tools
                .iter()
                .map(|tool| tool.trim().trim_start_matches('+').to_owned())
                .collect();
            ToolSet::parse(&bare).ok()
        })
        .is_some_and(|tools| tools.has("Agent"))
}

/// Whether a model inherited from `from_kind` survives a switch to `to_kind`.
fn keeps_inherited_model(from_kind: &str, to_kind: &str, model: &str) -> bool {
    from_kind == to_kind
        || agents::definition_model_kind(model) == Some(to_kind)
        || agents::keeps_model(to_kind, model)
}

fn merge_crafts(base: &[PromptSource], leaf: &[PromptSource]) -> Vec<PromptSource> {
    let mut merged = base.to_vec();
    for source in leaf {
        if !merged
            .iter()
            .any(|existing| existing.origin() == source.origin())
        {
            merged.push(source.clone());
        }
    }
    merged
}

impl DefinitionSource {
    pub(crate) fn chain(&self) -> &[String] {
        &self.inner.chain
    }

    pub(crate) fn natural_kind(&self) -> &str {
        &self.inner.natural_kind
    }

    pub(crate) fn written_model(&self) -> Option<&str> {
        self.inner.frontmatter.model.as_deref()
    }

    pub(crate) fn written_tier(&self) -> Option<ModelTier> {
        self.inner
            .frontmatter
            .tier
            .as_deref()
            .and_then(|raw| raw.parse().ok())
    }

    pub(crate) fn writes_engine(&self) -> bool {
        self.inner.frontmatter.model.is_some() || self.inner.frontmatter.tier.is_some()
    }

    pub(crate) fn preference(&self) -> &TierPreference {
        &self.inner.preference
    }

    pub(crate) fn writes_effort(&self) -> bool {
        self.inner.frontmatter.effort.is_some()
    }

    pub(crate) fn writes_fast(&self) -> bool {
        self.inner.frontmatter.fast.is_some()
    }

    pub(crate) fn writes_code_mode(&self) -> bool {
        self.inner.frontmatter.code_mode.is_some()
    }

    pub(crate) fn writes_auto_compact(&self) -> bool {
        self.inner.frontmatter.auto_compact.is_some()
    }

    pub(crate) fn build(
        &self,
        current: &Profile,
        base: Option<BuildBase<'_>>,
        target_kind: Option<&str>,
        model: Option<&str>,
    ) -> Result<Profile, DefinitionErr> {
        if let Some(BuildBase::Profile(profile)) = base
            && let Some(other) = &profile.definition
            && self
                .inner
                .chain
                .iter()
                .any(|entry| entry == &other.inner.name)
        {
            if target_kind.is_none() && model.is_none() {
                return Ok(current.clone());
            }
            return self.build(current, None, target_kind, model);
        }
        let key = MemoKey::new(base, target_kind, model);
        {
            let memo = self
                .inner
                .memo
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(cached) = memo.get(&key) {
                return cached.clone();
            }
        }
        let result =
            self.inner
                .build_with_craft(base, target_kind, model, &mut CraftState::Rendered, true);
        let mut memo = self
            .inner
            .memo
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        memo.entry(key).or_insert_with(|| result.clone());
        result
    }
}

impl DefinitionSourceInner {
    fn built_source(
        &self,
        x_def: Option<&Self>,
        frontmatter: AgentFrontmatter,
        crafts: Vec<PromptSource>,
        built: &Profile,
        preference: TierPreference,
        runtime_kind: Option<String>,
    ) -> DefinitionSource {
        let mut chain = self.chain.clone();
        if let Some(x) = x_def {
            for entry in &x.chain {
                if !chain.contains(entry) {
                    chain.push(entry.clone());
                }
            }
        }
        DefinitionSource {
            inner: Arc::new(Self {
                name: self.name.clone(),
                path: self.path.clone(),
                namespace: self.namespace,
                chain,
                frontmatter,
                crafts,
                natural_kind: built.agent.clone(),
                natural_variant: built.variant,
                preference,
                runtime_kind,
                seat: self.seat.clone(),
                context: Arc::clone(&self.context),
                memo: Mutex::default(),
            }),
        }
    }

    fn build_with_craft(
        &self,
        base: Option<BuildBase<'_>>,
        target_kind: Option<&str>,
        model: Option<&str>,
        craft: &mut CraftState<'_>,
        attach_definition: bool,
    ) -> Result<Profile, DefinitionErr> {
        let mut has_base = false;
        let mut x_def: Option<&Self> = None;
        let mut base_kind: Option<&str> = None;
        let mut base_variant: Option<&'static agents::Variant> = None;
        let mut base_writes_engine = false;
        let mut base_model_override: Option<&str> = None;
        match base {
            None => {}
            Some(BuildBase::Runtime(rt)) => {
                has_base = true;
                base_kind = Some(rt.kind());
                base_variant = rt.variant;
            }
            Some(BuildBase::Profile(profile)) if profile.kind_base => {
                has_base = true;
                base_kind = Some(profile.agent.as_str());
                base_variant = profile.variant;
            }
            Some(BuildBase::Profile(profile)) => {
                if let Some(other) = &profile.definition {
                    if !self.chain.iter().any(|entry| entry == &other.inner.name) {
                        has_base = true;
                        x_def = Some(&other.inner);
                        base_kind = (!other.inner.natural_kind.is_empty())
                            .then_some(other.inner.natural_kind.as_str());
                        base_variant = other.inner.natural_variant;
                        base_writes_engine = other.inner.frontmatter.model.is_some()
                            || other.inner.frontmatter.tier.is_some();
                    }
                } else {
                    has_base = true;
                    let rt = agents::find_runtime(&profile.agent);
                    base_kind = Some(rt.map_or(profile.agent.as_str(), |rt| rt.kind()));
                    base_variant = profile.variant.or_else(|| rt.and_then(|rt| rt.variant));
                    base_writes_engine = profile.model.is_some();
                    base_model_override = profile.model.as_deref();
                }
            }
        }

        let mut fm = self.frontmatter.clone();
        if let Some(explicit_model) = model {
            fm.model = Some(explicit_model.to_owned());
            fm.tier = None;
        } else if base_writes_engine {
            fm.model = None;
            fm.tier = None;
            if let Some(override_model) = base_model_override {
                fm.model = Some(override_model.to_owned());
            }
        }
        if let Some(x) = x_def {
            let mut x_fm = x.frontmatter.clone().for_namespace(self.namespace);
            if lists_agent_tool(&fm) {
                x_fm.subagents = None;
            }
            fm.inherit(&x_fm);
        }
        let crafts = match x_def {
            Some(x) => merge_crafts(&x.crafts, &self.crafts),
            None => self.crafts.clone(),
        };

        let (effective_kind, effective_variant, effective_preference, effective_runtime_kind) =
            if !has_base {
                (
                    self.natural_kind.clone(),
                    self.natural_variant,
                    self.preference.clone(),
                    self.runtime_kind.clone(),
                )
            } else if base_writes_engine {
                if let Some(x) = x_def {
                    (
                        x.natural_kind.clone(),
                        x.natural_variant,
                        x.preference.clone(),
                        x.runtime_kind.clone(),
                    )
                } else {
                    let kind = base_kind.unwrap_or(&self.natural_kind).to_owned();
                    (
                        kind.clone(),
                        base_variant,
                        TierPreference {
                            model: fm.model.clone(),
                            family: Some(kind.clone()),
                        },
                        Some(kind),
                    )
                }
            } else {
                let kind = base_kind.unwrap_or(&self.natural_kind).to_owned();
                (
                    kind.clone(),
                    base_variant,
                    TierPreference {
                        model: None,
                        family: base_kind
                            .map(str::to_owned)
                            .or_else(|| self.preference.family.clone()),
                    },
                    base_kind
                        .map(str::to_owned)
                        .or_else(|| self.runtime_kind.clone()),
                )
            };

        if let Some(raw_target) = target_kind {
            let target_rt = agents::find_runtime(raw_target);
            let kind = target_rt.map_or(raw_target, |rt| rt.kind());
            let variant = target_rt
                .and_then(|rt| rt.variant)
                .or_else(|| effective_variant.filter(|_| kind == effective_kind));
            if model.is_none() {
                let from_kind = if base_writes_engine {
                    effective_kind.as_str()
                } else {
                    self.natural_kind.as_str()
                };
                if let Some(inherited) = fm.model.as_deref()
                    && !keeps_inherited_model(from_kind, kind, inherited)
                {
                    fm.model = None;
                }
            }
            let pre_base_fm = fm.clone();
            let mut built = finish_profile(
                &self.context,
                &self.path,
                self.namespace,
                self.seat.as_ref(),
                kind,
                variant,
                false,
                fm,
                crafts.clone(),
                craft,
            )?;
            if attach_definition {
                built.definition = Some(self.built_source(
                    x_def,
                    pre_base_fm,
                    crafts,
                    &built,
                    effective_preference,
                    effective_runtime_kind,
                ));
            }
            return Ok(built);
        }

        if let Some(explicit_model) = model {
            let (kind, variant) = if has_base {
                let kind = if !effective_kind.is_empty() {
                    effective_kind.as_str()
                } else {
                    agents::definition_model_kind(explicit_model).unwrap_or("")
                };
                (kind, effective_variant.filter(|_| kind == effective_kind))
            } else if agents::keeps_model(&self.natural_kind, explicit_model) {
                (self.natural_kind.as_str(), self.natural_variant)
            } else if let Some(implied) = agents::definition_model_kind(explicit_model) {
                (
                    implied,
                    self.natural_variant
                        .filter(|_| implied == self.natural_kind),
                )
            } else {
                (self.natural_kind.as_str(), self.natural_variant)
            };
            let pre_base_fm = fm.clone();
            let mut built = finish_profile(
                &self.context,
                &self.path,
                self.namespace,
                self.seat.as_ref(),
                kind,
                variant,
                false,
                fm,
                crafts.clone(),
                craft,
            )?;
            if attach_definition {
                built.definition = Some(self.built_source(
                    x_def,
                    pre_base_fm,
                    crafts,
                    &built,
                    effective_preference,
                    effective_runtime_kind,
                ));
            }
            return Ok(built);
        }

        if let Some(tier_str) = fm.tier.as_deref() {
            let tier: ModelTier =
                tier_str
                    .parse()
                    .map_err(|error: crate::config::tiers::TierError| {
                        DefinitionErr::new(&self.path, error.to_string())
                    })?;
            let engine = effective_preference
                .family
                .as_deref()
                .filter(|kind| agents::runs_other_providers_models(kind));
            let mut renders = BTreeMap::new();
            let mut exclusions = BTreeMap::new();
            let candidates: Vec<(&str, Option<&str>)> = if let Some(engine) = engine {
                vec![(engine, None)]
            } else {
                self.context
                    .shared
                    .tiers
                    .entries(tier)
                    .filter_map(|(_, kind, model)| kind.map(|kind| (kind, Some(model))))
                    .collect()
            };
            for (kind, candidate_model) in candidates {
                if renders.contains_key(kind) || exclusions.contains_key(kind) {
                    continue;
                }
                let mut candidate_fm = fm.clone();
                candidate_fm.model = candidate_model.map(str::to_owned);
                let variant = effective_variant.filter(|_| kind == effective_kind);
                match finish_profile(
                    &self.context,
                    &self.path,
                    self.namespace,
                    self.seat.as_ref(),
                    kind,
                    variant,
                    false,
                    candidate_fm,
                    crafts.clone(),
                    craft,
                ) {
                    Ok(rendered) => {
                        renders.insert(kind.to_owned(), rendered);
                    }
                    Err(error) => {
                        exclusions.insert(kind.to_owned(), error);
                    }
                }
            }
            let blocked = OnceCell::new();
            let pick = self
                .context
                .shared
                .tiers
                .walk(
                    tier,
                    &effective_preference,
                    |kind| {
                        let error: &DefinitionErr = exclusions.get(kind)?;
                        if matches!(error.cause, DefinitionCause::DependsOnFailed { .. }) {
                            blocked.get_or_init(|| error.cause.clone());
                        }
                        Some(error.message.as_str())
                    },
                    |_, _| None::<()>,
                )
                .map_err(|error| {
                    DefinitionErr::new(
                        &self.path,
                        format!(
                            "{error}; {}",
                            exclusions
                                .iter()
                                .map(|(kind, reason)| format!("{kind}: {}", reason.message))
                                .collect::<Vec<_>>()
                                .join("; ")
                        ),
                    )
                    .with_cause(blocked.get().cloned().unwrap_or(DefinitionCause::Invalid))
                })?;
            let mut built = renders.remove(&pick.kind).ok_or_else(|| {
                DefinitionErr::new(&self.path, format!("no eligible build for {}", pick.kind))
            })?;
            built.model = Some(pick.model);
            built.effort = fm.effort.clone();
            built.model_tier = Some(TierProvenance {
                tier,
                fell_back: pick.used_tier.is_some(),
            });
            if attach_definition {
                built.definition = Some(self.built_source(
                    x_def,
                    fm,
                    crafts,
                    &built,
                    effective_preference,
                    effective_runtime_kind,
                ));
            }
            return Ok(built);
        }

        let kind = effective_kind.as_str();
        let variant = effective_variant;
        if !base_writes_engine
            && let Some(inherited) = fm.model.as_deref()
            && !keeps_inherited_model(&self.natural_kind, kind, inherited)
        {
            fm.model = None;
        }
        let pre_base_fm = fm.clone();
        let mut built = finish_profile(
            &self.context,
            &self.path,
            self.namespace,
            self.seat.as_ref(),
            kind,
            variant,
            false,
            fm,
            crafts.clone(),
            craft,
        )?;
        if attach_definition {
            built.definition = Some(self.built_source(
                x_def,
                pre_base_fm,
                crafts,
                &built,
                effective_preference,
                effective_runtime_kind,
            ));
        }
        Ok(built)
    }
}

#[derive(Clone)]
struct Resolved {
    frontmatter: AgentFrontmatter,
    chain: Vec<String>,
    profile: Profile,
    preference: TierPreference,
    runtime_kind: Option<String>,
}

pub(super) fn empty_profile(kind: &str) -> Profile {
    Profile {
        agent: kind.to_owned(),
        variant: None,
        definition_renders: None,
        definition: None,
        kind_defaults: false,
        kind_base: false,
        tools: None,
        model_tier: None,
        tier_stamp: None,
        isolation: None,
        empty_dir: None,
        skills: None,
        seat_consensus_skill: false,
        mcps: None,
        allowed_tools: None,
        description: None,
        subagents: None,
        model_reminder: None,
        keep_warm: None,
        idle_compact: None,
        smart_compact: None,
        mode: None,
        model: None,
        effort: None,
        fast: None,
        code_mode: None,
        budget: None,
        timeout: None,
        auto_compact: None,
        auto_compact_default: None,
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        args: None,
    }
}

/// Resolves every definition of one tree into `loaded`.
pub(super) fn resolve_namespace(mut resolver: Resolver<'_>, loaded: &mut LoadedDefinitions) {
    for name in resolver
        .tree
        .definitions
        .keys()
        .filter(|name| !name.contains(':'))
    {
        if resolver.resolve(name).is_none() {
            let paths = loaded.failed.entry(name.clone()).or_default();
            paths.insert(resolver.tree.definitions[name].path.clone());
            let mut dependency = name.as_str();
            let mut in_foreign = false;
            let mut seen = BTreeSet::new();
            // Failed qualified ancestors have no launchable name to own their diagnostics.
            while seen.insert((in_foreign, dependency)) {
                let definition = if in_foreign {
                    &resolver.foreign.definitions[dependency]
                } else {
                    &resolver.tree.definitions[dependency]
                };
                if dependency.contains(':') {
                    paths.insert(definition.path.clone());
                }
                let Some(parent) = definition.frontmatter.agent.as_deref() else {
                    break;
                };
                if !in_foreign && matches!(resolver.resolved.get(parent), Some(None)) {
                    dependency = parent;
                } else if resolver.namespace == "subagents"
                    && resolver
                        .foreign_resolver
                        .as_ref()
                        .is_some_and(|foreign| matches!(foreign.resolved.get(parent), Some(None)))
                {
                    in_foreign = true;
                    dependency = parent;
                } else {
                    break;
                }
            }
        }
    }
    loaded.errors.extend(resolver.errors);
    if let Some(foreign) = resolver.foreign_resolver {
        let bare_paths: BTreeSet<&Path> = resolver
            .foreign
            .definitions
            .iter()
            .filter(|(name, _)| !name.contains(':'))
            .map(|(_, def)| def.path.as_path())
            .collect();
        for error in foreign.errors {
            if !bare_paths.contains(error.path.as_path())
                && !loaded
                    .errors
                    .iter()
                    .any(|existing| existing.path == error.path)
            {
                loaded.errors.push(error);
            }
        }
    }
    for (name, resolved) in resolver.resolved {
        if name.contains(':') {
            continue;
        }
        if let Some(resolved) = resolved {
            let definition = &resolver.tree.definitions[&name];
            loaded.insert(
                resolver.namespace,
                &name,
                &definition.path,
                &super::DefinitionOrigin::new(
                    definition.pack.as_ref(),
                    definition.frontmatter.agent.as_deref(),
                ),
                resolved.profile,
            );
        }
    }
}

impl<'a> Resolver<'a> {
    pub(super) fn new(
        scope: LoadScope<'a>,
        namespace: &'static str,
        [tree, foreign]: [&'a Namespace; 2],
    ) -> Self {
        Self {
            scope,
            namespace,
            tree,
            foreign,
            skip_subagents: false,
            foreign_resolver: None,
            seat: None,
            seat_ancestors: Vec::new(),
            seat_crafts: Vec::new(),
            resolved: BTreeMap::new(),
            trail: Vec::new(),
            errors: Vec::new(),
        }
    }

    pub(super) fn with_seat(
        mut self,
        seat: SeatInfo,
        ancestors: Vec<String>,
        crafts: Vec<PromptSource>,
    ) -> Self {
        self.seat = Some(seat);
        self.seat_ancestors = ancestors;
        self.seat_crafts = crafts;
        self
    }
}

/// Team seat identity and frontmatter edits carried across precomputed and on-demand seat builds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SeatInfo {
    pub edits: SeatEdits,
    pub team: String,
    pub role: String,
    pub agent: String,
    pub profile_name: String,
}

/// What a Markdown team seat changes in its resolved frontmatter, applied below the kind base
/// so tools and skills the base supplies are changed too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SeatEdits {
    /// Only the leader keeps the question tool.
    pub leader: bool,
    /// A team running the shipped reflect consensus adds `rimz-reflect` to a non-empty skills
    /// list.
    pub reflects: bool,
}

/// The capability every Markdown team seat's built-in consensus names (`rimz(message)`).
const CONSENSUS_SKILL: &str = "rimz-message";

#[derive(Clone, Copy, Default)]
struct SeatSkillEdits {
    added_consensus: bool,
    added_reflect: bool,
}

impl SeatEdits {
    /// Applies seat edits to resolved frontmatter and reports whether the seat should append an
    /// optional `CONSENSUS_SKILL` entry to `profile.skills` after `skill_policy` checks the
    /// author-written entries (and whether the seat also appended `rimz-reflect`).
    fn edit(self, fm: &mut AgentFrontmatter) -> SeatSkillEdits {
        if !self.leader
            && let Some(tools) = &mut fm.tools
        {
            tools.retain(|tool| {
                agents::canonical_tool_name(tool.split('(').next().unwrap_or("").trim())
                    != "AskUserQuestion"
            });
        }
        let had_skills = fm.skills.as_ref().is_some_and(|skills| !skills.is_empty());
        let mut edits = SeatSkillEdits::default();
        if let Some(capabilities) = &mut fm.capabilities {
            if !self.leader {
                capabilities.retain(|entry| entry.trim() != "ask");
            }
            // Every Markdown team runs the built-in consensus, which names `rimz(message)`.
            // Beside `capabilities:`, RimZ capabilities go only in the grant, even beside
            // `skills: []` where adding one implies `Skill`.
            if !capabilities.iter().any(|entry| {
                matches!(
                    frontmatter::marked_name(entry),
                    CONSENSUS_SKILL | SERVER_NAME
                )
            }) {
                capabilities.push(CONSENSUS_SKILL.to_owned());
            }
        } else {
            if let Some(mcps) = &mut fm.mcps
                && !mcps.iter().any(|entry| {
                    matches!(
                        frontmatter::marked_name(entry),
                        CONSENSUS_SKILL | SERVER_NAME | "*"
                    )
                })
            {
                mcps.push(CONSENSUS_SKILL.to_owned());
            }
            if let Some(skills) = &fm.skills
                && !skills.iter().any(|skill| skill.trim() == CONSENSUS_SKILL)
            {
                edits.added_consensus = true;
            }
        }
        if self.reflects
            && had_skills
            && let Some(skills) = &mut fm.skills
            && !skills.iter().any(|skill| skill.trim() == "rimz-reflect")
        {
            skills.push("rimz-reflect".to_owned());
            edits.added_reflect = true;
        }
        edits
    }
}

pub(super) struct Resolver<'a> {
    scope: LoadScope<'a>,
    namespace: &'static str,
    tree: &'a Namespace,
    foreign: &'a Namespace,
    /// When resolving an `agents/` parent on behalf of a `subagents/` child, the parent's own
    /// `subagents:` list is dropped and never validated for the child.
    skip_subagents: bool,
    foreign_resolver: Option<Box<Resolver<'a>>>,
    seat: Option<SeatInfo>,
    seat_ancestors: Vec<String>,
    seat_crafts: Vec<PromptSource>,
    resolved: BTreeMap<String, Option<Resolved>>,
    trail: Vec<String>,
    errors: Vec<DefinitionErr>,
}

/// The names of `resolver`'s tree that load and the errors of the rest, recording neither.
pub(super) fn loadable(mut resolver: Resolver<'_>) -> (BTreeSet<String>, Vec<DefinitionErr>) {
    let names: Vec<String> = resolver
        .tree
        .definitions
        .keys()
        .filter(|name| !name.contains(':'))
        .cloned()
        .collect();
    let loaded = names
        .into_iter()
        .filter(|name| resolver.resolve(name).is_some())
        .collect();
    if let Some(foreign) = resolver.foreign_resolver {
        resolver.errors.extend(foreign.errors);
    }
    (loaded, resolver.errors)
}

impl Resolver<'_> {
    fn resolve(&mut self, name: &str) -> Option<Resolved> {
        if self.tree.failed.contains(name) {
            return None;
        }
        if let Some(resolved) = self.resolved.get(name) {
            return resolved.clone();
        }
        if self.trail.iter().any(|entry| entry == name) {
            let mut trail = self.trail.clone();
            trail.push(name.to_owned());
            self.errors.push(DefinitionErr::new(
                &self.tree.definitions[name].path,
                format!("follows itself through `agent:` ({})", trail.join(" -> ")),
            ));
            return None;
        }
        self.trail.push(name.to_owned());
        let result = self.materialize(name);
        self.trail.pop();
        let resolved = match result {
            Ok(resolved) => Some(resolved),
            Err(error) => {
                if !self
                    .errors
                    .iter()
                    .any(|previous| previous.path == error.path)
                {
                    self.errors.push(error);
                }
                None
            }
        };
        self.resolved.insert(name.to_owned(), resolved.clone());
        resolved
    }

    fn resolve_foreign(&mut self, name: &str) -> Option<Resolved> {
        let scope = self.scope;
        let tree = self.foreign;
        let foreign = self.tree;
        self.foreign_resolver
            .get_or_insert_with(|| {
                let mut resolver = Resolver::new(scope, "agents", [tree, foreign]);
                resolver.skip_subagents = true;
                Box::new(resolver)
            })
            .resolve(name)
    }

    fn materialize(&mut self, name: &str) -> Result<Resolved, DefinitionErr> {
        let definition = &self.tree.definitions[name];
        let path = &definition.path;
        let mut fm = definition.frontmatter.clone();
        if self.skip_subagents {
            fm.subagents = None;
        }
        if fm.model.is_some() && fm.tier.is_some() {
            return Err(DefinitionErr::new(
                path,
                "sets both `tier:` and `model:`; keep one",
            ));
        }
        if let Some(tier) = fm.model.as_deref().and_then(ModelTier::from_model) {
            return Err(DefinitionErr::new(
                path,
                format!("model is a tier; use `tier: {tier}`"),
            ));
        }
        let own_kind = fm
            .agent
            .as_deref()
            .and_then(agents::find_runtime)
            .map(agents::Runtime::kind);
        let model_kind = fm.model.as_deref().and_then(|model| {
            agents::definition_model_kind(model)
                .filter(|_| !own_kind.is_some_and(|kind| agents::keeps_model(kind, model)))
        });
        let mut runtime_kind = model_kind.or(own_kind).map(str::to_owned);
        if let (Some(kind), Some(implied)) = (own_kind, model_kind)
            && kind != implied
        {
            return Err(DefinitionErr::new(
                path,
                format!(
                    "runs on '{kind}' but model '{}' runs on '{implied}'; set a {kind} model or follow the {implied} base",
                    fm.model.as_deref().unwrap_or_default()
                ),
            ));
        }
        let own_runtime = fm.model.is_some() || fm.tier.is_some() || own_kind.is_some();
        let mut preference = TierPreference {
            model: fm.model.clone(),
            family: model_kind.or(own_kind).map(str::to_owned),
        };
        let parent_name = fm
            .agent
            .clone()
            .or_else(|| {
                fm.model
                    .as_deref()
                    .and_then(agents::definition_model_kind)
                    .map(str::to_owned)
            })
            .or_else(|| fm.tier.as_ref().map(|_| String::new()))
            .ok_or_else(|| {
                DefinitionErr::new(
                    path,
                    format!(
                        "has no `agent:` and {}; set `agent:`",
                        fm.model.as_ref().map_or_else(
                            || "it sets no model".to_owned(),
                            |model| format!("model '{model}' names no runtime")
                        )
                    ),
                )
            })?;
        let mut chain = match &self.seat {
            Some(seat) => {
                let mut chain = vec![seat.profile_name.clone()];
                for ancestor in &self.seat_ancestors {
                    if !chain.contains(ancestor) {
                        chain.push(ancestor.clone());
                    }
                }
                chain
            }
            None => vec![name.to_owned()],
        };
        let mut profile = if parent_name.is_empty() {
            let mut profile = empty_profile("");
            if !self.seat_crafts.is_empty() {
                profile.append_system_prompt_files = self.seat_crafts.clone();
            }
            profile
        } else if let Some(runtime) = agents::find_runtime(&parent_name) {
            let mut profile = empty_profile(runtime.kind());
            profile.variant = runtime.variant;
            if !self.seat_crafts.is_empty() {
                profile.append_system_prompt_files = self.seat_crafts.clone();
            }
            profile
        } else {
            let in_own = self.tree.problems.contains_key(&parent_name)
                || self.tree.definitions.contains_key(&parent_name)
                || self.tree.failed.contains(&parent_name);
            let in_foreign = self.namespace == "subagents"
                && (self.foreign.problems.contains_key(&parent_name)
                    || self.foreign.definitions.contains_key(&parent_name)
                    || self.foreign.failed.contains(&parent_name));
            if in_own || in_foreign {
                let parent = if in_own {
                    self.tree.lookup(&parent_name, path)?;
                    self.resolve(&parent_name)
                } else {
                    self.foreign.lookup(&parent_name, path)?;
                    self.resolve_foreign(&parent_name)
                }
                .ok_or_else(|| {
                    DefinitionErr::new(
                        path,
                        format!("follows `{parent_name}`, which failed to load"),
                    )
                    .with_cause(DefinitionCause::DependsOnFailed {
                        name: parent_name.clone(),
                    })
                })?;
                if in_foreign {
                    let parent_fm = parent.frontmatter.clone().for_namespace(self.namespace);
                    fm.inherit(&parent_fm);
                } else {
                    fm.inherit(&parent.frontmatter);
                }
                for ancestor in &parent.chain {
                    if !chain.contains(ancestor) {
                        chain.push(ancestor.clone());
                    }
                }
                if runtime_kind.is_none() {
                    runtime_kind = parent.runtime_kind;
                }
                if !own_runtime {
                    preference = parent.preference;
                }
                let mut profile = empty_profile(&parent.profile.agent);
                profile.variant = parent.profile.variant;
                profile.append_system_prompt_files = parent.profile.append_system_prompt_files;
                profile
            } else {
                self.tree.lookup(&parent_name, path)?;
                let hint = if self.foreign.definitions.contains_key(&parent_name)
                    || self.foreign.failed.contains(&parent_name)
                {
                    format!("; rimz resolves `agent:` within {}", self.namespace)
                } else {
                    String::new()
                };
                return Err(DefinitionErr::new(
                    path,
                    format!("follows unknown profile '{parent_name}'{hint}"),
                ));
            }
        };
        let unresolved = fm.clone();
        if let Some(shift) = fm
            .effort
            .as_deref()
            .filter(|effort| super::super::tiers::relative_effort(effort))
        {
            let source = if definition.frontmatter.effort.is_some() {
                String::new()
            } else {
                format!(" (inherited from '{}')", self.effort_source(&parent_name))
            };
            return Err(DefinitionErr::new(
                path,
                format!(
                    "effort {shift}{source} is relative; set an absolute effort or omit it for the chosen model's default"
                ),
            ));
        }
        if agents::runs_other_providers_models(&profile.agent) {
            runtime_kind = Some(profile.agent.clone());
            preference.family = Some(profile.agent.clone());
        } else if let Some(kind) = fm.model.as_deref().and_then(|model| {
            agents::definition_model_kind(model)
                .filter(|_| !agents::keeps_model(&profile.agent, model))
        }) {
            if profile.agent != kind {
                profile.variant = None;
            }
            profile.agent = kind.to_owned();
        } else if fm.model.is_some() {
            profile.agent = runtime_kind.clone().ok_or_else(|| {
                DefinitionErr::new(
                    path,
                    "model names no runtime and the chain has no family; set `agent:`",
                )
            })?;
        }
        let tier = fm
            .tier
            .as_deref()
            .map(str::parse::<ModelTier>)
            .transpose()
            .map_err(|error| DefinitionErr::new(path, error.to_string()))?;
        let mut craft_state = CraftState::Pending {
            traits: self.scope.traits,
            body: &definition.body,
            trait_names: definition.frontmatter.traits.as_deref().unwrap_or_default(),
            cached: None,
        };
        let mut source = DefinitionSourceInner {
            name: self
                .seat
                .as_ref()
                .map_or_else(|| name.to_owned(), |seat| seat.profile_name.clone()),
            path: path.clone(),
            namespace: self.namespace,
            chain: chain.clone(),
            frontmatter: unresolved.clone(),
            crafts: profile.append_system_prompt_files.clone(),
            natural_kind: profile.agent.clone(),
            natural_variant: profile.variant,
            preference: preference.clone(),
            runtime_kind: runtime_kind.clone(),
            seat: self.seat.clone(),
            context: Arc::clone(self.scope.context),
            memo: Mutex::default(),
        };
        let mut renders = BTreeMap::new();
        let mut exclusions = BTreeMap::new();
        let candidates = self
            .scope
            .context
            .shared
            .tiers
            .entries(ModelTier::Intern)
            .filter_map(|(_, kind, model)| kind.map(|kind| (kind, Some(model))))
            .chain(
                agents::known_kinds()
                    .filter(|kind| agents::runs_other_providers_models(kind))
                    .map(|kind| (kind, fm.model.as_deref())),
            );
        for (kind, model) in candidates {
            if renders.contains_key(kind) || exclusions.contains_key(kind) {
                continue;
            }
            match source.build_with_craft(None, Some(kind), model, &mut craft_state, false) {
                Ok(rendered) => {
                    renders.insert(kind.to_owned(), rendered);
                }
                Err(error) => {
                    exclusions.insert(kind.to_owned(), error);
                }
            }
        }
        let mut profile = if let Some(tier) = tier {
            // Any one model clears the tier, so the first failed dependency met is the one to fix.
            let blocked = OnceCell::new();
            let pick = self
                .scope
                .context
                .shared
                .tiers
                .walk(
                    tier,
                    &preference,
                    |kind| {
                        let error: &DefinitionErr = exclusions.get(kind)?;
                        if matches!(error.cause, DefinitionCause::DependsOnFailed { .. }) {
                            blocked.get_or_init(|| error.cause.clone());
                        }
                        Some(error.message.as_str())
                    },
                    |_, _| None::<()>,
                )
                .map_err(|error| {
                    DefinitionErr::new(
                        path,
                        format!(
                            "{error}; {}",
                            exclusions
                                .iter()
                                .map(|(kind, reason)| format!("{kind}: {}", reason.message))
                                .collect::<Vec<_>>()
                                .join("; ")
                        ),
                    )
                    .with_cause(blocked.get().cloned().unwrap_or(DefinitionCause::Invalid))
                })?;
            // The probe covers native families and model-client engines; the walk skips exclusions.
            let mut selected = renders[&pick.kind].clone();
            selected.model = Some(pick.model.clone());
            selected.effort = fm.effort.clone();
            selected.model_tier = Some(TierProvenance {
                tier,
                fell_back: pick.used_tier.is_some(),
            });
            selected
        } else {
            source
                .build_with_craft(None, None, None, &mut craft_state, false)
                .map_err(|mut error| {
                    if let Some(tier) = fm.model.as_deref().and_then(|model| {
                        agents::definition_model_kind(model).and_then(|kind| {
                            self.scope.context.shared.tiers.tier_for_model(kind, model)
                        })
                    }) {
                        error.message.push_str(&format!(
                            "; use `tier: {tier}` to allow another model or family"
                        ));
                    }
                    error
                })?
        };
        source.crafts = profile.append_system_prompt_files.clone();
        source.natural_kind = profile.agent.clone();
        source.natural_variant = profile.variant;
        let def_source = DefinitionSource {
            inner: Arc::new(source),
        };
        for rendered in renders.values_mut() {
            rendered.definition = Some(def_source.clone());
        }
        profile.definition = Some(def_source);
        profile.definition_renders = Some(DefinitionRenders {
            preference: preference.clone(),
            renders,
            exclusions: exclusions
                .into_iter()
                .map(|(kind, error)| (kind, error.message))
                .collect(),
            effort: fm.effort,
        });
        Ok(Resolved {
            frontmatter: unresolved,
            chain,
            profile,
            preference,
            runtime_kind,
        })
    }

    /// The definition along `name`'s `agent:` chain whose own frontmatter sets `effort:`.
    fn effort_source<'n>(&'n self, mut name: &'n str) -> &'n str {
        let mut tree = self.tree;
        loop {
            let definition = match tree.definitions.get(name) {
                Some(definition) => definition,
                None if self.namespace == "subagents"
                    && std::ptr::eq(tree, self.tree)
                    && self.foreign.definitions.contains_key(name) =>
                {
                    tree = self.foreign;
                    &self.foreign.definitions[name]
                }
                None => return name,
            };
            let own = &definition.frontmatter;
            match own.agent.as_deref() {
                Some(parent)
                    if own.effort.is_none()
                        && (tree.definitions.contains_key(parent)
                            || (self.namespace == "subagents"
                                && std::ptr::eq(tree, self.tree)
                                && self.foreign.definitions.contains_key(parent))) =>
                {
                    name = parent;
                }
                _ => return name,
            }
        }
    }

    /// The kind base `name` as its row in this resolver's namespace: validated as a profile of
    /// its kind, less the fields the namespace refuses, its rendered body the system prompt.
    pub(super) fn kind_base(&self, name: &str) -> Result<Profile, DefinitionErr> {
        let definition = &self.tree.definitions[name];
        let fm = definition.frontmatter.clone().for_namespace(self.namespace);
        let mut craft_state = CraftState::Pending {
            traits: self.scope.traits,
            body: &definition.body,
            trait_names: definition.frontmatter.traits.as_deref().unwrap_or_default(),
            cached: None,
        };
        let mut profile = finish_profile(
            self.scope.context,
            &definition.path,
            self.namespace,
            None,
            name,
            None,
            true,
            fm,
            Vec::new(),
            &mut craft_state,
        )?;
        profile.system_prompt_file = profile.append_system_prompt_files.pop();
        if profile.system_prompt_file.is_none() {
            return Err(DefinitionErr::new(
                &definition.path,
                super::empty_kind_base(name),
            ));
        }
        // Its tuning fills below written values at launch, so it sets no definition defaults.
        profile.kind_defaults = false;
        profile.auto_compact_default = None;
        Ok(profile)
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "single lowering boundary shared by initial load, kind bases, seats, and on-demand builds"
)]
fn finish_profile(
    context: &LoadContext,
    path: &Path,
    namespace: &str,
    seat: Option<&SeatInfo>,
    kind: &str,
    variant: Option<&'static agents::Variant>,
    kind_base: bool,
    mut fm: AgentFrontmatter,
    crafts: Vec<PromptSource>,
    craft_state: &mut CraftState<'_>,
) -> Result<Profile, DefinitionErr> {
    let shared = &context.shared;
    let mut profile = empty_profile(kind);
    profile.variant = variant;
    profile.kind_base = kind_base;
    profile.append_system_prompt_files = crafts;
    let takes_system_prompt = agents::find_definition(kind)
        .is_some_and(|definition| definition.spec().launch.takes_system_prompt());
    if let Some(error) = shared.failed_bases.get(kind) {
        let message = if error.is_conflict_over(kind) {
            error.message.clone()
        } else if takes_system_prompt {
            format!("runs on {kind}, {}", super::failed_kind_base(&error.path))
        } else {
            format!(
                "runs on {kind}, which takes no system prompt; its kind base {} failed to \
                 load — delete that file",
                error.path.display()
            )
        };
        return Err(DefinitionErr::new(path, message).with_cause(
            DefinitionCause::DependsOnFailed {
                name: kind.to_owned(),
            },
        ));
    }
    if !profile.kind_base
        && let Some(base) = shared.bases.get(kind)
    {
        fm.inherit_base(base, namespace, lists_agent_tool(&fm));
    }
    fm.settle_plus(kind)
        .map_err(|message| DefinitionErr::new(path, message))?;
    let seat_edits = seat.map_or_else(SeatSkillEdits::default, |seat| seat.edits.edit(&mut fm));
    profile.allowed_tools = fm
        .allowed_tools
        .as_ref()
        .map(|rules| {
            let mut parsed = Vec::new();
            for rule in rules {
                let rule = rule
                    .parse::<crate::config::ToolRule>()
                    .map_err(|error| DefinitionErr::new(path, error.to_string()))?;
                if !parsed.contains(&rule) {
                    parsed.push(rule);
                }
            }
            Ok::<_, DefinitionErr>(parsed)
        })
        .transpose()?;
    if let Some(definition) = agents::find_definition(kind)
        && fm.model.is_some()
        && !definition.spec().launch.supports_preset(PresetField::Model)
    {
        return Err(DefinitionErr::new(
            path,
            agents::PresetErr::UnsupportedField {
                agent: definition.spec().kind,
                field: "model",
            }
            .to_string(),
        ));
    }
    if let Some(model) = fm.model.as_deref()
        && let Some(implied) = agents::definition_model_kind(model)
        && implied != kind
        && !agents::keeps_model(kind, model)
    {
        return Err(DefinitionErr::new(
            path,
            format!(
                "runs on '{kind}' but model '{model}' runs on '{implied}'; set a {kind} model or follow the {implied} base"
            ),
        ));
    }
    let lowered = match fm.capabilities.as_deref() {
        Some(written) => {
            for (key, listed) in [
                ("skills", fm.skills.as_deref()),
                ("mcps", fm.mcps.as_deref()),
            ] {
                let rimz: Vec<String> = listed
                    .into_iter()
                    .flatten()
                    .map(|entry| entry.trim())
                    .filter(|entry| {
                        let name = frontmatter::marked_name(entry);
                        Capability::from_name(name).is_some()
                            || (key == "mcps" && (name == SERVER_NAME || name.starts_with("rimz-")))
                    })
                    .map(|entry| format!("`{entry}`"))
                    .collect();
                if !rimz.is_empty() {
                    return Err(DefinitionErr::new(
                        path,
                        format!(
                            "sets `capabilities:`, and its `{key}:` lists {}; beside \
                             `capabilities:` RimZ's MCP capabilities go only in \
                             `capabilities:`, so list them there instead",
                            rimz.join(", ")
                        ),
                    ));
                }
            }
            Some(lower_capabilities(path, written, fm.skills.as_deref())?)
        }
        None => None,
    };
    if lowered.is_none() && fm.tools.is_none() && agents::tools_required(kind) && !profile.kind_base
    {
        return Err(DefinitionErr::new(
            path,
            format!("lists neither `capabilities` nor `tools`, which {kind} needs; write one"),
        ));
    }
    let tools = match lowered {
        Some(lowered) => Some(lowered),
        None => fm
            .tools
            .as_deref()
            .map(ToolSet::parse)
            .transpose()
            .map_err(|error| DefinitionErr::new(path, error.to_string()))?,
    };
    // A kind base may leave a required list to each definition.
    if tools.is_some() || !profile.kind_base {
        agents::render_tool_args(kind, tools.as_ref())
            .map_err(|error| DefinitionErr::new(path, error.to_string()))?;
    }
    if fm.idle_compact.is_some() && namespace == "subagents" {
        return Err(DefinitionErr::new(
            path,
            "sets `idle-compact:`, which a subagent never needs; remove it",
        ));
    }
    profile.idle_compact = fm.idle_compact;
    profile.smart_compact = fm.smart_compact;
    if let Some(listed) = fm.subagents.as_ref() {
        if namespace == "subagents" {
            return Err(DefinitionErr::new(
                path,
                "sets `subagents:`, which only a direct profile takes; a child cannot launch again",
            ));
        }
        if tools.as_ref().is_some_and(|tools| tools.has("Agent")) {
            return Err(DefinitionErr::new(
                path,
                "sets `subagents:` and lists the Agent tool; the rimz doorway replaces the native one, so drop whichever this seat does not use",
            ));
        }
        // A `+` list nothing below added to leaves the field absent, which admits every child.
        if listed.iter().any(|entry| frontmatter::is_plus(entry)) {
            profile.subagents = None;
        } else {
            // A kind base's names check once the subagents tree has resolved.
            profile.subagents = Some(if profile.kind_base {
                names(path, "subagents", listed)?
            } else if let Some(subagents) = context.subagents.as_deref() {
                subagent_names(path, listed, subagents)?
            } else {
                names(path, "subagents", listed)?
            });
        }
    }
    let skill_check = match shared.skills.check.as_ref() {
        SkillCheck::Check {
            machine_isolation, ..
        } if (!cfg!(target_os = "linux")
            || !Isolation::resolve(None, fm.isolation, machine_isolation).is_scoped())
            && !agents::find_definition(kind).is_some_and(|definition| {
                matches!(
                    definition.spec().host_skills,
                    agents::skills::HostSkills::Switch { .. }
                )
            }) =>
        {
            SkillCheck::Skip
        }
        check => check,
    };
    profile.skills = match fm.skills.as_deref() {
        // `capabilities:` beside `skills: []` keeps the empty list; its RimZ capabilities
        // live in the grant.
        Some([]) if fm.capabilities.is_some() => Some(Vec::new()),
        listed => skill_policy(
            path,
            kind,
            listed,
            tools.as_ref(),
            skill_check,
            &shared.skills,
        )?,
    };
    if seat_edits.added_consensus
        && let Some(skills) = &mut profile.skills
    {
        // Impossible: `CONSENSUS_SKILL` ("rimz-message") is a valid bare skill name.
        let skill = CONSENSUS_SKILL
            .parse()
            .expect("built-in consensus skill name is a valid SkillName");
        let index = skills
            .len()
            .saturating_sub(usize::from(seat_edits.added_reflect));
        skills.insert(index, skill);
        profile.seat_consensus_skill = true;
    }
    profile.mcps = fm
        .mcps
        .as_deref()
        .map(|listed| mcp_names(path, listed))
        .transpose()?;
    profile.tools = tools;
    profile.kind_defaults = true;
    profile.isolation = fm.isolation;
    profile.empty_dir = fm.empty_dir;
    profile.mode = fm.mode;
    profile.model = fm.model.clone();
    profile.effort = fm.effort.clone();
    profile.auto_compact = fm
        .auto_compact
        .clone()
        .map(|value| auto_compact(path, &value))
        .transpose()?;
    profile.auto_compact_default =
        super::super::agents::native_auto_compact(shared.native_auto_compact, kind);
    profile.fast = fm.fast;
    profile.code_mode = fm.code_mode;
    if let Some(raw) = fm.budget.as_deref() {
        raw.parse::<crate::harness::budget::BudgetSpec>()
            .map_err(|error| DefinitionErr::new(path, error.to_string()))?;
    }
    profile.budget = fm.budget.clone();
    if let Some(value) = fm.timeout.as_deref() {
        if namespace != "subagents" {
            return Err(DefinitionErr::new(
                path,
                "sets `timeout:`, which only a subagent definition takes; remove it, or move \
                 the definition to subagents/",
            ));
        }
        let timeout = crate::utils::time::parse_compact_duration(value).map_err(|_| {
            DefinitionErr::new(
                path,
                format!(
                    "sets `timeout: {value}`, which is not a duration; write one whole \
                     number and a unit, s, m, h, or d, such as 90m"
                ),
            )
        })?;
        profile.timeout = Some(timeout);
    }
    if let Some(crate::config::KeepWarmPolicy::Block(fields)) = &fm.keep_warm
        && let Some(clause) = &fields.while_clause
    {
        return Err(DefinitionErr::new(
            path,
            format!(
                "profile '{}' keep-warm while `{clause}`: a solo definition has no board; remove `while` and use `max` only",
                path.file_stem().unwrap_or_default().to_string_lossy()
            ),
        ));
    }
    profile.keep_warm = fm.keep_warm.clone();
    profile.model_reminder = fm.model_reminder;
    profile.description = Some(frontmatter::description(path, fm.description.as_deref())?);
    if let CraftState::Pending {
        traits,
        body,
        trait_names,
        cached,
    } = craft_state
    {
        let craft = cached
            .get_or_insert_with(|| {
                traits::render(
                    &shared.home,
                    traits,
                    path,
                    body,
                    trait_names,
                    &shared.skills,
                )
            })
            .clone()?;
        if !craft.is_empty() {
            profile.append_system_prompt_files.push(PromptSource::Text {
                origin: path.to_path_buf(),
                text: craft,
            });
        }
    }
    if let Some(seat) = seat {
        if profile.empty_dir == Some(true) {
            return Err(DefinitionErr::new(
                path,
                format!(
                    "team '{}' selects definition '{}' with empty-dir; teams share the worktree",
                    seat.team, seat.agent
                ),
            ));
        }
        if !takes_system_prompt {
            return Err(DefinitionErr::new(
                path,
                format!(
                    "team '{}' role '{}' runs on {kind}, which takes no system prompt; \
                     every seat carries the team's pipeline in its system prompt — select a \
                     definition on a kind that takes a system prompt",
                    seat.team, seat.role
                ),
            ));
        }
    }
    if !takes_system_prompt && !profile.append_system_prompt_files.is_empty() {
        return Err(DefinitionErr::new(
            path,
            format!(
                "runs on {kind}, which takes no system prompt, so its craft body cannot be \
                 delivered; remove the body or run this definition on a kind that takes a \
                 system prompt"
            ),
        ));
    }
    // Prompt-taking kinds need their base even when the definition adds no craft.
    if takes_system_prompt && !shared.bases.contains_key(kind) {
        return Err(DefinitionErr::new(
            path,
            format!(
                "runs on {kind}, {}",
                super::missing_kind_base(&shared.home, kind)
            ),
        ));
    }
    Ok(profile)
}

/// A `subagents:` list's names, each a subagent in `subagents.allowed` (those that loaded, and
/// the `[agents.commands]` names), a kind, or `general`.
pub(super) fn subagent_names(
    path: &Path,
    listed: &[String],
    subagents: &SubagentCheck,
) -> Result<Vec<String>, DefinitionErr> {
    let names = names(path, "subagents", listed)?;
    for name in &names {
        subagents.check_lookup(name, path)?;
    }
    if let Some(failed) = names.iter().find(|name| {
        (subagents.defined.contains(*name) || subagents.failed.contains(*name))
            && !subagents.allowed.contains(*name)
    }) {
        return Err(DefinitionErr::new(
            path,
            format!("allows subagent '{failed}', which failed to load"),
        )
        .with_cause(DefinitionCause::DependsOnFailed {
            name: failed.clone(),
        }));
    }
    let unknown: Vec<_> = names
        .iter()
        .filter(|name| {
            name.as_str() != "general"
                && agents::find_definition(name).is_none()
                && !subagents.allowed.contains(*name)
        })
        .collect();
    if !unknown.is_empty() {
        return Err(DefinitionErr::new(
            path,
            format!("allows unknown subagent profile(s) {unknown:?}"),
        ));
    }
    Ok(names)
}

fn names(path: &Path, key: &str, listed: &[String]) -> Result<Vec<String>, DefinitionErr> {
    let mut names = Vec::new();
    for name in listed {
        let name = name.trim();
        if name.is_empty() {
            return Err(DefinitionErr::new(
                path,
                format!("sets `{key}:` to something other than a list of names"),
            ));
        }
        if !names.iter().any(|existing| existing == name) {
            names.push(name.to_owned());
        }
    }
    Ok(names)
}

fn mcp_names(path: &Path, listed: &[String]) -> Result<Vec<crate::config::McpName>, DefinitionErr> {
    let refuse = |error: crate::config::McpListErr| {
        DefinitionErr::new(path, format!("sets `mcps:` with an {error}"))
    };
    let mut names: Vec<crate::config::McpName> = Vec::new();
    for name in listed {
        let name = name.trim().parse().map_err(refuse)?;
        crate::config::mcps::push_mcp_entry(&mut names, name).map_err(refuse)?;
    }
    Ok(names)
}

pub(super) fn auto_compact(path: &Path, value: &str) -> Result<String, DefinitionErr> {
    let text = value.trim();
    if crate::store::message::AutoCompact::parse_native_window(text).is_err() {
        return Err(DefinitionErr::new(
            path,
            format!(
                "sets `auto-compact: {value}`; rimz takes a token count from 100k through 1M, spelled '200k', '200000', or '1m'"
            ),
        ));
    }
    Ok(text.to_owned())
}

/// The words of a `capabilities:` list, each with the tool bases it lowers to.
const WORDS: [(&str, &[&str]); 5] = [
    ("bash", &["Bash"]),
    ("read", &["Read", "Grep", "Glob", "LS"]),
    ("write", &["Edit", "Write"]),
    ("ask", &["AskUserQuestion"]),
    ("web", &["WebSearch", "WebFetch"]),
];

/// Whether a `capabilities:` list implies the Skill tool: unless `skills` is `[]` and no
/// entry grants a RimZ capability.
fn skill_implied(capabilities: &[String], skills: Option<&[String]>) -> bool {
    skills.is_none_or(|skills| !skills.is_empty())
        || capabilities.iter().any(|entry| {
            let name = frontmatter::marked_name(entry);
            name == SERVER_NAME || name.starts_with("rimz-")
        })
}

/// Lowers a resolved `capabilities:` list to the tools value it stands for: each word's tool
/// bases in written order, then `Skill` where [`skill_implied`], carrying each listed
/// capability with its own mark as the RimZ grant. `rimz` adds every capability left unlisted,
/// then itself with its mark, which marks each capability that has none of its own
/// (`ExecRequest::served_marks`) and reaches a provider's MCP code as it does from `mcps`.
fn lower_capabilities(
    path: &Path,
    written: &[String],
    skills: Option<&[String]>,
) -> Result<ToolSet, DefinitionErr> {
    let mut bases: Vec<String> = Vec::new();
    let mut listed: Vec<(&str, Option<LoadMode>)> = Vec::new();
    let names: Vec<&str> = Capability::ALL
        .iter()
        .map(|capability| capability.name())
        .collect();
    for entry in written.iter().map(|entry| entry.trim()) {
        let name = frontmatter::marked_name(entry);
        if let Some((_, tools)) = WORDS.iter().find(|(word, _)| *word == name) {
            if name != entry {
                return Err(DefinitionErr::new(
                    path,
                    format!(
                        "lists `{entry}` in `capabilities:`, but a load mark applies only to \
                         `rimz` and a capability with MCP tools ({}); drop the mark",
                        names.join(", ")
                    ),
                ));
            }
            for tool in *tools {
                if !bases.iter().any(|base| base == tool) {
                    bases.push((*tool).to_owned());
                }
            }
            continue;
        }
        let parsed = (name == SERVER_NAME)
            .then_some(name)
            .or_else(|| Capability::from_name(name).map(Capability::name));
        let mode = match entry.rsplit_once(':') {
            Some((_, mark)) => LoadMode::from_name(mark.trim()),
            None => None,
        };
        let marked_well = mode.is_some() || name == entry;
        let Some(name) = parsed.filter(|_| marked_well) else {
            let skill = name.parse::<crate::config::RimzEntry>().is_ok();
            let reason = if skill && marked_well {
                format!(
                    "lists `{entry}` in `capabilities:`, which holds only RimZ's capabilities \
                     with MCP tools ({}), or `rimz` for all of them; `{name}` is a skill, so \
                     list it in `skills:` instead",
                    names.join(", ")
                )
            } else {
                format!(
                    "lists `{entry}` in `capabilities:`, which is neither one of the words \
                     bash, read, write, ask, and web, nor `rimz` or a RimZ capability ({}), \
                     each with an optional `:always` or `:deferred`; a provider's own tool \
                     names go in `tools:`",
                    names.join(", ")
                )
            };
            return Err(DefinitionErr::new(path, reason));
        };
        match listed.iter().find(|(held, _)| *held == name) {
            Some((_, held)) if *held == mode => {}
            Some(_) => {
                return Err(DefinitionErr::new(
                    path,
                    format!("lists `{name}` in `capabilities:` with two different marks; keep one"),
                ));
            }
            None => listed.push((name, mode)),
        }
    }
    let wildcard = listed
        .iter()
        .find(|(name, _)| *name == SERVER_NAME)
        .map(|(_, mode)| *mode);
    let mut grant: Vec<crate::config::RimzEntry> = listed
        .iter()
        .filter_map(|(name, own)| {
            Some(crate::config::RimzEntry::new(
                Capability::from_name(name)?,
                *own,
            ))
        })
        .collect();
    if let Some(mark) = wildcard {
        let rest = Capability::ALL
            .into_iter()
            .filter(|capability| !listed.iter().any(|(name, _)| *name == capability.name()))
            .map(|capability| crate::config::RimzEntry::new(capability, None));
        grant.extend(rest);
        grant.push(crate::config::RimzEntry::server(mark));
    }
    if skill_implied(written, skills) {
        bases.push("Skill".to_owned());
    }
    Ok(ToolSet::parse(&bases)
        .map_err(|error| DefinitionErr::new(path, error.to_string()))?
        .with_rimz(grant))
}

/// Under [`SkillCheck::Check`], each listed skill resolves as the scoped view finds it:
/// provider root, library, then packs. The winning `SKILL.md` carries the marker check;
/// an unreadable copy fails rather than letting a shadowed copy speak for it.
pub(super) fn skill_policy(
    path: &Path,
    kind: &str,
    listed: Option<&[String]>,
    tools: Option<&ToolSet>,
    check: SkillCheck<'_>,
    catalog: &SkillCatalog,
) -> Result<Option<Vec<SkillName>>, DefinitionErr> {
    let Some(listed) = listed else {
        return Ok(None);
    };
    if agents::skills_lack_skill_tool(kind, tools) {
        return Err(DefinitionErr::new(
            path,
            "sets `skills:` and lists no Skill tool; the list governs which skills that tool reaches on its own",
        ));
    }
    let mut skills = Vec::new();
    let definition = agents::find_definition(kind);
    let discovered = match check {
        SkillCheck::Check { .. } => catalog
            .enumerate(kind)
            .map_err(|error| DefinitionErr::new(path, error))?,
        SkillCheck::Skip => None,
    };
    for name in names(path, "skills", listed)? {
        let skill: SkillName = name.parse().map_err(|_| DefinitionErr::new(path, format!("lists skill {name:?}; rimz reads one bare directory name, so a `<name>:<mode>` suffix, a path, or whitespace is refused")))?;
        if let SkillCheck::Check { env, .. } = check {
            let roots: Vec<PathBuf> = definition
                .and_then(|definition| definition.skills_home(env))
                .into_iter()
                .chain(catalog.chain.roots())
                .collect();
            if let Some(conflict) = discovered
                .as_ref()
                .and_then(|skills| skills.conflicts.get(&name))
            {
                return Err(DefinitionErr::new(path, &conflict.message));
            }
            let Some(found) = discovered
                .as_ref()
                .and_then(|skills| skills.skills.get(skill.as_str()))
            else {
                let searched: Vec<String> = roots
                    .iter()
                    .map(|root| root.join(&name).join("SKILL.md").display().to_string())
                    .collect();
                return Err(DefinitionErr::new(
                    path,
                    format!(
                        "lists skill '{name}', missing at {}; add it under {} or remove it from \
                         the `skills:` that lists it",
                        searched.join(" and "),
                        catalog.chain.library.display()
                    ),
                )
                .with_cause(DefinitionCause::MissingSkill { skill, roots }));
            };
            let skill_path = found.source.join("SKILL.md");
            let marker = definition.map_or(ManualSkill::Unsupported, |definition| {
                definition.manual_skill()
            });
            let manual =
                agents::skills::is_user_only(&found.source, marker, |file| catalog.read_text(file))
                    .map_err(|error| match error {
                        agents::skills::SkillMarkerErr::Unreadable {
                            path: file,
                            message,
                        } => DefinitionErr::new(
                            path,
                            format!(
                                "lists skill '{name}', unreadable at {}: {message}",
                                file.display()
                            ),
                        ),
                        agents::skills::SkillMarkerErr::Invalid {
                            path: file,
                            message,
                        } => DefinitionErr::new(
                            path,
                            format!(
                                "lists skill '{name}', invalid at {}: {message}",
                                file.display()
                            ),
                        ),
                    })?;
            if manual {
                let (marker_file, marker_line) = if matches!(marker, ManualSkill::OpenAiPolicy) {
                    (
                        found.source.join("agents/openai.yaml"),
                        "policy.allow_implicit_invocation: false",
                    )
                } else {
                    (skill_path, "disable-model-invocation: true")
                };
                return Err(DefinitionErr::new(
                    path,
                    format!(
                        "lists skill '{name}', which {} marks user-only for {kind} (`{marker_line}`); listing cannot lift that marker: drop it from `skills:` or remove the marker",
                        marker_file.display()
                    ),
                ));
            }
        }
        skills.push(skill);
    }
    Ok(Some(skills))
}
