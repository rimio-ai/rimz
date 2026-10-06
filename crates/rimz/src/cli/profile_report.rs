use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;
use serde::Serialize;

use super::render;
use rimz::config::effective::ProfileScope;
use rimz::harness::subagent_policy::{SubagentCatalog, SubagentProfile, SubagentProfileSource};

#[derive(Debug, PartialEq, Eq, Serialize)]
pub(crate) struct AgentProfileReport {
    pub(crate) name: String,
    pub(crate) source: &'static str,
    #[serde(skip)]
    brand_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tier: Option<rimz::config::tiers::ModelTier>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tier_fallback: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    allowed_tools: Vec<rimz::config::ToolRule>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) path: Option<PathBuf>,
}

pub(crate) fn available_profiles(
    profiles: &rimz::config::ProfilesConfig,
    commands: &rimz::config::CommandsConfig,
    sources: &rimz::config::AgentSpecSources,
    scope: ProfileScope,
) -> Vec<AgentProfileReport> {
    let mut reports = profiles
        .0
        .iter()
        .map(|(name, profile)| AgentProfileReport {
            name: name.clone(),
            source: "profile",
            brand_kind: Some(provider_brand_kind(&profile.agent, profiles).to_owned()),
            agent: Some(profile.agent.clone()),
            model: profile.model.clone(),
            tier: profile.model_tier.as_ref().map(|tier| tier.tier),
            tier_fallback: profile.model_tier.as_ref().map(|tier| tier.fell_back),
            effort: profile.effort.clone(),
            description: profile.description.clone(),
            allowed_tools: profile.allowed_tools.clone().unwrap_or_default(),
            path: sources.profile(scope, name).map(PathBuf::from),
        })
        .collect::<Vec<_>>();
    reports.extend(commands.0.keys().map(|name| AgentProfileReport {
        name: name.clone(),
        source: "command",
        brand_kind: None,
        agent: None,
        model: None,
        allowed_tools: Vec::new(),
        tier: None,
        tier_fallback: None,
        effort: None,
        description: None,
        path: sources.command(name).map(PathBuf::from),
    }));
    reports
}

/// Splits off the profiles named `<team>.<role>` for a configured team. The
/// launch grammar resolves those names as team roles, so they belong to
/// `rimz teams profiles` rather than the standalone agent catalog.
pub(crate) fn partition_team_profiles(
    reports: Vec<AgentProfileReport>,
    teams: &rimz::config::TeamsConfig,
) -> (Vec<AgentProfileReport>, Vec<AgentProfileReport>) {
    reports
        .into_iter()
        .partition(|report| report.source == "profile" && teams.role_spec(&report.name).is_some())
}

pub(crate) fn subagent_reports(
    catalog: SubagentCatalog,
    profiles: &rimz::config::ProfilesConfig,
    sources: &rimz::config::AgentSpecSources,
) -> Vec<AgentProfileReport> {
    let SubagentCatalog::Available(specs) = catalog else {
        return Vec::new();
    };
    specs
        .into_iter()
        .map(|spec| subagent_report(spec, profiles, sources))
        .collect()
}

fn subagent_report(
    profile: SubagentProfile,
    profiles: &rimz::config::ProfilesConfig,
    sources: &rimz::config::AgentSpecSources,
) -> AgentProfileReport {
    let (source, path) = match profile.source {
        SubagentProfileSource::Profile => (
            "profile",
            sources
                .profile(ProfileScope::Subagents, &profile.name)
                .map(PathBuf::from),
        ),
        SubagentProfileSource::Command => {
            ("command", sources.command(&profile.name).map(PathBuf::from))
        }
    };
    let brand_kind = profile
        .agent
        .as_deref()
        .map(|agent| provider_brand_kind(agent, profiles).to_owned());
    let tier = profiles
        .0
        .get(&profile.name)
        .and_then(|profile| profile.model_tier.as_ref());
    AgentProfileReport {
        allowed_tools: profiles
            .0
            .get(&profile.name)
            .and_then(|profile| profile.allowed_tools.clone())
            .unwrap_or_default(),
        tier: tier.map(|tier| tier.tier),
        tier_fallback: tier.map(|tier| tier.fell_back),
        name: profile.name,
        source,
        brand_kind,
        agent: profile.agent,
        model: profile.model,
        effort: profile.effort,
        description: profile.description,
        path,
    }
}

