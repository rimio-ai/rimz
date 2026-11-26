//! The one chooser of an engine: the provider kind, model, tier stamp and variant a definition
//! file, a launch, or a resume runs on.
//!
//! Three entries share the predicates here: [`file`] for a definition's own engine, [`launch`]
//! for a fresh launch under its flags and `--agent`, and [`resume`] for a resume held to the
//! session's kind. None of them reads a table or the disk: whether a kind builds and whether a
//! model can take a launch come in through a [`Probe`]. The variant
//! follows one rule, [`variant`], on every path.

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use crate::agents::{self, TierSkip, TierSkipReason, TierStamp, Variant};

use super::definitions::{DefinitionCause, DefinitionErr};
use super::tiers::{ModelTier, Routing, TierConfig, TierPreference};

#[cfg(test)]
mod tests;

/// Why a model on a kind cannot take a launch now, or `None` when it can.
type Availability = dyn Fn(&str, &str) -> Option<TierSkipReason> + Send + Sync;

/// What a launch asks of every agent cell it starts: `--agent`, `--model`, `--tier`, the tier
/// table they walk, which models can take a launch now, and, for a resume, the engine the
/// session's launch recorded. The default is a fresh launch with no flags that reads every model
/// as available.
#[derive(Clone, Default)]
pub struct Selection {
    agent: Option<String>,
    model: Option<String>,
    tier: Option<ModelTier>,
    tiers: TierConfig,
    availability: Option<Arc<Availability>>,
    session: Option<Recorded>,
}

/// The engine a resumed session's launch recorded, which a resume holds every cell to.
#[derive(Clone, Debug)]
struct Recorded {
    kind: String,
    /// The record's model when a record exists; `Some(None)` when it ran with none.
    model: Option<Option<String>>,
    stamp: Option<TierStamp>,
}

impl std::fmt::Debug for Selection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Selection")
            .field("agent", &self.agent)
            .field("model", &self.model)
            .field("tier", &self.tier)
            .field("availability", &self.availability.is_some())
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

impl Selection {
    /// A launch with no flags over `tiers`.
    pub fn new(tiers: &TierConfig) -> Self {
        Self {
            tiers: tiers.clone(),
            ..Self::default()
        }
    }

    /// Sets `--agent`; a blank value is none.
    #[must_use]
    pub fn agent(mut self, agent: Option<&str>) -> Self {
        self.agent = nonblank(agent);
        self
    }

    /// Sets `--model`; a blank value is none.
    #[must_use]
    pub fn model(mut self, model: Option<&str>) -> Self {
        self.model = nonblank(model);
        self
    }

    #[must_use]
    pub fn tier(mut self, tier: Option<ModelTier>) -> Self {
        self.tier = tier;
        self
    }

    /// Sets which models can take a launch now.
    #[must_use]
    pub fn availability(
        mut self,
        unavailable: impl Fn(&str, &str) -> Option<TierSkipReason> + Send + Sync + 'static,
    ) -> Self {
        self.availability = Some(Arc::new(unavailable));
        self
    }

    /// Holds every cell to the resumed session's `kind`, replaying the model its launch
    /// recorded when a record exists (`Some(None)`: it ran with none) and its tier stamp.
    #[must_use]
    pub(crate) fn resume(
        mut self,
        kind: &str,
        recorded: Option<Option<&str>>,
        stamp: Option<&TierStamp>,
    ) -> Self {
        self.session = Some(Recorded {
            kind: kind.to_owned(),
            model: recorded.map(|model| model.map(str::to_owned)),
            stamp: stamp.cloned(),
        });
        self
    }

