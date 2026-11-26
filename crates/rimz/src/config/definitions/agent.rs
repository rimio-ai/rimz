//! Chain resolution and profile materialization for both definition namespaces.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::agents::{self, ManualSkill, PresetField, ToolSet};
use crate::config::engine::{self, Engine, keeps_inherited_model};
use crate::config::tiers::{ModelTier, TierPreference, TierProvenance};
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
            && a.seat == b.seat
            && a.is_project == b.is_project
            && a.context == b.context
    }
}

impl Eq for DefinitionSource {}

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
    own: Option<&'a SubagentCheck>,
    inherited: Option<&'a SubagentCheck>,
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
    seat: Option<SeatInfo>,
    is_project: bool,
    context: Arc<LoadContext>,
    /// The detail of each subagent that failed in the trusted project this definition launches
    /// in, which a build that depends on one names as its cause.
    failed_subagents: Arc<BTreeMap<String, String>>,
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

fn lists_agent_tool(fm: &AgentFrontmatter, kind: &str) -> bool {
    fm.tools
        .as_deref()
        .and_then(|tools| {
            let bare: Vec<String> = tools
                .iter()
                .map(|tool| tool.trim().trim_start_matches('+').to_owned())
                .collect();
            ToolSet::parse(&bare).ok()
        })
        .is_some_and(|tools| agents::grants_tool(kind, tools.bases(), "Agent"))
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
            fm.inherit_base(base, inner.namespace, lists_agent_tool(&fm, &row.agent));
        }
        let Some(listed) = fm.subagents.as_deref() else {
            return Ok(());
        };
        let validator = SubagentValidator {
            own: Some(subagents),
            inherited: None,
            own_names: None,
        };
        subagent_list(&inner.path, listed, false, &validator).map(drop)
    }

    /// This definition with its on-demand builds checking against `context`, the agents context
    /// of the trusted project it launches in, where `failed_subagents` holds the detail of each
    /// subagent that failed.
    pub(super) fn in_project(
        &self,
        context: &Arc<LoadContext>,
        failed_subagents: &Arc<BTreeMap<String, String>>,
    ) -> Self {
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
                seat: inner.seat.clone(),
                is_project: inner.is_project,
                context: Arc::clone(context),
                failed_subagents: Arc::clone(failed_subagents),
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

    /// The tier table this definition's walks read.
    pub(crate) fn tiers(&self) -> &crate::config::tiers::TierConfig {
        &self.inner.context.shared.tiers
    }

    /// The entries of its tier, and of the tiers above it, that this definition cannot run, each
    /// with the refusal of a build under that entry's kind: none for a definition with no tier.
    pub fn tier_exclusions(&self) -> Vec<engine::TierExclusion> {
        let Some(tier) = self.written_tier() else {
            return Vec::new();
        };
        let mut build = |kind: &str, _: Option<&'static agents::Variant>, model: Option<&str>| {
            self.build(None, kind, model)
        };
        engine::tier_exclusions(
            self.tiers(),
            tier,
            self.preference(),
            (&self.inner.natural_kind, self.inner.natural_variant),
            &mut engine::Probe {
                build: &mut build,
                unavailable: &mut |_, _| None,
            },
        )
    }

    /// Builds this definition again under X (`base`) on the kind the engine already chose, with a
    /// model. Nothing is cached: each call builds.
    pub(crate) fn build(
        &self,
        base: Option<BuildBase<'_>>,
        target_kind: &str,
        model: Option<&str>,
    ) -> Result<Profile, DefinitionErr> {
        self.inner
            .build_with_craft(base, target_kind, model, &mut CraftState::Rendered, true)
            .map_err(|error| self.inner.name_failed_subagent(error))
    }
}

/// The render a tier walk picked, carrying the tier's model and provenance and the chain's
/// effort.
fn tiered((engine, mut render): (Engine, Profile), effort: Option<&str>) -> Profile {
    render.model = engine.model.named().map(str::to_owned);
    render.effort = effort.map(str::to_owned);
    render.model_tier = engine.tier.map(|stamp| TierProvenance {
        tier: stamp.tier,
        fell_back: stamp.used_tier.is_some(),
    });
    render
}