fn provider_brand_kind<'a>(
    raw_agent: &'a str,
    profiles: &'a rimz::config::ProfilesConfig,
) -> &'a str {
    let mut current = raw_agent;
    let mut seen = HashSet::new();
    while seen.insert(current) {
        let Some(profile) = profiles.0.get(current) else {
            return if rimz::agents::find_definition(current).is_some() {
                current
            } else {
                raw_agent
            };
        };
        let next = profile.agent.as_str();
        if next == current && rimz::agents::find_definition(next).is_some() {
            return next;
        }
        current = next;
    }
    raw_agent
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum ProfileListing {
    Agents { team_profiles_hidden: bool },
    Subagents,
    Teams,
}

pub(crate) fn list_profiles(
    mut reports: Vec<AgentProfileReport>,
    listing: ProfileListing,
    json: bool,
    show_path: bool,
) -> Result<()> {
    apply_path_visibility(&mut reports, show_path);
    if json {
        return render::json_pretty(&reports);
    }
    render::finish(profile_cards(&reports, listing, &mut render::out()))
}

fn apply_path_visibility(reports: &mut [AgentProfileReport], show_path: bool) {
    if !show_path {
        for report in reports {
            report.path = None;
        }
    }
}

fn profile_cards(
    reports: &[AgentProfileReport],
    listing: ProfileListing,
    out: &mut impl Write,
) -> std::io::Result<()> {
    if reports.is_empty() {
        let profile_directory = match listing {
            ProfileListing::Agents { .. } => "agents",
            ProfileListing::Subagents => "subagents",
            ProfileListing::Teams => {
                writeln!(out, "No team profiles configured.")?;
                writeln!(out, "Install a team with `rimz teams install forge`.")?;
                return Ok(());
            }
        };
        writeln!(out, "No profiles or commands configured.")?;
        writeln!(
            out,
            "Add {profile_directory}/<name>.md in your agents home, or [agents.commands] in config.toml."
        )?;
        if let ProfileListing::Agents {
            team_profiles_hidden: true,
        } = listing
        {
            writeln!(
                out,
                "Team role profiles are hidden; list them with `rimz teams profiles`."
            )?;
        }
        return Ok(());
    }

    for (index, report) in reports.iter().enumerate() {
        if index > 0 {
            writeln!(out)?;
        }

        let model = report
            .model
            .as_deref()
            .map(|model| model_label(model, report.tier, report.tier_fallback));
        let (name_style, segments) = match &report.agent {
            Some(agent) => {
                let mut segments = vec![agent.as_str()];
                segments.extend(model.as_deref());
                segments.extend(report.effort.as_deref());
                let brand_kind = report.brand_kind.as_deref().unwrap_or(agent);
                (render::palette::identity(brand_kind).bold(), segments)
            }
            None => (render::palette::muted(), vec!["command"]),
        };
        writeln!(
            out,
            "{} — {}",
            render::paint(name_style, &report.name),
            segments.join(" · ")
        )?;
        if let Some(description) = &report.description {
            writeln!(out, "  {description}")?;
        }
        if !report.allowed_tools.is_empty() {
            writeln!(
                out,
                "  allowed-tools: {}",
                report
                    .allowed_tools
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )?;
        }
        if let Some(path) = &report.path {
            writeln!(out, "  {}", render::home_relative(&path.to_string_lossy()))?;
        }
    }
    Ok(())
}