    /// The resume this selection asks of a cell with L and X, when it resumes a session.
    pub(crate) fn resume_of<'a>(
        &'a self,
        l: Side<'a>,
        x: Option<Side<'a>>,
        role: Option<&'a str>,
    ) -> Option<Resume<'a>> {
        let session = self.session.as_ref()?;
        Some(Resume {
            session: &session.kind,
            l,
            x,
            role,
            model: self.model.as_deref(),
            tier: self.tier,
            recorded: session.model.as_ref().map(Option::as_deref),
            stamp: session.stamp.as_ref(),
        })
    }

    /// Whether this selection resumes a session.
    pub(crate) fn resumes(&self) -> bool {
        self.session.is_some()
    }

    pub fn agent_name(&self) -> Option<&str> {
        self.agent.as_deref()
    }

    pub fn model_flag(&self) -> Option<&str> {
        self.model.as_deref()
    }

    pub fn tier_flag(&self) -> Option<ModelTier> {
        self.tier
    }

    pub(crate) fn tiers(&self) -> &TierConfig {
        &self.tiers
    }

    /// The same launch with `--agent`, `--model` and `--tier` dropped, for the cells a launch
    /// builds but does not start.
    #[must_use]
    pub(crate) fn without_flags(&self) -> Self {
        Self {
            agent: None,
            model: None,
            tier: None,
            ..self.clone()
        }
    }

    pub(crate) fn unavailable(&self, kind: &str, model: &str) -> Option<TierSkipReason> {
        self.availability.as_ref()?(kind, model)
    }
}

fn nonblank(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// The engine a definition, a launch or a resume runs on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Engine {
    pub kind: String,
    pub variant: Option<&'static Variant>,
    pub model: Model,
    /// Set when a tier chose the model.
    pub tier: Option<TierStamp>,
}

/// Where an engine's model comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Model {
    /// A written line, a flag, a tier entry or a launch record names it.
    Named(String),
    /// Nothing names one: the final kind's base model fills it, then the provider's default.
    FromBase,
    /// The recorded launch ran with none, so no layer fills one on replay.
    Unset,
}

impl Model {
    fn written(model: Option<&str>) -> Self {
        model.map_or(Self::FromBase, |model| Self::Named(model.to_owned()))
    }

    pub(crate) fn named(&self) -> Option<&str> {
        match self {
            Self::Named(model) => Some(model),
            Self::FromBase | Self::Unset => None,
        }
    }
}

/// Why no engine could be chosen.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum EngineErr {
    /// The written lines or the flags contradict each other.
    #[error("{0}")]
    Conflict(String),
    /// A definition the build under the chosen engine depends on failed: the chain, `--agent`,
    /// or the final kind's base.
    #[error("{}", .0.message)]
    DependsOnFailed(DefinitionErr),
    /// The build under the chosen engine failed for another reason.
    #[error("{}", .0.message)]
    Unbuildable(DefinitionErr),
    /// No entry at the tier, or above it where the walk climbs, builds and takes a launch.
    /// `blocked` is the first failed definition an entry depended on, the one to fix.
    #[error("{reason}")]
    NoEligible {
        tier: ModelTier,
        reason: String,
        exclusions: BTreeMap<String, DefinitionErr>,
        blocked: Option<DefinitionCause>,
    },
    /// `--agent` runs on a kind other than the session's.
    #[error(
        "this session is {session}, but --agent {agent} is {requested}; resume with a same-kind \
         profile or launch fresh"
    )]
    OtherKind {
        session: String,
        agent: String,
        requested: String,
    },
    /// L now runs on another kind and does not build under the session's.
    #[error("now resolves to {now}, but this session is {was}")]
    KindChanged { was: String, now: String },
}

impl EngineErr {
    /// The refusal of the definition at `path` that asked for this engine.
    pub(super) fn into_definition_err(self, path: &Path) -> DefinitionErr {
        match self {
            Self::DependsOnFailed(error) | Self::Unbuildable(error) => error,
            Self::NoEligible {
                reason, blocked, ..
            } => DefinitionErr::new(path, reason)
                .with_cause(blocked.unwrap_or(DefinitionCause::Invalid)),
            other => DefinitionErr::new(path, other.to_string()),
        }
    }
}

