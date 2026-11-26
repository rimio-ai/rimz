use std::collections::BTreeMap;
use std::path::PathBuf;

use super::*;

/// What a probe is told: the kinds whose build fails and why, and the models that cannot take a
/// launch.
#[derive(Default)]
struct World {
    broken: BTreeMap<&'static str, DefinitionCause>,
    unavailable: Vec<(&'static str, &'static str)>,
    tiers: TierConfig,
}

impl World {
    fn broken(mut self, kind: &'static str, cause: DefinitionCause) -> Self {
        self.broken.insert(kind, cause);
        self
    }

    fn unavailable(mut self, kind: &'static str, model: &'static str) -> Self {
        self.unavailable.push((kind, model));
        self
    }

    fn tiers(mut self, toml: &str) -> Self {
        self.tiers = toml::from_str(toml).expect("test tier table parses");
        self
    }

    /// Runs one entry and spells its outcome: `kind/variant/model/tier`, where the model is
    /// `base` for one the kind base fills and `unset` for none, and the tier carries the row it
    /// climbed to, the models it skipped and the entries that did not build; or the error's
    /// variant and message.
    fn run(
        &self,
        entry: impl FnOnce(&TierConfig, &mut Probe<'_, ()>) -> Result<(Engine, ()), EngineErr>,
    ) -> String {
        let mut build = |kind: &str, _: Option<&'static Variant>, _: Option<&str>| {
            self.broken.get(kind).map_or(Ok(()), |cause| {
                Err(DefinitionErr {
                    path: PathBuf::from("probe.md"),
                    message: format!("{kind} does not build"),
                    cause: cause.clone(),
                })
            })
        };
        let mut unavailable = |kind: &str, model: &str| {
            self.unavailable
                .contains(&(kind, model))
                .then_some(TierSkipReason::LoggedOut)
        };
        let mut probe = Probe {
            build: &mut build,
            unavailable: &mut unavailable,
        };
        match entry(&self.tiers, &mut probe) {
            Ok((engine, ())) => spell(&engine),
            Err(error) => spell_err(&error),
        }
    }
}

fn spell(engine: &Engine) -> String {
    let model = match &engine.model {
        Model::Named(model) => model.as_str(),
        Model::FromBase => "base",
        Model::Unset => "unset",
    };
    let tier = engine.tier.as_ref().map_or_else(
        || "-".to_owned(),
        |stamp| {
            let mut tier = stamp.tier.to_string();
            if let Some(used) = stamp.used_tier {
                tier.push_str(&format!("^{used}"));
            }
            for skip in &stamp.skipped {
                tier.push_str(&format!(" skip:{}", skip.model));
            }
            for pass in &stamp.unbuildable {
                tier.push_str(&format!(" unbuilt:{}", pass.model));
            }
            tier
        },
    );
    format!(
        "{}/{}/{model}/{tier}",
        engine.kind,
        engine.variant.map_or("-", |variant| variant.name)
    )
}

fn spell_err(error: &EngineErr) -> String {
    let name = match error {
        EngineErr::Conflict(_) => "Conflict",
        EngineErr::DependsOnFailed(_) => "DependsOnFailed",
        EngineErr::Unbuildable(_) => "Unbuildable",
        EngineErr::NoEligible {
            blocked: Some(_), ..
        } => "NoEligible(blocked)",
        EngineErr::NoEligible { .. } => "NoEligible",
        EngineErr::OtherKind { .. } => "OtherKind",
        EngineErr::KindChanged { .. } => "KindChanged",
    };
    format!("{name}: {error}")
}

fn mini() -> Option<&'static Variant> {
    agents::find_runtime("opencode-mini").and_then(|runtime| runtime.variant)
}

fn failed_parent() -> DefinitionCause {
    DefinitionCause::DependsOnFailed {
        name: "parent".to_owned(),
        subagent: false,
    }
}

fn lines<'a>(kind: &'a str, model: Option<&'a str>) -> Lines<'a> {
    Lines {
        kind,
        variant: None,
        model,
        family: (!kind.is_empty()).then_some(kind),
    }
}

fn prefer(family: Option<&str>) -> TierPreference {
    TierPreference {
        model: None,
        family: family.map(str::to_owned),
    }
}

/// A definition file on `kind` that writes `model` or `tier`.
fn def<'a>(
    name: &'a str,
    kind: &'a str,
    model: Option<&'a str>,
    tier: Option<ModelTier>,
) -> Side<'a> {
    Side {
        name,
        kind,
        variant: None,
        model,
        tier,
        preference: family(kind),
        file: true,
    }
}

