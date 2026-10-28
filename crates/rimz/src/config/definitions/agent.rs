//! Chain resolution and profile materialization for both definition namespaces.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::agents::{self, ManualSkill, PresetField, ToolSet};
use crate::config::tiers::{DefinitionRenders, ModelTier, TierPreference, TierProvenance};
use crate::config::{Isolation, Profile, PromptSource, SkillName};

use super::frontmatter::AgentFrontmatter;
use super::{
    DefinitionCause, DefinitionErr, LoadScope, LoadedDefinitions, Namespace, SkillCatalog,
    SkillCheck, frontmatter, traits,
};

#[derive(Clone)]
struct Resolved {
    frontmatter: AgentFrontmatter,
    profile: Profile,
    preference: TierPreference,
    runtime_kind: Option<String>,
}

pub(super) fn empty_profile(kind: &str) -> Profile {
    Profile {
        agent: kind.to_owned(),
        definition_renders: None,
        kind_defaults: false,
        tools: None,
        model_tier: None,
        tier_stamp: None,
        isolation: None,
        empty_dir: None,
        skills: None,
        allowed_tools: None,
        description: None,
        subagents: None,
        model_reminder: None,
        keep_warm: None,
        idle_compact: None,
        mode: None,
        model: None,
        effort: None,
        fast: None,
        budget: None,
        auto_compact: None,
        auto_compact_default: None,
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        args: None,
    }
}

/// Resolves every definition of one tree into `loaded`.
pub(super) fn resolve_namespace(mut resolver: Resolver<'_>, loaded: &mut LoadedDefinitions) {
    for name in resolver.tree.definitions.keys() {
        if resolver.resolve(name).is_none() {
            loaded
                .failed
                .entry(name.clone())
                .or_default()
                .insert(resolver.tree.definitions[name].path.clone());
        }
    }
    loaded.errors.extend(resolver.errors);
    for (name, resolved) in resolver.resolved {
        if let Some(resolved) = resolved {
            loaded.insert(
                resolver.namespace,
                &name,
                &resolver.tree.definitions[&name].path,
                resolved.profile,
            );
        }
    }
}

impl<'a> Resolver<'a> {
    pub(super) fn new(
        scope: LoadScope<'a>,
        namespace: &'a str,
        [tree, foreign]: [&'a Namespace; 2],
        allowed_children: &'a BTreeSet<String>,
    ) -> Self {
        Self {
            scope,
            namespace,
            tree,
            foreign,
            allowed_children,
            resolved: BTreeMap::new(),
            trail: Vec::new(),
            errors: Vec::new(),
        }
    }
}

pub(super) struct Resolver<'a> {
    scope: LoadScope<'a>,
    namespace: &'a str,
    tree: &'a Namespace,
    foreign: &'a Namespace,
    allowed_children: &'a BTreeSet<String>,
    resolved: BTreeMap<String, Option<Resolved>>,
    trail: Vec<String>,
    errors: Vec<DefinitionErr>,
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