/// Builds a launch under a kind and variant with a model (`None`: the kind base's).
pub(crate) type Build<'a, T> =
    dyn FnMut(&str, Option<&'static Variant>, Option<&str>) -> Result<T, DefinitionErr> + 'a;

/// What one choice is told about the world. `T` is what a build yields, handed back with the
/// engine it was built under.
pub(crate) struct Probe<'a, T> {
    /// Builds the launch under a kind and variant with a model (`None`: the kind base's), or
    /// says why it cannot.
    pub build: &'a mut Build<'a, T>,
    /// Why a model on a kind cannot take a launch now, or `None` when it can.
    pub unavailable: &'a mut dyn FnMut(&str, &str) -> Option<TierSkipReason>,
}

/// The lines a definition's chain writes about its kind, merged leaf first, before the kind
/// base.
pub(super) struct Lines<'a> {
    /// The kind and variant the chain's `agent:` lines end on, before a model moves them; empty
    /// for a chain rooted in a tier alone.
    pub kind: &'a str,
    pub variant: Option<&'static Variant>,
    pub model: Option<&'a str>,
    /// The family a model no provider owns runs on.
    pub family: Option<&'a str>,
}

/// One side of a launch: the launched name L, or the `--agent` value X.
#[derive(Clone, Debug)]
pub(crate) struct Side<'a> {
    /// The name as the user gave it, for messages.
    pub name: &'a str,
    /// The kind and variant it runs on with no flag.
    pub kind: &'a str,
    pub variant: Option<&'static Variant>,
    /// The model or tier its written lines set, merged over its chain before the kind base.
    pub model: Option<&'a str>,
    pub tier: Option<ModelTier>,
    pub preference: TierPreference,
    /// Whether it names a definition file, a kind base included; a bare provider name does not.
    pub file: bool,
}

impl<'a> Side<'a> {
    /// A bare provider name: no file and no written lines.
    pub(crate) fn bare(name: &'a str, kind: &'a str, variant: Option<&'static Variant>) -> Self {
        Self {
            name,
            kind,
            variant,
            model: None,
            tier: None,
            preference: family(kind),
            file: false,
        }
    }

    fn writes_engine(&self) -> bool {
        self.model.is_some() || self.tier.is_some()
    }
}

/// A fresh launch: L, X, the model the launched team role writes, and the `--model` and
/// `--tier` flags.
pub(crate) struct Launch<'a> {
    pub l: Side<'a>,
    pub x: Option<Side<'a>>,
    pub role: Option<&'a str>,
    pub model: Option<&'a str>,
    pub tier: Option<ModelTier>,
}

/// A resume of a session: its kind, L as its lines stand now, X, the flags, and what the
/// session's launch recorded.
pub(crate) struct Resume<'a> {
    pub session: &'a str,
    pub l: Side<'a>,
    pub x: Option<Side<'a>>,
    /// The model the resumed team role writes, which only X's fresh choice ranks.
    pub role: Option<&'a str>,
    pub model: Option<&'a str>,
    pub tier: Option<ModelTier>,
    /// The recorded launch's model when a record exists; `Some(None)` when it ran with none.
    pub recorded: Option<Option<&'a str>>,
    pub stamp: Option<&'a TierStamp>,
}

/// The variant a launch runs: its owner's (X's when the launch names one, L's otherwise), kept
/// only when the final kind is the owner's kind.
pub(crate) fn variant(
    kind: &str,
    owner_kind: &str,
    owner_variant: Option<&'static Variant>,
) -> Option<&'static Variant> {
    owner_variant.filter(|_| kind == owner_kind)
}

/// The kind `model` moves an engine on `kind` to: the model's provider, unless `kind` keeps
/// the model.
pub(super) fn model_moves_to(kind: Option<&str>, model: &str) -> Option<&'static str> {
    agents::definition_model_kind(model)
        .filter(|_| !kind.is_some_and(|kind| agents::keeps_model(kind, model)))
}