impl DefinitionSourceInner {
    /// `error` with the detail of the failed project subagent it depends on, when it does.
    fn name_failed_subagent(&self, error: DefinitionErr) -> DefinitionErr {
        let DefinitionCause::DependsOnFailed {
            name,
            subagent: true,
        } = &error.cause
        else {
            return error;
        };
        let Some(detail) = self.failed_subagents.get(name) else {
            return error;
        };
        DefinitionErr {
            message: format!(
                "{}: {}\ncaused by: {detail}",
                error.path.display(),
                error.message
            ),
            ..error
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "constructs the retained source for one rebuilt profile row"
    )]
    fn built_source(
        &self,
        x_def: Option<&Self>,
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
                seat: self.seat.clone(),
                is_project: self.is_project || x_def.is_some_and(|x| x.is_project),
                context,
                failed_subagents: Arc::clone(&self.failed_subagents),
            }),
        }
    }

    fn build_with_craft(
        &self,
        base: Option<BuildBase<'_>>,
        target_kind: &str,
        model: Option<&str>,
        craft: &mut CraftState<'_>,
        attach_definition: bool,
    ) -> Result<Profile, DefinitionErr> {
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
                _ => (false, None, None, None, false),
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
            own: self.context.subagents.as_deref(),
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
                let x_fm = x.frontmatter.clone().for_namespace(self.namespace);
                if x_fm.subagents.is_some() {
                    if self.frontmatter.subagents.is_none() {
                        subagent_validator.own = x.context.subagents.as_deref();
                        if x.is_project {
                            retained_context = Arc::clone(&x.context);
                        }
                    } else if let Some(own_list) = self.frontmatter.subagents.as_deref()
                        && own_list.iter().any(|entry| frontmatter::is_plus(entry))
                    {
                        subagent_validator.inherited = x.context.subagents.as_deref();
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
        // A leaf that lists the Agent tool on the kind a build runs on keeps only its own
        // `subagents`, so each build drops the override's list once it knows its kind.
        let keeps_own_subagents = |fm: &mut AgentFrontmatter, kind: &str| {
            let yields =
                x_def.is_some() && !x_in_chain && lists_agent_tool(&self.frontmatter, kind);
            if yields {
                fm.subagents.clone_from(&self.frontmatter.subagents);
            }
            yields
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

        let target_rt = agents::find_runtime(target_kind);
        let kind = target_rt.map_or(target_kind, |rt| rt.kind());
        let variant = target_rt
            .and_then(|rt| rt.variant)
            .or_else(|| engine::variant(kind, &effective_kind, effective_variant));
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
        keeps_own_subagents(&mut fm, kind);
        let mut pre_base_fm = fm.clone();
        let mut built = finish_profile(
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
        if keeps_own_subagents(&mut pre_base_fm, &built.agent) {
            retained_context = Arc::clone(&self.context);
        }
        if attach_definition {
            built.definition = Some(self.built_source(
                x_def,
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
    leader: bool,
    /// A team running the shipped reflect consensus adds `rimz-reflect` to a non-empty skills
    /// list.
    reflects: bool,
}

/// The capability every Markdown team seat's built-in consensus names (`rimz(message)`).
const CONSENSUS_SKILL: &str = "rimz-message";

/// The capability whose tools the Board writes fragment after the consensus names.
const BOARD_CAPABILITY: &str = "rimz-blackboard";

#[derive(Clone, Copy, Default)]
struct SeatSkillEdits {
    added_consensus: bool,
    added_reflect: bool,
}

impl SeatEdits {
    /// Builds seat edits for `handle` on `team`, taking the leader from
    /// [`crate::config::Team::leader`] so seat edits share one leader definition with the
    /// served-capability and launch switch-off rules.
    pub(super) fn for_seat(team: &crate::config::Team, handle: &str, reflects: bool) -> Self {
        Self {
            leader: team.leader.as_deref() == Some(handle),
            reflects,
        }
    }

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
            // Every Markdown team runs the built-in consensus, which names `rimz(message)`,
            // and its Board writes fragment, which names the blackboard tools (a project TOML
            // team may replace the consensus with `consensus-file`, and its seats get no seat
            // edits). Beside `capabilities:`, RimZ capabilities go only in the grant, even
            // beside `skills: []` where adding one implies `Skill`.
            for granted in [CONSENSUS_SKILL, BOARD_CAPABILITY] {
                if !capabilities.iter().any(|entry| {
                    let name = frontmatter::marked_name(entry);
                    name == granted || name == SERVER_NAME
                }) {
                    capabilities.push(granted.to_owned());
                }
            }
        } else {
            if let Some(mcps) = &mut fm.mcps {
                for granted in [CONSENSUS_SKILL, BOARD_CAPABILITY] {
                    if !mcps.iter().any(|entry| {
                        let name = frontmatter::marked_name(entry);
                        name == granted || matches!(name, SERVER_NAME | "*")
                    }) {
                        mcps.push(granted.to_owned());
                    }
                }
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
        }
        let (kind, variant) = engine::file_kind(&engine::Lines {
            kind: &profile.agent,
            variant: profile.variant,
            model: fm.model.as_deref(),
            family: runtime_kind.as_deref(),
        })
        .map_err(|error| error.into_definition_err(path))?;
        let (written_kind, written_variant) = (profile.agent.clone(), profile.variant);
        (profile.agent, profile.variant) = (kind.to_owned(), variant);
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
            seat: self.seat.clone(),
            is_project: self.is_project,
            context: Arc::clone(self.scope.context),
            failed_subagents: Arc::default(),
        };
        let lines = engine::Lines {
            kind: &written_kind,
            variant: written_variant,
            model: fm.model.as_deref(),
            family: runtime_kind.as_deref(),
        };
        let mut build = |kind: &str, _: Option<&'static agents::Variant>, model: Option<&str>| {
            source.build_with_craft(None, kind, model, &mut craft_state, false)
        };
        // The probe covers native families and model-client engines; the walk skips refusals.
        let chosen = engine::file(
            &lines,
            tier,
            &preference,
            &self.scope.context.shared.tiers,
            &mut engine::Probe {
                build: &mut build,
                unavailable: &mut |_, _| None,
            },
        );
        let mut profile = match chosen {
            Ok(chosen) if tier.is_some() => tiered(chosen, fm.effort.as_deref()),
            Ok((_, profile)) => profile,
            Err(error) => {
                let mut error = error.into_definition_err(path);
                if tier.is_none()
                    && !self.is_project
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
                return Err(error);
            }
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
        profile.definition = Some(def_source);
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
            own: self.scope.context.subagents.as_deref(),
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
        fm.inherit_base(base, namespace, lists_agent_tool(&fm, kind));
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
    // A built-in kind base lists no tools, so a launch on it keeps the provider's own.
    let on_built_in_base = shared.built_in.contains_key(kind);
    if lowered.is_none()
        && fm.tools.is_none()
        && agents::tools_required(kind)
        && !profile.kind_base
        && !on_built_in_base
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
    if tools.is_some() || !(profile.kind_base || on_built_in_base) {
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
        if let Some(tools) = tools
            .as_ref()
            .filter(|tools| agents::grants_tool(kind, tools.bases(), "Agent"))
        {
            let message = match agents::tool_preset(kind, tools.bases()) {
                Some(preset) => format!(
                    "sets `subagents:` and lists the `{preset}` tool preset, which holds the Agent tool; the rimz doorway replaces the native one, so drop `subagents:`, or list the tools by name without Agent"
                ),
                _ => "sets `subagents:` and lists the Agent tool; the rimz doorway replaces the native one, so drop whichever this seat does not use".to_owned(),
            };
            return Err(DefinitionErr::new(path, message));
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
        if let Some(subagents) = validator.own {
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
    if let (Some(own_names), Some(inh_check)) = (&validator.own_names, validator.inherited) {
        let (own_listed, inh_listed): (Vec<_>, Vec<_>) = listed
            .iter()
            .cloned()
            .partition(|entry| own_names.contains(entry.trim()));
        if !own_listed.is_empty() {
            if let Some(own_check) = validator.own {
                subagent_names(path, &own_listed, own_check)?;
            } else {
                names(path, "subagents", &own_listed)?;
            }
        }
        if !inh_listed.is_empty() {
            subagent_names(path, &inh_listed, inh_check)?;
        }
        return names(path, "subagents", listed).map(Some);
    }
    match validator.own {
        Some(subagents) => subagent_names(path, listed, subagents).map(Some),
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
        own: Some(subagents),
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
/// the `[agents.commands]` names) or a kind.
fn subagent_names(
    path: &Path,
    listed: &[String],
    subagents: &SubagentCheck,
) -> Result<Vec<String>, DefinitionErr> {
    let names = names(path, "subagents", listed)?;
    for name in &names {
        subagents.check_lookup(name, path)?;
    }
    refuse_failed(path, &names, subagents)?;
    let unknown: Vec<_> = names
        .iter()
        .filter(|name| {
            agents::find_definition(name).is_none() && !subagents.allowed.contains(*name)
        })
        .collect();
    if !unknown.is_empty() {
        return Err(subagents.unknown_err(path, &unknown));
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

/// The capability `rimz-shell` and `rimz-watch` replaced.
const RETIRED_WAIT: &str = "rimz-wait";

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
        if name == RETIRED_WAIT {
            return Err(DefinitionErr::new(
                path,
                format!(
                    "lists `{entry}` in `capabilities:`, a capability RimZ split in two: list \
                     `rimz-shell` to run a command outside your turn, `rimz-watch` to wake on a \
                     condition, or both"
                ),
            ));
        }
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
    if let Some((doorway, _)) = listed
        .iter()
        .find(|(name, _)| [Capability::Ask.name(), SERVER_NAME].contains(name))
        .filter(|_| bases.iter().any(|base| base == "AskUserQuestion"))
    {
        return Err(DefinitionErr::new(
            path,
            format!(
                "lists `ask` and `{doorway}` in `capabilities:`; the rimz doorway replaces the \
                 native one, so drop whichever this seat does not use"
            ),
        ));
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