    fn materialize(&mut self, name: &str) -> Result<Resolved, DefinitionErr> {
        let definition = &self.tree.definitions[name];
        let path = &definition.path;
        let mut fm = definition.frontmatter.clone();
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
            .and_then(agents::find_definition)
            .map(|definition| definition.spec().kind);
        let model_kind = fm
            .model
            .as_deref()
            .and_then(agents::definition_model_kind)
            .filter(|_| !own_kind.is_some_and(agents::runs_other_providers_models));
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
        let mut profile = if parent_name.is_empty() {
            empty_profile("")
        } else if let Some(kind) = agents::find_definition(&parent_name) {
            empty_profile(kind.spec().kind)
        } else if self.tree.definitions.contains_key(&parent_name)
            || self.tree.failed.contains(&parent_name)
        {
            let parent = self.resolve(&parent_name).ok_or_else(|| {
                DefinitionErr::new(
                    path,
                    format!("follows `{parent_name}`, which failed to load"),
                )
                .with_cause(DefinitionCause::DependsOnFailed {
                    name: parent_name.clone(),
                })
            })?;
            fm.inherit(&parent.frontmatter);
            if runtime_kind.is_none() {
                runtime_kind = parent.runtime_kind;
            }
            if !own_runtime {
                preference = parent.preference;
            }
            let mut profile = empty_profile(&parent.profile.agent);
            profile.append_system_prompt_files = parent.profile.append_system_prompt_files;
            profile
        } else {
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
        } else if let Some(kind) = fm.model.as_deref().and_then(agents::definition_model_kind) {
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
        let mut renders = BTreeMap::new();
        let mut exclusions = BTreeMap::new();
        for (_, kind, model) in self.scope.tiers.entries(ModelTier::Intern) {
            if renders.contains_key(kind) || exclusions.contains_key(kind) {
                continue;
            }
            let mut candidate = profile.clone();
            candidate.agent = kind.to_owned();
            let mut candidate_fm = fm.clone();
            candidate_fm.model = Some(model.to_owned());
            match self.finish(name, candidate_fm, candidate) {
                Ok(rendered) => {
                    renders.insert(kind.to_owned(), rendered);
                }
                Err(error) => {
                    exclusions.insert(kind.to_owned(), error.message);
                }
            }
        }
        let mut profile = if let Some(tier) = tier {
            let pick = self
                .scope
                .tiers
                .walk(
                    tier,
                    &preference,
                    |kind| exclusions.get(kind).map(String::as_str),
                    |_, _| None::<()>,
                )
                .map_err(|error| {
                    DefinitionErr::new(
                        path,
                        format!(
                            "{error}; {}",
                            exclusions
                                .iter()
                                .map(|(kind, reason)| format!("{kind}: {reason}"))
                                .collect::<Vec<_>>()
                                .join("; ")
                        ),
                    )
                })?;
            // The probe loop rendered or excluded every table family, and the walk passes the excluded ones.
            let mut selected = renders[&pick.kind].clone();
            selected.model = Some(pick.model.clone());
            selected.effort = fm.effort.clone();
            selected.model_tier = Some(TierProvenance {
                tier,
                fell_back: pick.used_tier.is_some(),
            });
            selected
        } else {
            self.finish(name, fm.clone(), profile)
                .map_err(|mut error| {
                    if let Some(tier) = fm.model.as_deref().and_then(|model| {
                        agents::definition_model_kind(model)
                            .and_then(|kind| self.scope.tiers.tier_for_model(kind, model))
                    }) {
                        error.message.push_str(&format!(
                            "; use `tier: {tier}` to allow another model or family"
                        ));
                    }
                    error
                })?
        };
        profile.definition_renders = Some(DefinitionRenders {
            preference: preference.clone(),
            renders,
            exclusions,
            effort: fm.effort,
        });
        Ok(Resolved {
            frontmatter: unresolved,
            profile,
            preference,
            runtime_kind,
        })
    }

    /// The definition along `name`'s `agent:` chain whose own frontmatter sets `effort:`.
    fn effort_source<'n>(&'n self, mut name: &'n str) -> &'n str {
        loop {
            let own = &self.tree.definitions[name].frontmatter;
            match own.agent.as_deref() {
                Some(parent)
                    if own.effort.is_none() && self.tree.definitions.contains_key(parent) =>
                {
                    name = parent;
                }
                _ => return name,
            }
        }
    }

    fn finish(
        &self,
        name: &str,
        fm: AgentFrontmatter,
        mut profile: Profile,
    ) -> Result<Profile, DefinitionErr> {
        let definition = &self.tree.definitions[name];
        let path = &definition.path;
        let kind = profile.agent.as_str();
        let takes_system_prompt = agents::find_definition(kind)
            .is_some_and(|definition| definition.spec().launch.takes_system_prompt());
        let base = self.scope.home.join("agents").join(format!("{kind}.md"));
        if self.scope.failed_bases.contains(&base) {
            let message = if takes_system_prompt {
                format!(
                    "runs on {kind}, {}",
                    super::failed_kind_base(self.scope.home, kind)
                )
            } else {
                format!(
                    "runs on {kind}, which takes no system prompt; its kind base {} failed to \
                     load — delete that file",
                    base.display()
                )
            };
            return Err(DefinitionErr::new(path, message).with_cause(
                DefinitionCause::DependsOnFailed {
                    name: kind.to_owned(),
                },
            ));
        }
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
            && !agents::runs_other_providers_models(kind)
        {
            return Err(DefinitionErr::new(
                path,
                format!(
                    "runs on '{kind}' but model '{model}' runs on '{implied}'; set a {kind} model or follow the {implied} base"
                ),
            ));
        }
        if fm.tools.is_none() && agents::tools_required(kind) {
            return Err(DefinitionErr::new(
                path,
                format!("lists no `tools`, which {kind} needs"),
            ));
        }
        let tools = fm
            .tools
            .as_deref()
            .map(ToolSet::parse)
            .transpose()
            .map_err(|error| DefinitionErr::new(path, error.to_string()))?;
        agents::render_tool_args(kind, tools.as_ref())
            .map_err(|error| DefinitionErr::new(path, error.to_string()))?;
        if fm.idle_compact.is_some() && self.namespace == "subagents" {
            return Err(DefinitionErr::new(
                path,
                "sets `idle-compact:`, which a subagent never needs; remove it",
            ));
        }
        profile.idle_compact = fm.idle_compact;
        if let Some(listed) = fm.subagents.as_ref() {
            if self.namespace == "subagents" {
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
            let names = names(path, "subagents", listed)?;
            if let Some(failed) = names.iter().find(|name| {
                (self.foreign.definitions.contains_key(*name)
                    || self.foreign.failed.contains(*name))
                    && !self.allowed_children.contains(*name)
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
                        && !self.allowed_children.contains(*name)
                })
                .collect();
            if !unknown.is_empty() {
                return Err(DefinitionErr::new(
                    path,
                    format!("allows unknown subagent profile(s) {unknown:?}"),
                ));
            }
            profile.subagents = Some(names);
        }
        let skill_check = match self.scope.skills.check {
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
        profile.skills = skill_policy(
            path,
            kind,
            fm.skills.as_deref(),
            tools.as_ref(),
            skill_check,
            self.scope.skills,
        )?;
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
            super::super::agents::native_auto_compact(self.scope.native_auto_compact, kind);
        profile.fast = fm.fast;
        profile.budget = fm.budget.clone();
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
        let craft = traits::render(
            self.scope.home,
            path,
            &definition.body,
            fm.traits.as_deref().unwrap_or_default(),
            self.scope.skills,
        )?;
        if !craft.is_empty() {
            profile.append_system_prompt_files.push(PromptSource::Text {
                origin: path.clone(),
                text: craft,
            });
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
        if takes_system_prompt && !self.scope.bases.contains(kind) {
            return Err(DefinitionErr::new(
                path,
                format!(
                    "runs on {kind}, {}",
                    super::missing_kind_base(self.scope.home, kind)
                ),
            ));
        }
        Ok(profile)
    }
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

/// Under [`SkillCheck::Check`], each listed skill resolves as the scoped view finds it: the kind's provider skill root, then the library. The first root holding `SKILL.md` wins and carries the marker check; only an absent file falls through, so an unreadable copy fails rather than letting a shadowed library copy speak for it.
pub(super) fn skill_policy(
    path: &Path,
    kind: &str,
    listed: Option<&[String]>,
    tools: Option<&ToolSet>,
    check: SkillCheck<'_>,
    catalog: &SkillCatalog<'_>,
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
        if let SkillCheck::Check { env, library, .. } = check {
            let roots: Vec<PathBuf> = definition
                .and_then(|definition| definition.skills_home(env))
                .into_iter()
                .chain([library.to_path_buf()])
                .collect();
            let Some(found) = discovered
                .as_ref()
                .and_then(|skills| skills.get(skill.as_str()))
            else {
                let searched: Vec<String> = roots
                    .iter()
                    .map(|root| root.join(&name).join("SKILL.md").display().to_string())
                    .collect();
                return Err(DefinitionErr::new(
                    path,
                    format!(
                        "lists skill '{name}', missing at {}",
                        searched.join(" and ")
                    ),
                )
                .with_cause(DefinitionCause::MissingSkill { skill, roots }));
            };
            let skill_path = found.source.join("SKILL.md");
            let text = catalog.read_text(&skill_path).map_err(|error| {
                DefinitionErr::new(
                    path,
                    format!(
                        "lists skill '{name}', unreadable at {}: {error}",
                        skill_path.display()
                    ),
                )
            })?;
            let skill_path = skill_path.as_path();
            let directory = skill_path.parent().unwrap_or(skill_path);
            let marker = definition.map_or(ManualSkill::Unsupported, |definition| {
                definition.manual_skill()
            });
            let manual = match marker {
                ManualSkill::Unsupported => false,
                ManualSkill::Frontmatter => {
                    let (block, _) = frontmatter::split(skill_path, &text).map_err(|error| {
                        DefinitionErr::new(
                            path,
                            format!(
                                "lists skill '{name}', invalid at {}: {}",
                                error.path.display(),
                                error.message
                            ),
                        )
                    })?;
                    block.lines().any(|line| {
                        line.strip_prefix("disable-model-invocation:")
                            .is_some_and(|value| value.trim() == "true")
                    })
                }
                ManualSkill::OpenAiPolicy => {
                    let policy_path = directory.join("agents/openai.yaml");
                    match catalog.read_text(&policy_path) {
                        Ok(text) => {
                            #[derive(serde::Deserialize)]
                            struct Document {
                                policy: Option<Policy>,
                            }
                            #[derive(serde::Deserialize)]
                            struct Policy {
                                allow_implicit_invocation: Option<bool>,
                            }
                            let document: Option<Document> = serde_saphyr::from_str(&text)
                                .map_err(|error| {
                                    DefinitionErr::new(
                                        path,
                                        format!(
                                            "lists skill '{name}', invalid at {}: {error}",
                                            policy_path.display()
                                        ),
                                    )
                                })?;
                            document
                                .and_then(|document| document.policy)
                                .and_then(|policy| policy.allow_implicit_invocation)
                                == Some(false)
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                        Err(error) => {
                            return Err(DefinitionErr::new(
                                path,
                                format!(
                                    "lists skill '{name}', unreadable at {}: {error}",
                                    policy_path.display()
                                ),
                            ));
                        }
                    }
                }
            };
            if manual {
                let (marker_file, marker_line) = if matches!(marker, ManualSkill::OpenAiPolicy) {
                    (
                        directory.join("agents/openai.yaml"),
                        "policy.allow_implicit_invocation: false",
                    )
                } else {
                    (skill_path.to_owned(), "disable-model-invocation: true")
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