/// Whether a model inherited from `from_kind` survives a switch to `to_kind`.
pub(crate) fn keeps_inherited_model(from_kind: &str, to_kind: &str, model: &str) -> bool {
    from_kind == to_kind
        || agents::definition_model_kind(model) == Some(to_kind)
        || agents::keeps_model(to_kind, model)
}

/// The kind a team seat runs on: a model its role writes moves it to the model's provider
/// unless the agent runs every provider's models, and a model its chain wrote moves it unless
/// the agent keeps that model.
pub(crate) fn seat_kind<'a>(
    agent_kind: &'a str,
    model: Option<&str>,
    role_writes_model: bool,
) -> &'a str {
    model
        .and_then(|model| {
            let kept = if role_writes_model {
                agents::runs_other_providers_models(agent_kind)
            } else {
                agents::keeps_model(agent_kind, model)
            };
            agents::definition_model_kind(model).filter(|_| !kept)
        })
        .unwrap_or(agent_kind)
}

fn family(kind: &str) -> TierPreference {
    TierPreference {
        model: None,
        family: Some(kind.to_owned()),
    }
}

/// The kind and variant a definition's lines run on before any tier walk: a provider that runs
/// every provider's models keeps them, a model of another provider moves them there, and a model
/// no provider owns runs on the chain's family.
pub(super) fn file_kind<'a>(
    lines: &Lines<'a>,
) -> Result<(&'a str, Option<&'static Variant>), EngineErr> {
    if agents::runs_other_providers_models(lines.kind) {
        return Ok((lines.kind, lines.variant));
    }
    let kind = if let Some(owner) = lines
        .model
        .and_then(|model| model_moves_to(Some(lines.kind), model))
    {
        owner
    } else if lines.model.is_some() {
        lines.family.ok_or_else(|| {
            EngineErr::Conflict(
                "model names no runtime and the chain has no family; set `agent:`".to_owned(),
            )
        })?
    } else {
        lines.kind
    };
    Ok((kind, variant(kind, lines.kind, lines.variant)))
}

/// A definition's own engine: its lines' kind, or the walk of its tier over the kinds that
/// build.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "validation asks it in a later step")
)]
pub(super) fn file<T>(
    lines: &Lines<'_>,
    tier: Option<ModelTier>,
    preference: &TierPreference,
    tiers: &TierConfig,
    probe: &mut Probe<'_, T>,
) -> Result<(Engine, T), EngineErr> {
    let (kind, variant) = file_kind(lines)?;
    match tier {
        Some(tier) => walk_tier(tiers, tier, preference, kind, variant, probe),
        None => build(kind, variant, Model::written(lines.model), None, probe),
    }
}

/// Refuses flags that contradict each other or `--agent`.
pub(crate) fn check_flags(
    x: Option<&Side<'_>>,
    model: Option<&str>,
    tier: Option<ModelTier>,
) -> Result<(), EngineErr> {
    if tier.is_some() && model.is_some() {
        return Err(EngineErr::Conflict(
            "choose one of --tier and --model".to_owned(),
        ));
    }
    if let Some(tier) = model.and_then(ModelTier::from_model) {
        return Err(EngineErr::Conflict(format!("use `--tier {tier}`")));
    }
    if let (Some(x), Some(model)) = (x, model) {
        crate::harness::spec::check_agent_model_family(x.name, x.kind, model)
            .map_err(EngineErr::Conflict)?;
    }
    Ok(())
}