pub(crate) fn model_label(
    model: &str,
    tier: Option<rimz::config::tiers::ModelTier>,
    fallback: Option<bool>,
) -> String {
    match tier {
        Some(tier) => format!(
            "{model} ({tier}{})",
            if fallback == Some(true) {
                ", fallback"
            } else {
                ""
            }
        ),
        None => model.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_profiles_show_concrete_models_and_fallback_in_cards_and_json() {
        let profiles = rimz::config::ProfilesConfig(std::collections::BTreeMap::from([(
            "planner".into(),
            rimz::config::Profile {
                agent: "claude".into(),
                model: Some("fable".into()),
                effort: Some("high".into()),
                model_tier: Some(rimz::config::tiers::TierProvenance {
                    tier: rimz::config::tiers::ModelTier::Senior,
                    fell_back: true,
                }),
                ..toml::from_str("agent = 'claude'").unwrap()
            },
        )]));
        let reports = available_profiles(
            &profiles,
            &Default::default(),
            &Default::default(),
            ProfileScope::Agents,
        );
        let json = serde_json::to_value(&reports).unwrap();
        assert_eq!(json[0]["tier"], "senior");
        assert_eq!(json[0]["model"], "fable");
        let mut output = Vec::new();
        profile_cards(&reports, ProfileListing::Teams, &mut output).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("senior"));
        assert!(output.contains("fallback"));
    }

    #[test]
    fn profiles_render_as_cards_with_independent_model_and_effort_segments() {
        let reports = vec![
            AgentProfileReport {
                allowed_tools: Vec::new(),
                tier: None,
                tier_fallback: None,
                name: "planner".to_owned(),
                source: "profile",
                brand_kind: Some("codex".to_owned()),
                agent: Some("codex".to_owned()),
                model: Some("gpt-5.6".to_owned()),
                effort: Some("high".to_owned()),
                description: Some("Plans the work".to_owned()),
                path: Some(PathBuf::from("/tmp/.agents/agents/planner.md")),
            },
            AgentProfileReport {
                allowed_tools: Vec::new(),
                tier: None,
                tier_fallback: None,
                name: "reviewer".to_owned(),
                source: "profile",
                brand_kind: Some("claude".to_owned()),
                agent: Some("claude".to_owned()),
                model: None,
                effort: Some("max".to_owned()),
                description: Some("Reviews the result".to_owned()),
                path: None,
            },
            AgentProfileReport {
                allowed_tools: Vec::new(),
                tier: None,
                tier_fallback: None,
                name: "coder".to_owned(),
                source: "profile",
                brand_kind: Some("codex".to_owned()),
                agent: Some("codex".to_owned()),
                model: Some("gpt-5.6".to_owned()),
                effort: None,
                description: None,
                path: None,
            },
            AgentProfileReport {
                allowed_tools: Vec::new(),
                tier: None,
                tier_fallback: None,
                name: "lint".to_owned(),
                source: "command",
                brand_kind: None,
                agent: None,
                model: None,
                effort: None,
                description: None,
                path: Some(PathBuf::from("/tmp/rimz/config.toml")),
            },
        ];
        let mut output = Vec::new();

        profile_cards(
            &reports,
            ProfileListing::Agents {
                team_profiles_hidden: false,
            },
            &mut anstream::StripStream::new(&mut output),
        )
        .expect("render profile cards");

        insta::assert_snapshot!(String::from_utf8(output).expect("utf-8"), @r"
        planner — codex · gpt-5.6 · high
          Plans the work
          /tmp/.agents/agents/planner.md

        reviewer — claude · max
          Reviews the result

        coder — codex · gpt-5.6

        lint — command
          /tmp/rimz/config.toml
        ");
    }

    #[test]
    fn paths_are_opt_in_for_json() {
        let mut reports = vec![AgentProfileReport {
            allowed_tools: Vec::new(),
            tier: None,
            tier_fallback: None,
            name: "planner".to_owned(),
            source: "profile",
            brand_kind: Some("codex".to_owned()),
            agent: Some("codex".to_owned()),
            model: None,
            effort: None,
            description: None,
            path: Some(PathBuf::from("/tmp/.agents/agents/planner.md")),
        }];

        let json = serde_json::to_value(&reports).expect("serialize profile reports with paths");
        assert_eq!(json[0]["path"], "/tmp/.agents/agents/planner.md");

        apply_path_visibility(&mut reports, false);

        let json = serde_json::to_value(&reports).expect("serialize profile reports without paths");
        assert!(json[0].get("path").is_none());
    }

    #[test]
    fn empty_catalog_names_the_configuration_section() {
        let mut output = Vec::new();
        profile_cards(&[], ProfileListing::Subagents, &mut output).expect("render empty catalog");

        assert_eq!(
            String::from_utf8(output).expect("utf-8"),
            "No profiles or commands configured.\n\
             Add subagents/<name>.md in your agents home, or [agents.commands] in config.toml.\n"
        );
    }

    #[test]
    fn empty_agent_catalog_points_at_hidden_team_profiles() {
        let mut output = Vec::new();
        profile_cards(
            &[],
            ProfileListing::Agents {
                team_profiles_hidden: true,
            },
            &mut output,
        )
        .expect("render empty catalog");

        assert_eq!(
            String::from_utf8(output).expect("utf-8"),
            "No profiles or commands configured.\n\
             Add agents/<name>.md in your agents home, or [agents.commands] in config.toml.\n\
             Team role profiles are hidden; list them with `rimz teams profiles`.\n"
        );
    }

    #[test]
    fn disabled_subagent_catalog_has_no_reports() {
        assert!(
            subagent_reports(
                SubagentCatalog::Disabled,
                &rimz::config::ProfilesConfig::default(),
                &rimz::config::AgentSpecSources::default(),
            )
            .is_empty()
        );
    }

    #[test]
    fn allowed_tools_cards_and_json_follow_profile_and_subagent_catalogs() {
        for allowed_tools in [
            Some(vec![
                "Bash(git *)".parse().unwrap(),
                "Read".parse().unwrap(),
            ]),
            None,
            Some(Vec::new()),
        ] {
            let shown = allowed_tools
                .as_ref()
                .is_some_and(|rules| !rules.is_empty());
            let profiles = rimz::config::ProfilesConfig(
                [(
                    "fixer".into(),
                    rimz::config::Profile {
                        allowed_tools,
                        description: Some("Keeps branch current".into()),
                        ..profile("claude")
                    },
                )]
                .into(),
            );
            let commands = rimz::config::CommandsConfig::default();
            let sources = rimz::config::AgentSpecSources::default();
            let catalog = rimz::harness::subagent_policy::catalog(
                None,
                &Default::default(),
                &profiles,
                &commands,
            );
            for (reports, listing) in [
                (
                    available_profiles(&profiles, &commands, &sources, ProfileScope::Agents),
                    ProfileListing::Agents {
                        team_profiles_hidden: false,
                    },
                ),
                (
                    subagent_reports(catalog, &profiles, &sources),
                    ProfileListing::Subagents,
                ),
            ] {
                let json = serde_json::to_value(&reports).unwrap();
                let mut output = Vec::new();
                profile_cards(&reports, listing, &mut output).unwrap();
                let output = String::from_utf8(output).unwrap();
                if shown {
                    assert_eq!(
                        json[0]["allowed_tools"],
                        serde_json::json!(["Bash(git *)", "Read"])
                    );
                    assert!(
                        output.contains(
                            "  Keeps branch current\n  allowed-tools: Bash(git *), Read\n"
                        ),
                        "{output}"
                    );
                } else {
                    assert!(json[0].get("allowed_tools").is_none());
                    assert!(!output.contains("allowed-tools:"));
                }
            }
        }
    }

    #[test]
    fn chained_profiles_resolve_the_provider_for_brand_tint() {
        let profiles = rimz::config::ProfilesConfig(
            [
                ("deep".to_owned(), profile("planner")),
                ("planner".to_owned(), profile("claude")),
                ("claude".to_owned(), profile("claude")),
            ]
            .into(),
        );

        let reports = available_profiles(
            &profiles,
            &rimz::config::CommandsConfig::default(),
            &rimz::config::AgentSpecSources::default(),
            ProfileScope::Agents,
        );
        let deep = reports
            .iter()
            .find(|report| report.name == "deep")
            .expect("deep profile");
        assert_eq!(deep.agent.as_deref(), Some("planner"));
        assert_eq!(deep.brand_kind.as_deref(), Some("claude"));
        assert!(
            serde_json::to_value(deep)
                .expect("serialize profile")
                .get("brand_kind")
                .is_none()
        );
    }

    #[test]
    fn team_role_profiles_split_from_standalone_profiles() {
        let profiles = rimz::config::ProfilesConfig(
            [
                ("forge.coder".to_owned(), profile("claude")),
                ("gone.coder".to_owned(), profile("claude")),
                ("coder".to_owned(), profile("claude")),
            ]
            .into(),
        );
        let commands =
            rimz::config::CommandsConfig([("forge.lint".to_owned(), "lint".to_owned())].into());
        let teams =
            rimz::config::TeamsConfig([("forge".to_owned(), rimz::config::Team::default())].into());
        let reports = available_profiles(
            &profiles,
            &commands,
            &rimz::config::AgentSpecSources::default(),
            ProfileScope::Agents,
        );

        let (team, standalone) = partition_team_profiles(reports, &teams);

        let names = |reports: &[AgentProfileReport]| {
            reports
                .iter()
                .map(|report| report.name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&team), ["forge.coder"]);
        assert_eq!(names(&standalone), ["coder", "gone.coder", "forge.lint"]);
    }

    #[test]
    fn cyclic_profile_brand_falls_back_to_the_raw_agent() {
        let profiles = rimz::config::ProfilesConfig(
            [
                ("planner".to_owned(), profile("reviewer")),
                ("reviewer".to_owned(), profile("planner")),
            ]
            .into(),
        );

        assert_eq!(provider_brand_kind("reviewer", &profiles), "reviewer");
    }

    fn profile(agent: &str) -> rimz::config::Profile {
        rimz::config::Profile {
            allowed_tools: None,
            definition_renders: None,
            model_tier: None,
            tier_stamp: None,
            isolation: None,
            auto_compact: None,
            agent: agent.to_owned(),
            description: None,
            subagents: None,
            model_reminder: None,
            keep_warm: None,
            mode: None,
            model: None,
            effort: None,
            budget: None,
            system_prompt_file: None,
            append_system_prompt_files: Vec::new(),
            skills: None,
            args: None,
        }
    }
}