fn launch_of<'a>(l: Side<'a>, x: Option<Side<'a>>) -> Launch<'a> {
    Launch {
        l,
        x,
        model: None,
        tier: None,
    }
}

fn resume_of<'a>(session: &'a str, l: Side<'a>) -> Resume<'a> {
    Resume {
        session,
        l,
        x: None,
        model: None,
        tier: None,
        recorded: None,
        stamp: None,
    }
}

fn stamp(tier: ModelTier, model: &str) -> TierStamp {
    TierStamp {
        tier,
        model: model.to_owned(),
        used_tier: None,
        skipped: Vec::new(),
        unbuildable: Vec::new(),
    }
}

/// Every row is a form and its inputs against the engine it must get. The default table:
/// intern luna (codex) and haiku, junior sol (codex) and sonnet, senior opus and astra (codex),
/// principal fable; claude owns the rest, and opencode runs every provider's models.
#[test]
fn each_entry_chooses_kind_model_tier_and_variant_by_one_rule() {
    use ModelTier::{Junior, Principal, Senior};
    let world = World::default;
    let senior_stamp = stamp(Senior, "astra");
    let rows: Vec<(&str, String, &str)> = vec![
        // A definition file's own engine.
        (
            "file: a model of its own kind stays",
            world().run(|t, p| file(&lines("claude", Some("sonnet")), None, &prefer(None), t, p)),
            "claude/-/sonnet/-",
        ),
        (
            "file: another provider's model moves the kind",
            world().run(|t, p| file(&lines("claude", Some("astra")), None, &prefer(None), t, p)),
            "codex/-/astra/-",
        ),
        (
            "file: a model client keeps any model and its variant",
            world().run(|t, p| {
                let lines = Lines {
                    variant: mini(),
                    ..lines("opencode", Some("opus"))
                };
                file(&lines, None, &prefer(None), t, p)
            }),
            "opencode/opencode-mini/opus/-",
        ),
        (
            "file: a model no provider owns runs on the family",
            world().run(|t, p| {
                let lines = Lines {
                    family: Some("codex"),
                    ..lines("codex", Some("local-model"))
                };
                file(&lines, None, &prefer(None), t, p)
            }),
            "codex/-/local-model/-",
        ),
        (
            "file: a model no provider owns with no family refuses",
            world().run(|t, p| file(&lines("", Some("local-model")), None, &prefer(None), t, p)),
            "Conflict: model names no runtime and the chain has no family; set `agent:`",
        ),
        (
            "file: a tier walks its family first",
            world().run(|t, p| {
                file(
                    &lines("codex", None),
                    Some(Senior),
                    &prefer(Some("codex")),
                    t,
                    p,
                )
            }),
            "codex/-/astra/senior",
        ),
        (
            "file: a tier passes over a kind that does not build",
            world()
                .broken("codex", DefinitionCause::Invalid)
                .run(|t, p| {
                    file(
                        &lines("codex", None),
                        Some(Senior),
                        &prefer(Some("codex")),
                        t,
                        p,
                    )
                }),
            "claude/-/opus/senior unbuilt:astra",
        ),
        (
            "file: a tier passes over an unavailable model",
            world().unavailable("claude", "opus").run(|t, p| {
                file(
                    &lines("claude", None),
                    Some(Senior),
                    &prefer(Some("claude")),
                    t,
                    p,
                )
            }),
            "codex/-/astra/senior skip:opus",
        ),
        (
            "file: a tier with no family takes the table's order",
            world().run(|t, p| file(&lines("", None), Some(Junior), &prefer(None), t, p)),
            "codex/-/sol/junior",
        ),
        (
            "file: a tier climbs to the next row with an entry that builds",
            world()
                .tiers("junior = [\"sonnet\"]")
                .broken("claude", DefinitionCause::Invalid)
                .run(|t, p| {
                    file(
                        &lines("codex", None),
                        Some(Junior),
                        &prefer(Some("codex")),
                        t,
                        p,
                    )
                }),
            "codex/-/astra/junior^senior unbuilt:sonnet",
        ),
        (
            "file: a tier on a model client runs the first entry there",
            world().run(|t, p| {
                let lines = Lines {
                    variant: mini(),
                    ..lines("opencode", None)
                };
                file(&lines, Some(Senior), &prefer(Some("opencode")), t, p)
            }),
            "opencode/opencode-mini/opus/senior",
        ),
        (
            "file: a tier blocked by a failed dependency names it",
            world().broken("claude", failed_parent()).run(|t, p| {
                file(
                    &lines("claude", None),
                    Some(Principal),
                    &prefer(Some("claude")),
                    t,
                    p,
                )
            }),
            "NoEligible(blocked): no eligible model at tier principal or above; claude: claude does not \
             build",
        ),
        (
            "file: a build blocked by a failed dependency stays typed",
            world()
                .broken("claude", failed_parent())
                .run(|t, p| file(&lines("claude", Some("sonnet")), None, &prefer(None), t, p)),
            "DependsOnFailed: claude does not build",
        ),
        (
            "file: any other build failure is typed",
            world()
                .broken("claude", DefinitionCause::Invalid)
                .run(|t, p| file(&lines("claude", Some("sonnet")), None, &prefer(None), t, p)),
            "Unbuildable: claude does not build",
        ),
        // A fresh launch: L, X and the flags.
        (
            "launch: no flags runs L's own model",
            world().run(|t, p| {
                launch(
                    &launch_of(def("rev", "claude", Some("sonnet"), None), None),
                    t,
                    p,
                )
            }),
            "claude/-/sonnet/-",
        ),
        (
            "launch: --model moves a file's kind to the model's provider",
            world().run(|t, p| {
                let req = Launch {
                    model: Some("astra"),
                    ..launch_of(def("rev", "claude", Some("sonnet"), None), None)
                };
                launch(&req, t, p)
            }),
            "codex/-/astra/-",
        ),
        (
            "launch: --model on a bare name keeps its kind",
            world().run(|t, p| {
                let req = Launch {
                    model: Some("astra"),
                    ..launch_of(Side::bare("claude", "claude", None), None)
                };
                launch(&req, t, p)
            }),
            "claude/-/astra/-",
        ),
        (
            "launch: --model runs on X's kind",
            world().run(|t, p| {
                let req = Launch {
                    model: Some("astra"),
                    ..launch_of(
                        def("rev", "claude", Some("sonnet"), None),
                        Some(Side::bare("codex", "codex", None)),
                    )
                };
                launch(&req, t, p)
            }),
            "codex/-/astra/-",
        ),
        (
            "launch: --model of another provider than X refuses",
            world().run(|t, p| {
                let req = Launch {
                    model: Some("opus"),
                    ..launch_of(
                        def("rev", "claude", None, None),
                        Some(Side::bare("codex", "codex", None)),
                    )
                };
                launch(&req, t, p)
            }),
            "Conflict: --agent codex runs on codex but --model opus runs on claude; choose the same family",
        ),
        (
            "launch: --tier and --model refuse together",
            world().run(|t, p| {
                let req = Launch {
                    model: Some("opus"),
                    tier: Some(Senior),
                    ..launch_of(def("rev", "claude", None, None), None)
                };
                launch(&req, t, p)
            }),
            "Conflict: choose one of --tier and --model",
        ),
        (
            "launch: a tier passed as --model refuses",
            world().run(|t, p| {
                let req = Launch {
                    model: Some("senior"),
                    ..launch_of(def("rev", "claude", None, None), None)
                };
                launch(&req, t, p)
            }),
            "Conflict: use `--tier senior`",
        ),
        (
            "launch: --tier walks under X's family",
            world().run(|t, p| {
                let req = Launch {
                    tier: Some(Senior),
                    ..launch_of(
                        def("rev", "claude", Some("sonnet"), None),
                        Some(Side::bare("codex", "codex", None)),
                    )
                };
                launch(&req, t, p)
            }),
            "codex/-/astra/senior",
        ),
        (
            "launch: --tier walks under L's preference",
            world().run(|t, p| {
                let req = Launch {
                    tier: Some(Senior),
                    ..launch_of(def("rev", "codex", Some("sol"), None), None)
                };
                launch(&req, t, p)
            }),
            "codex/-/astra/senior",
        ),
        (
            "launch: --tier under a model client keeps X's variant",
            world().run(|t, p| {
                let req = Launch {
                    tier: Some(Senior),
                    ..launch_of(
                        def("rev", "claude", None, None),
                        Some(Side::bare("opencode-mini", "opencode", mini())),
                    )
                };
                launch(&req, t, p)
            }),
            "opencode/opencode-mini/opus/senior",
        ),
        (
            "launch: --tier takes another family's entry on its row before it climbs",
            world().tiers("junior = [\"sonnet\"]").run(|t, p| {
                let req = Launch {
                    tier: Some(Junior),
                    ..launch_of(
                        def("rev", "codex", None, None),
                        Some(Side::bare("codex", "codex", None)),
                    )
                };
                launch(&req, t, p)
            }),
            "claude/-/sonnet/junior",
        ),
        (
            "launch: X's tier walks under X",
            world().run(|t, p| {
                let req = launch_of(
                    def("rev", "claude", Some("sonnet"), None),
                    Some(def("planner", "codex", None, Some(Senior))),
                );
                launch(&req, t, p)
            }),
            "codex/-/astra/senior",
        ),
        (
            "launch: X's model replaces L's",
            world().run(|t, p| {
                let req = launch_of(
                    def("rev", "claude", Some("sonnet"), None),
                    Some(def("planner", "claude", Some("opus"), None)),
                );
                launch(&req, t, p)
            }),
            "claude/-/opus/-",
        ),
        (
            "launch: a plain X of another kind drops L's model",
            world().run(|t, p| {
                let req = launch_of(
                    def("rev", "claude", Some("sonnet"), None),
                    Some(Side::bare("codex", "codex", None)),
                );
                launch(&req, t, p)
            }),
            "codex/-/base/-",
        ),
        (
            "launch: a plain X that is a model client keeps L's model",
            world().run(|t, p| {
                let req = launch_of(
                    def("rev", "claude", Some("sonnet"), None),
                    Some(Side::bare("opencode", "opencode", None)),
                );
                launch(&req, t, p)
            }),
            "opencode/-/sonnet/-",
        ),
        (
            "launch: L's tier walks under a plain X's family",
            world().run(|t, p| {
                let req = launch_of(
                    def("rev", "codex", None, Some(Senior)),
                    Some(Side::bare("claude", "claude", None)),
                );
                launch(&req, t, p)
            }),
            "claude/-/opus/senior",
        ),
        (
            "launch: the variant is X's when X is given",
            world().run(|t, p| {
                let l = Side {
                    variant: mini(),
                    ..def("rev", "opencode", None, None)
                };
                launch(
                    &launch_of(l, Some(Side::bare("opencode", "opencode", None))),
                    t,
                    p,
                )
            }),
            "opencode/-/base/-",
        ),
        (
            "launch: the variant is L's with no X",
            world().run(|t, p| {
                launch(
                    &launch_of(Side::bare("opencode-mini", "opencode", mini()), None),
                    t,
                    p,
                )
            }),
            "opencode/opencode-mini/base/-",
        ),
        (
            "launch: X whose build fails is typed",
            world()
                .broken("codex", DefinitionCause::Invalid)
                .run(|t, p| {
                    let req = launch_of(
                        def("rev", "claude", Some("sonnet"), None),
                        Some(Side::bare("codex", "codex", None)),
                    );
                    launch(&req, t, p)
                }),
            "Unbuildable: codex does not build",
        ),
        // A resume, held to the session's kind.
        (
            "resume: the record's model keeps the stamp it matches",
            world().run(|t, p| {
                let req = Resume {
                    recorded: Some(Some("astra")),
                    stamp: Some(&senior_stamp),
                    ..resume_of("codex", def("rev", "codex", None, Some(Senior)))
                };
                resume(&req, t, p)
            }),
            "codex/-/astra/senior",
        ),
        (
            "resume: the record's model wins over the stamp's",
            world().run(|t, p| {
                let req = Resume {
                    recorded: Some(Some("sol")),
                    stamp: Some(&senior_stamp),
                    ..resume_of("codex", def("rev", "codex", None, Some(Senior)))
                };
                resume(&req, t, p)
            }),
            "codex/-/sol/-",
        ),
        (
            "resume: a record with no model runs with none",
            world().run(|t, p| {
                let req = Resume {
                    recorded: Some(None),
                    stamp: Some(&senior_stamp),
                    ..resume_of("codex", def("rev", "codex", Some("sol"), None))
                };
                resume(&req, t, p)
            }),
            "codex/-/unset/-",
        ),
        (
            "resume: with no record the stamp's model runs",
            world().run(|t, p| {
                let req = Resume {
                    stamp: Some(&senior_stamp),
                    ..resume_of("codex", def("rev", "codex", None, Some(Senior)))
                };
                resume(&req, t, p)
            }),
            "codex/-/astra/senior",
        ),
        (
            "resume: --tier takes its own row on the session's kind",
            world().run(|t, p| {
                let req = Resume {
                    tier: Some(Senior),
                    ..resume_of("claude", def("rev", "claude", Some("sonnet"), None))
                };
                resume(&req, t, p)
            }),
            "claude/-/opus/senior",
        ),
        (
            "resume: --tier never climbs",
            world().tiers("junior = [\"sonnet\"]").run(|t, p| {
                let req = Resume {
                    tier: Some(Junior),
                    ..resume_of("codex", def("rev", "codex", None, None))
                };
                resume(&req, t, p)
            }),
            "NoEligible: tier junior has no entry for codex; tiers carrying codex: intern, senior",
        ),
        (
            "resume: --tier takes its first entry when none is available",
            world().unavailable("codex", "astra").run(|t, p| {
                let req = Resume {
                    tier: Some(Senior),
                    ..resume_of("codex", def("rev", "codex", None, None))
                };
                resume(&req, t, p)
            }),
            "codex/-/astra/senior",
        ),
        (
            "resume: --tier on a model client takes any provider's entry",
            world().run(|t, p| {
                let req = Resume {
                    tier: Some(Senior),
                    ..resume_of("opencode", Side::bare("opencode-mini", "opencode", mini()))
                };
                resume(&req, t, p)
            }),
            "opencode/opencode-mini/opus/senior",
        ),
        (
            "resume: --model runs as given on the session's kind",
            world().run(|t, p| {
                let req = Resume {
                    model: Some("luna"),
                    stamp: Some(&senior_stamp),
                    ..resume_of("codex", def("rev", "codex", None, Some(Senior)))
                };
                resume(&req, t, p)
            }),
            "codex/-/luna/-",
        ),
        (
            "resume: X of another kind refuses",
            world().run(|t, p| {
                let req = Resume {
                    x: Some(Side::bare("claude", "claude", None)),
                    ..resume_of("codex", def("rev", "codex", Some("sol"), None))
                };
                resume(&req, t, p)
            }),
            "OtherKind: this session is codex, but --agent claude is claude; resume with a same-kind profile or launch fresh",
        ),
        (
            "resume: X of the session's kind runs its model",
            world().run(|t, p| {
                let req = Resume {
                    x: Some(def("planner", "codex", Some("luna"), None)),
                    recorded: Some(Some("sol")),
                    ..resume_of("codex", def("rev", "codex", Some("sol"), None))
                };
                resume(&req, t, p)
            }),
            "codex/-/luna/-",
        ),
        (
            "resume: X's tier walks as a fresh launch's when it lands on the session's kind",
            world().run(|t, p| {
                let req = Resume {
                    x: Some(def("planner", "claude", None, Some(Principal))),
                    ..resume_of("claude", def("rev", "claude", None, None))
                };
                resume(&req, t, p)
            }),
            "claude/-/fable/principal",
        ),
        (
            "resume: L moved to another kind and the session's kind does not build",
            world()
                .broken("claude", DefinitionCause::Invalid)
                .run(|t, p| {
                    resume(
                        &resume_of("claude", def("rev", "codex", Some("astra"), None)),
                        t,
                        p,
                    )
                }),
            "KindChanged: now resolves to codex, but this session is claude",
        ),
        (
            "resume: L moved to another kind but the session's kind builds",
            world().run(|t, p| {
                resume(
                    &resume_of("claude", def("rev", "codex", Some("astra"), None)),
                    t,
                    p,
                )
            }),
            "claude/-/base/-",
        ),
        (
            "resume: a failed build on the session's own kind stays typed",
            world().broken("codex", failed_parent()).run(|t, p| {
                let req = Resume {
                    recorded: Some(Some("sol")),
                    ..resume_of("codex", def("rev", "codex", Some("sol"), None))
                };
                resume(&req, t, p)
            }),
            "DependsOnFailed: codex does not build",
        ),
    ];
    let (labels, actual): (Vec<_>, Vec<_>) = rows
        .iter()
        .map(|(label, actual, _)| (*label, format!("{label}: {actual}")))
        .unzip();
    let expected: Vec<_> = rows
        .iter()
        .zip(&labels)
        .map(|((_, _, expected), label)| format!("{label}: {expected}"))
        .collect();
    assert_eq!(actual, expected);
}