/// A fresh launch's engine, flags first: `--tier` walks under X's family, else L's preference;
/// `--model` runs on X's kind, else moves a launch that names a file to the model's provider;
/// then X's written engine; then the role's model, on X's kind where that kind keeps it from the
/// kind the role moves the seat to, else on that kind; then L's tier, then X's kind with L's model
/// where that kind keeps it. The chosen engine is built through `probe`.
pub(crate) fn launch<T>(
    req: &Launch<'_>,
    tiers: &TierConfig,
    probe: &mut Probe<'_, T>,
) -> Result<(Engine, T), EngineErr> {
    check_flags(req.x.as_ref(), req.model, req.tier)?;
    let owner = req.x.as_ref().unwrap_or(&req.l);
    let walk_preference = || {
        req.x
            .as_ref()
            .map_or_else(|| req.l.preference.clone(), |x| family(x.kind))
    };
    if let Some(tier) = req.tier {
        return walk_tier(
            tiers,
            tier,
            &walk_preference(),
            owner.kind,
            owner.variant,
            probe,
        );
    }
    let (kind, model) = if let Some(model) = req.model {
        let kind = if let Some(x) = &req.x {
            x.kind
        } else if req.l.file {
            model_moves_to(Some(req.l.kind), model).unwrap_or(req.l.kind)
        } else {
            req.l.kind
        };
        (kind, Model::Named(model.to_owned()))
    } else if let Some(x) = req.x.as_ref().filter(|x| x.writes_engine()) {
        if let Some(tier) = x.tier {
            return walk_tier(tiers, tier, &x.preference, x.kind, x.variant, probe);
        }
        (x.kind, Model::written(x.model))
    } else if let Some(model) = req.role.filter(|model| {
        req.x.as_ref().is_none_or(|x| {
            keeps_inherited_model(seat_kind(req.l.kind, Some(model), true), x.kind, model)
        })
    }) {
        let kind = req
            .x
            .as_ref()
            .map_or_else(|| seat_kind(req.l.kind, Some(model), true), |x| x.kind);
        (kind, Model::Named(model.to_owned()))
    } else if let Some(tier) = req.l.tier {
        return walk_tier(
            tiers,
            tier,
            &walk_preference(),
            owner.kind,
            owner.variant,
            probe,
        );
    } else {
        let model = req
            .l
            .model
            .filter(|model| keeps_inherited_model(req.l.kind, owner.kind, model));
        (owner.kind, Model::written(model))
    };
    build(
        kind,
        variant(kind, owner.kind, owner.variant),
        model,
        None,
        probe,
    )
}

