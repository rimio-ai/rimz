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

/// Maximum inheritance chain length for project TOML profiles.
const MAX_PROFILE_DEPTH: usize = 16;

/// Chain member names that wrote each tuning field on a definition.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TuningWriters {
    pub effort: Option<String>,
    pub fast: Option<String>,
    pub code_mode: Option<String>,
    pub auto_compact: Option<String>,
}

impl TuningWriters {
    fn from_frontmatter(fm: &AgentFrontmatter, name: &str) -> Self {
        Self {
            effort: fm.effort.as_ref().map(|_| name.to_owned()),
            fast: fm.fast.map(|_| name.to_owned()),
            code_mode: fm.code_mode.map(|_| name.to_owned()),
            auto_compact: fm.auto_compact.as_ref().map(|_| name.to_owned()),
        }
    }

    fn inherit(&mut self, parent: &Self) {
        for (dst, src) in [
            (&mut self.effort, &parent.effort),
            (&mut self.fast, &parent.fast),
            (&mut self.code_mode, &parent.code_mode),
            (&mut self.auto_compact, &parent.auto_compact),
        ] {
            if dst.is_none() {
                dst.clone_from(src);
            }
        }
    }

    pub(super) fn overlay_from(&mut self, fm: &AgentFrontmatter, name: &str) {
        let mut own = Self::from_frontmatter(fm, name);
        own.inherit(self);
        *self = own;
    }
}

/// Retained definition state for rebuilding a stored profile row under an override,
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
            && a.writers == b.writers
            && a.crafts == b.crafts
            && a.system_prompt_file == b.system_prompt_file
            && a.args == b.args
            && a.original_kind == b.original_kind
            && a.natural_kind == b.natural_kind
            && a.natural_variant == b.natural_variant
            && a.preference == b.preference
            && a.runtime_kind == b.runtime_kind
            && a.built_under == b.built_under
            && a.seat == b.seat
            && a.is_project == b.is_project
            && a.context == b.context
    }
}