/// A resume's engine, held to the session's kind: X given on the resume (nothing recorded) of
/// another kind refuses, while a recorded X keeps the session's kind; `--tier` takes its
/// own row on that kind and never climbs; `--model` runs as given unless it names a tier or a
/// model the session's kind does not run; then the record's model when
/// a record exists, none included, keeping the stamp only when it names that model; else the
/// stamp's; else X as a fresh launch would choose it, on the session's kind with X's or L's model
/// where X's choice lands elsewhere; else L's tier, never climbing, or L's model where the
/// session's kind keeps it. A failed build under the session's kind reads as
/// [`EngineErr::KindChanged`] only when L now runs on another kind, X given or not.
pub(crate) fn resume<T>(
    req: &Resume<'_>,
    tiers: &TierConfig,
    probe: &mut Probe<'_, T>,
) -> Result<(Engine, T), EngineErr> {
    let session = req.session;
    // A recorded X ran where its tier walk landed, which can be another kind than its own.
    let given_x = req
        .x
        .as_ref()
        .filter(|_| req.recorded.is_none() && req.stamp.is_none());
    check_flags(given_x, req.model, req.tier)?;
    if let Some(x) = given_x
        && x.kind != session
    {
        return Err(EngineErr::OtherKind {
            session: session.to_owned(),
            agent: x.name.to_owned(),
            requested: x.kind.to_owned(),
        });
    }
    if let Some(model) = req.model
        && let Some(owner) = model_moves_to(Some(session), model).filter(|owner| *owner != session)
    {
        return Err(EngineErr::Conflict(format!(
            "this session is {session}, but --model {model} runs on {owner}; resume with a \
             {session} model or launch fresh"
        )));
    }
    let owner = req.x.as_ref().unwrap_or(&req.l);
    let variant = variant(session, owner.kind, owner.variant);
    let (model, tier) = if let Some(tier) = req.tier {
        let stamp = session_tier(tiers, tier, session, &mut *probe.unavailable)?;
        (Model::Named(stamp.model.clone()), Some(stamp))
    } else if let Some(model) = req.model {
        (Model::Named(model.to_owned()), None)
    } else if let Some(recorded) = req.recorded {
        let model = recorded.map_or(Model::Unset, |model| Model::Named(model.to_owned()));
        let tier = req
            .stamp
            .filter(|stamp| model.named() == Some(stamp.model.as_str()))
            .cloned();
        (model, tier)
    } else if let Some(stamp) = req.stamp {
        (Model::Named(stamp.model.clone()), Some(stamp.clone()))
    } else if let Some(x) = &req.x {
        let fresh = Launch {
            l: req.l.clone(),
            x: Some(x.clone()),
            role: req.role,
            model: None,
            tier: None,
        };
        if let Ok(built) = launch(&fresh, tiers, probe)
            && built.0.kind == session
        {
            return Ok(built);
        }
        let model = if x.writes_engine() {
            x.model
        } else {
            req.l.model
        };
        (
            Model::written(model.filter(|model| keeps_inherited_model(x.kind, session, model))),
            None,
        )
    } else if let Some(tier) = req.l.tier {
        let stamp = session_tier(tiers, tier, session, &mut *probe.unavailable)?;
        (Model::Named(stamp.model.clone()), Some(stamp))
    } else {
        let model = req
            .l
            .model
            .filter(|model| keeps_inherited_model(req.l.kind, session, model));
        (Model::written(model), None)
    };
    let changed = (req.l.kind != session).then(|| req.l.kind.to_owned());
    build(session, variant, model, tier, probe).map_err(|error| match changed {
        Some(now) => EngineErr::KindChanged {
            was: session.to_owned(),
            now,
        },
        None => error,
    })
}

fn build<T>(
    kind: &str,
    variant: Option<&'static Variant>,
    model: Model,
    tier: Option<TierStamp>,
    probe: &mut Probe<'_, T>,
) -> Result<(Engine, T), EngineErr> {
    let built = (probe.build)(kind, variant, model.named()).map_err(|error| {
        if matches!(error.cause, DefinitionCause::DependsOnFailed { .. }) {
            EngineErr::DependsOnFailed(error)
        } else {
            EngineErr::Unbuildable(error)
        }
    })?;
    Ok((
        Engine {
            kind: kind.to_owned(),
            variant,
            model,
            tier,
        },
        built,
    ))
}

/// Walks `tier` upward under `preference`: builds each candidate kind once (the one engine
/// when the family runs every provider's models, else each kind the tier table names from
/// `tier` up), then picks as [`pick_tier`] does.
pub(super) fn walk_tier<T>(
    tiers: &TierConfig,
    tier: ModelTier,
    preference: &TierPreference,
    owner_kind: &str,
    owner_variant: Option<&'static Variant>,
    probe: &mut Probe<'_, T>,
) -> Result<(Engine, T), EngineErr> {
    let client = preference
        .family
        .as_deref()
        .filter(|kind| agents::runs_other_providers_models(kind));
    let candidates: Vec<(&str, Option<&str>)> = if let Some(client) = client {
        vec![(client, None)]
    } else {
        tiers
            .entries(tier)
            .filter_map(|(_, kind, model)| kind.map(|kind| (kind, Some(model))))
            .collect()
    };
    let mut built = BTreeMap::new();
    let mut exclusions = BTreeMap::new();
    for (kind, model) in candidates {
        if built.contains_key(kind) || exclusions.contains_key(kind) {
            continue;
        }
        match (probe.build)(kind, variant(kind, owner_kind, owner_variant), model) {
            Ok(render) => {
                built.insert(kind.to_owned(), render);
            }
            Err(error) => {
                exclusions.insert(kind.to_owned(), error);
            }
        }
    }
    let owner = (owner_kind, owner_variant);
    pick_tier(
        tiers,
        tier,
        preference,
        owner,
        built,
        &exclusions,
        &mut *probe.unavailable,
    )
}