impl Eq for DefinitionSource {}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum MemoBase {
    Runtime(String, Option<&'static str>),
    Definition(String, PathBuf),
}

impl MemoBase {
    fn from_build_base(base: BuildBase<'_>) -> Option<Self> {
        match base {
            BuildBase::Runtime(rt) => Some(Self::Runtime(
                rt.kind().to_owned(),
                rt.variant.map(|v| v.name),
            )),
            BuildBase::Profile(profile) => profile
                .definition
                .as_ref()
                .map(|def| Self::Definition(def.inner.name.clone(), def.inner.path.clone())),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct MemoKey {
    base: Option<MemoBase>,
    target_kind: Option<String>,
    model: Option<String>,
}

impl MemoKey {
    fn new(base: Option<BuildBase<'_>>, target_kind: Option<&str>, model: Option<&str>) -> Self {
        Self {
            base: base.and_then(MemoBase::from_build_base),
            target_kind: target_kind.map(str::to_owned),
            model: model.map(str::to_owned),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct KindArgs {
    kind: String,
    args: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PromptLayer {
    owner: Option<String>,
    source: PromptSource,
}

/// Splits subagent validation when a definition inherits from an override (`+` list): names in
/// `own_names` check against `own`, while inherited names check against `inherited`.
struct SubagentValidator<'a> {
    own: Option<(&'a SubagentCheck, bool)>,
    inherited: Option<(&'a SubagentCheck, bool)>,
    own_names: Option<BTreeSet<String>>,
}

fn args_for_kind(layers: &[KindArgs], kind: &str) -> Option<String> {
    layers
        .iter()
        .find(|entry| entry.kind == kind)
        .map(|entry| entry.args.clone())
}

#[derive(Debug)]
struct DefinitionSourceInner {
    name: String,
    path: PathBuf,
    namespace: &'static str,
    chain: Vec<String>,
    frontmatter: AgentFrontmatter,
    writers: TuningWriters,
    crafts: Vec<PromptLayer>,
    system_prompt_file: Option<PromptSource>,
    args: Vec<KindArgs>,
    /// The kind this definition resolved to at load, preserved across on-demand rebuilds.
    original_kind: String,
    natural_kind: String,
    natural_variant: Option<&'static agents::Variant>,
    preference: TierPreference,
    runtime_kind: Option<String>,
    built_under: Option<MemoBase>,
    seat: Option<SeatInfo>,
    is_project: bool,
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

impl CraftState<'_> {
    fn rendered_source(&self, origin: &Path) -> Option<PromptSource> {
        let Self::Pending {
            cached: Some(Ok(text)),
            ..
        } = self
        else {
            return None;
        };
        (!text.is_empty()).then(|| PromptSource::Text {
            origin: origin.to_path_buf(),
            text: text.clone(),
        })
    }
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
pub(crate) fn keeps_inherited_model(from_kind: &str, to_kind: &str, model: &str) -> bool {
    from_kind == to_kind
        || agents::definition_model_kind(model) == Some(to_kind)
        || agents::keeps_model(to_kind, model)
}

fn merge_crafts(
    x_crafts: &[PromptLayer],
    leaf_crafts: &[PromptLayer],
    leaf_chain: &[String],
) -> Vec<PromptLayer> {
    let mut merged: Vec<PromptLayer> = x_crafts
        .iter()
        .filter(|layer| {
            layer
                .owner
                .as_deref()
                .is_none_or(|owner| !leaf_chain.iter().any(|entry| entry == owner))
        })
        .cloned()
        .collect();
    for layer in leaf_crafts {
        if matches!(layer.source, PromptSource::Text { .. })
            && merged
                .iter()
                .any(|existing| existing.source.origin() == layer.source.origin())
        {
            continue;
        }
        merged.push(layer.clone());
    }
    merged
}

impl DefinitionSource {
    pub(crate) fn chain(&self) -> &[String] {
        &self.inner.chain
    }

    pub(crate) fn original_kind(&self) -> &str {
        &self.inner.original_kind
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

    pub(crate) fn writers(&self) -> &TuningWriters {
        &self.inner.writers
    }

    /// The verdict `finish_profile` gives this definition's `subagents:` list, over the kind base
    /// `row` runs on, when `subagents` stands in for the names its load checked against.
    pub(super) fn check_subagents(
        &self,
        row: &Profile,
        subagents: &SubagentCheck,
    ) -> Result<(), DefinitionErr> {
        let inner = &self.inner;
        let mut fm = inner.frontmatter.clone();
        if let Some(base) = inner.context.shared.bases.get(&row.agent) {
            fm.inherit_base(base, inner.namespace, lists_agent_tool(&fm));
        }
        let Some(listed) = fm.subagents.as_deref() else {
            return Ok(());
        };
        let validator = SubagentValidator {
            own: Some((subagents, !inner.is_project)),
            inherited: None,
            own_names: None,
        };
        subagent_list(&inner.path, listed, false, &validator).map(drop)
    }

    /// This definition with its on-demand builds checking against `context`, the agents context
    /// of the trusted project it launches in, and a memo of its own.
    pub(super) fn in_project(&self, context: &Arc<LoadContext>) -> Self {
        let inner = &self.inner;
        Self {
            inner: Arc::new(DefinitionSourceInner {
                name: inner.name.clone(),
                path: inner.path.clone(),
                namespace: inner.namespace,
                chain: inner.chain.clone(),
                frontmatter: inner.frontmatter.clone(),
                writers: inner.writers.clone(),
                crafts: inner.crafts.clone(),
                system_prompt_file: inner.system_prompt_file.clone(),
                args: inner.args.clone(),
                original_kind: inner.original_kind.clone(),
                natural_kind: inner.natural_kind.clone(),
                natural_variant: inner.natural_variant,
                preference: inner.preference.clone(),
                runtime_kind: inner.runtime_kind.clone(),
                built_under: inner.built_under.clone(),
                seat: inner.seat.clone(),
                is_project: inner.is_project,
                context: Arc::clone(context),
                memo: Mutex::default(),
            }),
        }
    }

    /// The built-in kind base of `kind` as a row of this definition's namespace, where no home
    /// or pack base stands for that kind.
    pub(crate) fn built_in_base(&self, kind: &str) -> Option<&Profile> {
        let rows = self.inner.context.shared.built_in.get(kind)?;
        Some(if self.inner.namespace == "subagents" {
            &rows.1
        } else {
            &rows.0
        })
    }

    /// Whether this row was already built under `base`.
    pub(crate) fn is_built_under(&self, base: BuildBase<'_>) -> bool {
        self.inner.built_under.is_some()
            && self.inner.built_under == MemoBase::from_build_base(base)
    }

    pub(crate) fn build(
        &self,
        current: &Profile,
        base: Option<BuildBase<'_>>,
        target_kind: Option<&str>,
        model: Option<&str>,
    ) -> Result<Profile, DefinitionErr> {
        if model.is_none()
            && target_kind.is_none_or(|kind| kind == current.agent)
            && base.is_some_and(|base| self.is_built_under(base))
        {
            return Ok(current.clone());
        }
        let skip_memo = matches!(
            base,
            Some(BuildBase::Profile(profile))
                if profile
                    .definition
                    .as_ref()
                    .is_some_and(|def| def.inner.is_project)
        );
        if skip_memo {
            return self.inner.build_with_craft(
                base,
                target_kind,
                model,
                &mut CraftState::Rendered,
                true,
            );
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

pub(in crate::config) struct TierCandidates {
    pub(in crate::config) engine: bool,
    pub(in crate::config) renders: BTreeMap<String, Profile>,
    pub(in crate::config) exclusions: BTreeMap<String, DefinitionErr>,
}

/// Build the per-kind candidate rows and exclusions for one tier under `preference`: a single
/// engine build for providers that run other providers' models, or one build per distinct kind
/// named in the tier table.
pub(in crate::config) fn build_tier_candidates(
    tiers: &crate::config::tiers::TierConfig,
    tier: ModelTier,
    preference: &TierPreference,
    mut build: impl FnMut(&str, Option<&str>) -> Result<Profile, DefinitionErr>,
) -> TierCandidates {
    let engine = preference
        .family
        .as_deref()
        .filter(|kind| agents::runs_other_providers_models(kind));
    let candidates: Vec<(&str, Option<&str>)> = if let Some(engine) = engine {
        vec![(engine, None)]
    } else {
        tiers
            .entries(tier)
            .filter_map(|(_, kind, model)| kind.map(|kind| (kind, Some(model))))
            .collect()
    };
    let mut renders = BTreeMap::new();
    let mut exclusions = BTreeMap::new();
    for (kind, candidate_model) in candidates {
        if renders.contains_key(kind) || exclusions.contains_key(kind) {
            continue;
        }
        match build(kind, candidate_model) {
            Ok(rendered) => {
                renders.insert(kind.to_owned(), rendered);
            }
            Err(error) => {
                exclusions.insert(kind.to_owned(), error);
            }
        }
    }
    TierCandidates {
        engine: engine.is_some(),
        renders,
        exclusions,
    }
}

fn walk_tier_renders(
    path: &Path,
    tiers: &crate::config::tiers::TierConfig,
    tier: ModelTier,
    preference: &TierPreference,
    effort: Option<&str>,
    renders: &BTreeMap<String, Profile>,
    exclusions: &BTreeMap<String, DefinitionErr>,
) -> Result<Profile, DefinitionErr> {
    // Any one model clears the tier, so the first failed dependency met is the one to fix.
    let blocked = OnceCell::new();
    let pick = tiers
        .walk(
            tier,
            preference,
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
            DefinitionErr::new(path, error.to_string())
                .with_cause(blocked.get().cloned().unwrap_or(DefinitionCause::Invalid))
        })?;
    let mut selected = renders
        .get(&pick.kind)
        .cloned()
        .ok_or_else(|| DefinitionErr::new(path, format!("no eligible build for {}", pick.kind)))?;
    selected.model = Some(pick.model);
    selected.effort = effort.map(str::to_owned);
    selected.model_tier = Some(TierProvenance {
        tier,
        fell_back: pick.used_tier.is_some(),
    });
    Ok(selected)
}

impl DefinitionSourceInner {
    #[expect(
        clippy::too_many_arguments,
        reason = "constructs the retained source for one rebuilt profile row"
    )]
    fn built_source(
        &self,
        x_def: Option<&Self>,
        built_under: Option<MemoBase>,
        frontmatter: AgentFrontmatter,
        crafts: Vec<PromptLayer>,
        system_prompt_file: Option<PromptSource>,
        args: Vec<KindArgs>,
        built: &Profile,
        preference: TierPreference,
        runtime_kind: Option<String>,
        context: Arc<LoadContext>,
    ) -> DefinitionSource {
        let mut chain = self.chain.clone();
        let mut writers = self.writers.clone();
        if let Some(x) = x_def {
            for entry in &x.chain {
                if !chain.contains(entry) {
                    chain.push(entry.clone());
                }
            }
            writers.inherit(&x.writers);
        }
        DefinitionSource {
            inner: Arc::new(Self {
                name: self.name.clone(),
                path: self.path.clone(),
                namespace: self.namespace,
                chain,
                frontmatter,
                writers,
                crafts,
                system_prompt_file,
                args,
                original_kind: self.original_kind.clone(),
                natural_kind: built.agent.clone(),
                natural_variant: built.variant,
                preference,
                runtime_kind,
                built_under: built_under.or_else(|| self.built_under.clone()),
                seat: self.seat.clone(),
                is_project: self.is_project || x_def.is_some_and(|x| x.is_project),
                context,
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
        let mut built_under = base.and_then(MemoBase::from_build_base);
        let (has_base, x_def, base_kind, base_variant, base_writes_engine) = match base {
            Some(BuildBase::Runtime(rt)) => (true, None, Some(rt.kind()), rt.variant, false),
            Some(BuildBase::Profile(profile)) => match &profile.definition {
                Some(other) => (
                    true,
                    Some(other.inner.as_ref()),
                    (!other.inner.natural_kind.is_empty())
                        .then_some(other.inner.natural_kind.as_str()),
                    other.inner.natural_variant,
                    other.writes_engine(),
                ),
                _ => {
                    built_under = None;
                    (false, None, None, None, false)
                }
            },
            None => (false, None, None, None, false),
        };

        let mut fm = self.frontmatter.clone();
        if let Some(explicit_model) = model {
            fm.model = Some(explicit_model.to_owned());
            fm.tier = None;
        } else if base_writes_engine {
            fm.model = None;
            fm.tier = None;
        }
        let mut subagent_validator = SubagentValidator {
            own: self
                .context
                .subagents
                .as_deref()
                .map(|check| (check, !self.is_project)),
            inherited: None,
            own_names: None,
        };
        let mut retained_context = Arc::clone(&self.context);
        let x_in_chain = x_def.is_some_and(|x| self.chain.iter().any(|entry| entry == &x.name));
        if let Some(x) = x_def {
            if x_in_chain {
                // Non-engine fields and crafts of a shared ancestor are already in `self`;
                // apply only its engine so `+` lists and crafts enter once.
                if fm.model.is_none() && fm.tier.is_none() {
                    fm.model.clone_from(&x.frontmatter.model);
                    fm.tier.clone_from(&x.frontmatter.tier);
                }
            } else {
                let mut x_fm = x.frontmatter.clone().for_namespace(self.namespace);
                if lists_agent_tool(&fm) {
                    x_fm.subagents = None;
                }
                if x_fm.subagents.is_some() {
                    if self.frontmatter.subagents.is_none() {
                        subagent_validator.own = x
                            .context
                            .subagents
                            .as_deref()
                            .map(|check| (check, !x.is_project));
                        if x.is_project {
                            retained_context = Arc::clone(&x.context);
                        }
                    } else if let Some(own_list) = self.frontmatter.subagents.as_deref()
                        && own_list.iter().any(|entry| frontmatter::is_plus(entry))
                    {
                        subagent_validator.inherited = x
                            .context
                            .subagents
                            .as_deref()
                            .map(|check| (check, !x.is_project));
                        subagent_validator.own_names = Some(
                            own_list
                                .iter()
                                .filter(|entry| !frontmatter::is_plus(entry))
                                .map(|entry| entry.trim().to_owned())
                                .collect(),
                        );
                        if x.is_project {
                            retained_context = Arc::clone(&x.context);
                        }
                    }
                }
                fm.inherit(&x_fm);
            }
        }
        let crafts = match x_def.filter(|_| !x_in_chain) {
            Some(x) => merge_crafts(&x.crafts, &self.crafts, &self.chain),
            None => self.crafts.clone(),
        };
        let craft_sources: Vec<PromptSource> =
            crafts.iter().map(|layer| layer.source.clone()).collect();
        let system_prompt_file = match x_def.filter(|_| !x_in_chain) {
            Some(x) => self
                .system_prompt_file
                .clone()
                .or_else(|| x.system_prompt_file.clone()),
            None => self.system_prompt_file.clone(),
        };
        let mut merged_args = self.args.clone();
        if let Some(x) = x_def.filter(|_| !x_in_chain) {
            merged_args.extend(x.args.iter().cloned());
        }

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

        let (mut built, pre_base_fm) = if target_kind.is_none()
            && model.is_none()
            && let Some(tier_str) = fm.tier.as_deref()
        {
            let tier: ModelTier =
                tier_str
                    .parse()
                    .map_err(|error: crate::config::tiers::TierError| {
                        DefinitionErr::new(&self.path, error.to_string())
                    })?;
            let candidates = build_tier_candidates(
                &self.context.shared.tiers,
                tier,
                &effective_preference,
                |kind, candidate_model| {
                    let mut candidate_fm = fm.clone();
                    candidate_fm.model = candidate_model.map(str::to_owned);
                    let variant = effective_variant.filter(|_| kind == effective_kind);
                    finish_profile(
                        &self.context,
                        &self.path,
                        &self.name,
                        self.namespace,
                        self.seat.as_ref(),
                        kind,
                        variant,
                        false,
                        self.is_project,
                        &subagent_validator,
                        candidate_fm,
                        craft_sources.clone(),
                        system_prompt_file.clone(),
                        args_for_kind(&merged_args, kind),
                        craft,
                    )
                },
            );
            let built = walk_tier_renders(
                &self.path,
                &self.context.shared.tiers,
                tier,
                &effective_preference,
                fm.effort.as_deref(),
                &candidates.renders,
                &candidates.exclusions,
            )?;
            (built, fm)
        } else {
            let (kind, variant) = if let Some(raw_target) = target_kind {
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
                (kind, variant)
            } else if let Some(explicit_model) = model {
                if has_base {
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
                }
            } else {
                let kind = effective_kind.as_str();
                if !base_writes_engine
                    && let Some(inherited) = fm.model.as_deref()
                    && !keeps_inherited_model(&self.natural_kind, kind, inherited)
                {
                    fm.model = None;
                }
                (kind, effective_variant)
            };
            let pre_base_fm = fm.clone();
            let built = finish_profile(
                &self.context,
                &self.path,
                &self.name,
                self.namespace,
                self.seat.as_ref(),
                kind,
                variant,
                false,
                self.is_project,
                &subagent_validator,
                fm,
                craft_sources,
                system_prompt_file.clone(),
                args_for_kind(&merged_args, kind),
                craft,
            )?;
            (built, pre_base_fm)
        };
        if attach_definition {
            built.definition = Some(self.built_source(
                x_def,
                built_under,
                pre_base_fm,
                crafts,
                system_prompt_file,
                merged_args,
                &built,
                effective_preference,
                effective_runtime_kind,
                retained_context,
            ));
        }
        Ok(built)
    }
}

#[derive(Clone)]
struct Resolved {
    frontmatter: AgentFrontmatter,
    writers: TuningWriters,
    chain: Vec<String>,
    profile: Profile,
    preference: TierPreference,
    runtime_kind: Option<String>,
    crafts: Vec<PromptLayer>,
    system_prompt_file: Option<PromptSource>,
    args: Vec<KindArgs>,
}

pub(super) fn empty_profile(kind: &str) -> Profile {
    Profile {
        agent: kind.to_owned(),
        variant: None,
        definition_renders: None,
        definition: None,
        kind_defaults: false,
        kind_base: false,
        built_in_base: false,
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
    for error in resolver.errors {
        if !loaded
            .errors
            .iter()
            .any(|existing| existing.path == error.path)
        {
            loaded.errors.push(error);
        }
    }
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

/// Resolves every project row of one namespace, returning loaded rows, per-name errors, and
/// any per-name errors discovered in the foreign namespace (e.g. an agent parent resolved on
/// behalf of a subagent child) without deduplicating across rows that share `.rimz/config.toml`.
pub(super) fn resolve_project_namespace(
    mut resolver: Resolver<'_>,
) -> (
    BTreeMap<String, Profile>,
    BTreeMap<String, DefinitionErr>,
    BTreeMap<String, DefinitionErr>,
) {
    let names: Vec<String> = resolver.tree.definitions.keys().cloned().collect();
    for name in names {
        resolver.resolve(&name);
    }
    let profiles = resolver
        .resolved
        .into_iter()
        .filter_map(|(name, resolved)| Some((name, resolved?.profile)))
        .collect();
    let foreign_errors = resolver
        .foreign_resolver
        .map(|foreign| foreign.row_errors)
        .unwrap_or_default();
    (profiles, resolver.row_errors, foreign_errors)
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
            is_project: false,
            foreign_resolver: None,
            seat: None,
            seat_ancestors: Vec::new(),
            seat_crafts: Vec::new(),
            seat_writers: TuningWriters::default(),
            resolved: BTreeMap::new(),
            trail: Vec::new(),
            errors: Vec::new(),
            row_errors: BTreeMap::new(),
        }
    }

    pub(super) fn with_project(mut self) -> Self {
        self.is_project = true;
        self
    }

    pub(super) fn with_seat(
        mut self,
        seat: SeatInfo,
        ancestors: Vec<String>,
        crafts: Vec<PromptSource>,
        writers: TuningWriters,
    ) -> Self {
        self.seat = Some(seat);
        self.seat_ancestors = ancestors;
        self.seat_crafts = crafts;
        self.seat_writers = writers;
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
    /// Whether this tree holds project TOML rows sharing `.rimz/config.toml`.
    is_project: bool,
    foreign_resolver: Option<Box<Resolver<'a>>>,
    seat: Option<SeatInfo>,
    seat_ancestors: Vec<String>,
    seat_crafts: Vec<PromptSource>,
    seat_writers: TuningWriters,
    resolved: BTreeMap<String, Option<Resolved>>,
    trail: Vec<String>,
    errors: Vec<DefinitionErr>,
    row_errors: BTreeMap<String, DefinitionErr>,
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
            let error = DefinitionErr::new(
                &self.tree.definitions[name].path,
                format!("follows itself through `agent:` ({})", trail.join(" -> ")),
            );
            if self.is_project {
                self.row_errors
                    .entry(name.to_owned())
                    .or_insert_with(|| error.clone());
            }
            self.errors.push(error);
            return None;
        }
        self.trail.push(name.to_owned());
        let result = self.materialize(name);
        self.trail.pop();
        let resolved = match result {
            Ok(resolved) => Some(resolved),
            Err(error) => {
                if self.is_project {
                    self.row_errors.entry(name.to_owned()).or_insert(error);
                } else if !self
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
        let is_project = self.is_project;
        self.foreign_resolver
            .get_or_insert_with(|| {
                let mut resolver = Resolver::new(scope, "agents", [tree, foreign]);
                resolver.skip_subagents = true;
                resolver.is_project = is_project;
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
        if let Some(raw) = definition.extras.args.as_deref()
            && shlex::split(raw).is_none()
        {
            return Err(DefinitionErr::new(path, "check shell quoting in `args`"));
        }
        let mut system_prompt_file = definition.extras.system_prompt_file.clone();
        let mut parent_args = Vec::new();
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
        let mut writers = if self.seat.is_some() {
            self.seat_writers.clone()
        } else {
            TuningWriters::from_frontmatter(&definition.frontmatter, name)
        };
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
        let mut crafts: Vec<PromptLayer>;
        let mut profile = if parent_name.is_empty() {
            let profile = empty_profile("");
            crafts = self
                .seat_crafts
                .iter()
                .cloned()
                .map(|source| PromptLayer {
                    owner: None,
                    source,
                })
                .collect();
            profile
        } else if let Some(runtime) = agents::find_runtime(&parent_name) {
            let mut profile = empty_profile(runtime.kind());
            profile.variant = runtime.variant;
            crafts = self
                .seat_crafts
                .iter()
                .cloned()
                .map(|source| PromptLayer {
                    owner: None,
                    source,
                })
                .collect();
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
                        subagent: false,
                    })
                })?;
                if in_foreign {
                    let parent_fm = parent.frontmatter.clone().for_namespace(self.namespace);
                    fm.inherit(&parent_fm);
                } else {
                    fm.inherit(&parent.frontmatter);
                }
                writers.inherit(&parent.writers);
                for ancestor in &parent.chain {
                    if !chain.contains(ancestor) {
                        chain.push(ancestor.clone());
                    }
                }
                if self.is_project && chain.len() > MAX_PROFILE_DEPTH {
                    return Err(DefinitionErr::new(
                        path,
                        format!(
                            "profile `{name}` inheritance chain is deeper than {MAX_PROFILE_DEPTH}"
                        ),
                    ));
                }
                if runtime_kind.is_none() {
                    runtime_kind = parent.runtime_kind;
                }
                if !own_runtime {
                    preference = parent.preference;
                }
                let mut profile = empty_profile(&parent.profile.agent);
                profile.variant = parent.profile.variant;
                crafts = parent.crafts;
                if system_prompt_file.is_none() {
                    system_prompt_file = parent.system_prompt_file;
                }
                parent_args = parent.args;
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
        for extra_prompt in &definition.extras.append_system_prompt_files {
            crafts.push(PromptLayer {
                owner: Some(name.to_owned()),
                source: extra_prompt.clone(),
            });
        }
        let mut args = Vec::new();
        if let Some(raw) = &definition.extras.args {
            args.push(KindArgs {
                kind: profile.agent.clone(),
                args: raw.clone(),
            });
        }
        args.extend(parent_args);
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
            writers: writers.clone(),
            crafts,
            system_prompt_file: system_prompt_file.clone(),
            args: args.clone(),
            original_kind: profile.agent.clone(),
            natural_kind: profile.agent.clone(),
            natural_variant: profile.variant,
            preference: preference.clone(),
            runtime_kind: runtime_kind.clone(),
            built_under: None,
            seat: self.seat.clone(),
            is_project: self.is_project,
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
            // The probe covers native families and model-client engines; the walk skips exclusions.
            walk_tier_renders(
                path,
                &self.scope.context.shared.tiers,
                tier,
                &preference,
                fm.effort.as_deref(),
                &renders,
                &exclusions,
            )?
        } else {
            source
                .build_with_craft(None, None, None, &mut craft_state, false)
                .map_err(|mut error| {
                    if !self.is_project
                        && let Some(tier) = fm.model.as_deref().and_then(|model| {
                            agents::definition_model_kind(model).and_then(|kind| {
                                self.scope.context.shared.tiers.tier_for_model(kind, model)
                            })
                        })
                    {
                        error.message.push_str(&format!(
                            "; use `tier: {tier}` to allow another model or family"
                        ));
                    }
                    error
                })?
        };
        if let Some(craft_text) = craft_state.rendered_source(path) {
            source.crafts.push(PromptLayer {
                owner: None,
                source: craft_text,
            });
        }
        let resolved_crafts = source.crafts.clone();
        source.original_kind = profile.agent.clone();
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
            writers,
            chain,
            profile,
            preference,
            runtime_kind,
            crafts: resolved_crafts,
            system_prompt_file,
            args,
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
    pub(super) fn kind_base(&self, name: &str, built_in: bool) -> Result<Profile, DefinitionErr> {
        let definition = &self.tree.definitions[name];
        let fm = definition.frontmatter.clone().for_namespace(self.namespace);
        let mut craft_state = CraftState::Pending {
            traits: self.scope.traits,
            body: &definition.body,
            trait_names: definition.frontmatter.traits.as_deref().unwrap_or_default(),
            cached: None,
        };
        let subagent_validator = SubagentValidator {
            own: self
                .scope
                .context
                .subagents
                .as_deref()
                .map(|check| (check, true)),
            inherited: None,
            own_names: None,
        };
        let mut profile = finish_profile(
            self.scope.context,
            &definition.path,
            name,
            self.namespace,
            None,
            name,
            None,
            true,
            false,
            &subagent_validator,
            fm,
            Vec::new(),
            None,
            None,
            &mut craft_state,
        )?;
        profile.system_prompt_file = profile.append_system_prompt_files.pop();
        profile.built_in_base = built_in;
        if profile.system_prompt_file.is_none() {
            if !built_in {
                return Err(DefinitionErr::new(
                    &definition.path,
                    super::empty_kind_base(name),
                ));
            }
            // An appending kind's built-in base has no body, yet stands as the base its
            // definitions' crafts and team text compose onto.
            profile.system_prompt_file = Some(PromptSource::Text {
                origin: definition.path.clone(),
                text: String::new(),
            });
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
    name: &str,
    namespace: &str,
    seat: Option<&SeatInfo>,
    kind: &str,
    variant: Option<&'static agents::Variant>,
    kind_base: bool,
    is_project: bool,
    subagent_validator: &SubagentValidator<'_>,
    mut fm: AgentFrontmatter,
    crafts: Vec<PromptSource>,
    system_prompt_file: Option<PromptSource>,
    args: Option<String>,
    craft_state: &mut CraftState<'_>,
) -> Result<Profile, DefinitionErr> {
    let shared = &context.shared;
    let mut profile = empty_profile(kind);
    profile.variant = variant;
    profile.kind_base = kind_base;
    profile.system_prompt_file = system_prompt_file;
    profile.append_system_prompt_files = crafts;
    profile.args = args;
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
                subagent: false,
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
        // A project row takes its tools from the base alone.
        let message = if is_project {
            format!(
                "runs on {kind}, which requires a tools list; add `capabilities:` or `tools:` to \
                 the machine kind base {}",
                shared
                    .home
                    .join("agents")
                    .join(format!("{kind}.md"))
                    .display()
            )
        } else {
            format!("lists neither `capabilities` nor `tools`, which {kind} needs; write one")
        };
        return Err(DefinitionErr::new(path, message));
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
        profile.subagents = subagent_list(path, listed, profile.kind_base, subagent_validator)?;
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
    profile.auto_compact_default = shared.auto_compact_default;
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
                "profile '{name}' keep-warm while `{clause}`: a solo definition has no board; remove `while` and use `max` only",
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
    if !takes_system_prompt && profile.system_prompt_file.is_some() {
        return Err(DefinitionErr::new(
            path,
            format!(
                "runs on {kind}, which takes no system prompt, so `system-prompt-file` cannot be \
                 delivered; remove it or run on a kind that takes a system prompt"
            ),
        ));
    }
    if !takes_system_prompt && !profile.append_system_prompt_files.is_empty() {
        let message = if profile
            .append_system_prompt_files
            .iter()
            .any(|source| matches!(source, PromptSource::File(_)))
        {
            format!(
                "runs on {kind}, which takes no system prompt, so `append-system-prompt-files` \
                 cannot be delivered; remove it or run on a kind that takes a system prompt"
            )
        } else {
            format!(
                "runs on {kind}, which takes no system prompt, so its craft body cannot be \
                 delivered; remove the body or run this definition on a kind that takes a \
                 system prompt"
            )
        };
        return Err(DefinitionErr::new(path, message));
    }
    Ok(profile)
}

/// The `subagents:` field `finish_profile` lowers `listed` to under `validator`. A `+` list nothing
/// below added to leaves the field absent, which admits every child, and fails only on a name
/// that failed to load.
fn subagent_list(
    path: &Path,
    listed: &[String],
    kind_base: bool,
    validator: &SubagentValidator<'_>,
) -> Result<Option<Vec<String>>, DefinitionErr> {
    if listed.iter().any(|entry| frontmatter::is_plus(entry)) {
        if let Some((subagents, _)) = validator.own {
            let added: Vec<String> = listed
                .iter()
                .map(|entry| frontmatter::added(entry).to_owned())
                .collect();
            refuse_failed(path, &added, subagents)?;
        }
        return Ok(None);
    }
    // A kind base's names check once the subagents tree has resolved.
    if kind_base {
        return names(path, "subagents", listed).map(Some);
    }
    if let (Some(own_names), Some((inh_check, inh_implicit))) =
        (&validator.own_names, validator.inherited)
    {
        let (own_listed, inh_listed): (Vec<_>, Vec<_>) = listed
            .iter()
            .cloned()
            .partition(|entry| own_names.contains(entry.trim()));
        if !own_listed.is_empty() {
            if let Some((own_check, own_implicit)) = validator.own {
                subagent_names(path, &own_listed, own_check, own_implicit)?;
            } else {
                names(path, "subagents", &own_listed)?;
            }
        }
        if !inh_listed.is_empty() {
            subagent_names(path, &inh_listed, inh_check, inh_implicit)?;
        }
        return names(path, "subagents", listed).map(Some);
    }
    match validator.own {
        Some((subagents, allow_implicit_general)) => {
            subagent_names(path, listed, subagents, allow_implicit_general).map(Some)
        }
        None => names(path, "subagents", listed).map(Some),
    }
}

/// A kind base's `subagents:` field once the subagents tree has resolved, from its written
/// `listed`: a plain list's checked names, or a `+` list's absent field.
pub(super) fn base_subagents(
    path: &Path,
    listed: &[String],
    subagents: &SubagentCheck,
) -> Result<Option<Vec<String>>, DefinitionErr> {
    let validator = SubagentValidator {
        own: Some((subagents, true)),
        inherited: None,
        own_names: None,
    };
    subagent_list(path, listed, false, &validator)
}

/// Refuses the first of `names` that `subagents` records as failed to load, checking an entry
/// spelled as a kind alias no subagent name shadows as that kind, the row the runtime offers.
fn refuse_failed(
    path: &Path,
    names: &[String],
    subagents: &SubagentCheck,
) -> Result<(), DefinitionErr> {
    let known = |name: &str| {
        subagents.defined.contains(name)
            || subagents.failed.contains(name)
            || subagents.allowed.contains(name)
    };
    let Some((entry, failed)) = names.iter().find_map(|entry| {
        let row = Some(entry.as_str())
            .filter(|name| known(name))
            .or_else(|| {
                agents::find_runtime(entry)
                    .filter(|runtime| runtime.variant.is_none())
                    .map(agents::Runtime::kind)
            })
            .unwrap_or(entry);
        ((subagents.defined.contains(row) || subagents.failed.contains(row))
            && !subagents.allowed.contains(row))
        .then_some((entry, row))
    }) else {
        return Ok(());
    };
    Err(DefinitionErr::new(
        path,
        format!("allows subagent '{entry}', which failed to load"),
    )
    .with_cause(DefinitionCause::DependsOnFailed {
        name: failed.to_owned(),
        subagent: true,
    }))
}

/// A `subagents:` list's names, each a subagent in `subagents.allowed` (those that loaded, and
/// the `[agents.commands]` names), a kind, or `general`.
fn subagent_names(
    path: &Path,
    listed: &[String],
    subagents: &SubagentCheck,
    allow_implicit_general: bool,
) -> Result<Vec<String>, DefinitionErr> {
    let names = names(path, "subagents", listed)?;
    for name in &names {
        subagents.check_lookup(name, path)?;
    }
    refuse_failed(path, &names, subagents)?;
    let unknown: Vec<_> = names
        .iter()
        .filter(|name| {
            !(allow_implicit_general && name.as_str() == "general")
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