/// Walks `tier` upward under `preference` over kinds already built (`built`) or refused
/// (`exclusions`): passes over the refused kinds and the models `unavailable` names, takes the
/// first buildable entry when every one is unavailable, and never descends. The variant is the
/// owner's, by [`variant`].
pub(super) fn pick_tier<T>(
    tiers: &TierConfig,
    tier: ModelTier,
    preference: &TierPreference,
    (owner_kind, owner_variant): (&str, Option<&'static Variant>),
    mut built: BTreeMap<String, T>,
    exclusions: &BTreeMap<String, DefinitionErr>,
    unavailable: &mut dyn FnMut(&str, &str) -> Option<TierSkipReason>,
) -> Result<(Engine, T), EngineErr> {
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
            |kind, model| unavailable(kind, model),
        )
        .map_err(|error| EngineErr::NoEligible {
            tier,
            reason: error.to_string(),
            exclusions: exclusions.clone(),
            blocked: blocked.get().cloned(),
        })?;
    let render = built
        .remove(&pick.kind)
        .ok_or_else(|| EngineErr::Conflict(format!("no eligible build for {}", pick.kind)))?;
    let engine = Engine {
        variant: variant(&pick.kind, owner_kind, owner_variant),
        model: Model::Named(pick.model.clone()),
        tier: Some(TierStamp {
            tier,
            model: pick.model,
            used_tier: pick.used_tier,
            skipped: pick
                .skipped
                .into_iter()
                .map(|(model, reason)| TierSkip { model, reason })
                .collect(),
            unbuildable: pick.unbuildable,
        }),
        kind: pick.kind,
    };
    Ok((engine, render))
}

/// The model `tier` gives a resume on `kind`: the first entry of that tier alone, on `kind` or
/// any provider when `kind` runs every provider's models, that can take a launch, else that
/// tier's first such entry. It never climbs.
pub(super) fn session_tier(
    tiers: &TierConfig,
    tier: ModelTier,
    kind: &str,
    unavailable: &mut dyn FnMut(&str, &str) -> Option<TierSkipReason>,
) -> Result<TierStamp, EngineErr> {
    let runs_any = agents::runs_other_providers_models(kind);
    let mut skipped = Vec::new();
    let mut first = None;
    let model = tiers
        .entries(tier)
        .filter(|(row, family, _)| *row == tier && (runs_any || *family == Some(kind)))
        .find_map(|(_, _, model)| {
            first.get_or_insert_with(|| model.to_owned());
            if tiers.routing == Routing::Fallback
                && let Some(reason) = unavailable(kind, model)
            {
                skipped.push(TierSkip {
                    model: model.to_owned(),
                    reason,
                });
                return None;
            }
            Some(model.to_owned())
        })
        .or_else(|| {
            // Match the fresh walk's best-effort availability, without climbing.
            skipped.clear();
            first
        })
        .ok_or_else(|| {
            let working = tiers
                .entries(ModelTier::Intern)
                .filter_map(|(tier, family, _)| (runs_any || family == Some(kind)).then_some(tier))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .map(|tier| tier.to_string())
                .collect::<Vec<_>>();
            EngineErr::NoEligible {
                tier,
                reason: format!(
                    "tier {tier} has no entry for {kind}; tiers carrying {kind}: {}",
                    if working.is_empty() {
                        "none".into()
                    } else {
                        working.join(", ")
                    }
                ),
                exclusions: BTreeMap::new(),
                blocked: None,
            }
        })?;
    Ok(TierStamp {
        tier,
        model,
        used_tier: None,
        unbuildable: Vec::new(),
        skipped,
    })
}
