//! Scene-specific launch corrections from the resolver's candidate facts.

use std::path::Path;

use rimz::config::{ProfilesConfig, TeamsConfig};
use rimz::harness::plan::ResolveLaunchError;
use rimz::harness::spec::{LayoutErr, SpecKind, UnknownSpec, team_summary};
use unicode_width::UnicodeWidthStr;

use crate::cli::usage::{UsageError, scene_verbs, shell_command};

pub(in crate::cli) fn agents_error(error: anyhow::Error, bare: bool) -> anyhow::Error {
    let Some(unknown) = unknown_spec(&error) else {
        return error;
    };
    let argv = super::invocation_args();
    let message = agents_message(unknown, bare, &argv, &rimz::disk::paths::agents_home());
    if bare {
        UsageError::new(message).into()
    } else {
        anyhow::anyhow!(message)
    }
}

pub(in crate::cli) fn teams_suggestions(
    word: &str,
    teams: &TeamsConfig,
    profiles: &ProfilesConfig,
    include_verbs: bool,
) -> Option<String> {
    let mut candidates = verb_suggestions("teams", include_verbs);
    candidates.extend(teams.0.iter().map(|(name, team)| Suggestion {
        name: name.clone(),
        label: "team",
        summary: team_summary(team, profiles),
    }));
    let closest = closest(word, &candidates);
    if closest.is_empty() {
        return None;
    }
    let argv = super::invocation_args();
    Some(suggestion_block("teams", word, &closest, &argv, true))
}

fn unknown_spec(error: &anyhow::Error) -> Option<&UnknownSpec> {
    error.chain().find_map(|cause| {
        let layout = cause.downcast_ref::<LayoutErr>().or_else(|| {
            match cause.downcast_ref::<ResolveLaunchError>() {
                Some(ResolveLaunchError::Layout(layout)) => Some(layout),
                _ => None,
            }
        })?;
        match layout {
            LayoutErr::UnknownSpec(unknown) => Some(unknown.as_ref()),
            _ => None,
        }
    })
}

fn agents_message(
    unknown: &UnknownSpec,
    bare: bool,
    argv: &[String],
    agents_home: &Path,
) -> String {
    let mut candidates = verb_suggestions("agents", bare);
    candidates.extend(unknown.candidates.iter().map(|candidate| {
        let (label, summary) = match &candidate.kind {
            SpecKind::Profile(profile) => (
                "profile",
                Some(crate::cli::profile_report::profile_summary(
                    &profile.agent,
                    profile.model.as_deref(),
                    profile.model_tier.as_ref().map(|tier| tier.tier),
                    profile.model_tier.as_ref().map(|tier| tier.fell_back),
                    profile.effort.as_deref(),
                    profile.fast == Some(true),
                )),
            ),
            SpecKind::Command => ("command", None),
            SpecKind::Team(summary) => ("team", summary.clone()),
            SpecKind::Kind => ("kind", None),
            SpecKind::Cell => ("cell", None),
            SpecKind::Role => ("role", None),
        };
        Suggestion {
            name: candidate.name.clone(),
            label,
            summary,
        }
    }));
    let closest = closest(&unknown.word, &candidates);
    let verb = if closest.iter().any(|candidate| candidate.label == "verb") {
        "verb, "
    } else {
        ""
    };
    let heading = format!(
        "no {verb}team, profile, or command named `{}`",
        unknown.word
    );
    if !closest.is_empty() {
        return format!(
            "{heading}\n{}",
            suggestion_block("agents", &unknown.word, &closest, argv, false)
        );
    }
    let team = super::home_relative_path(
        &agents_home
            .join("teams")
            .join(format!("{}.md", unknown.word)),
    );
    let profile = super::home_relative_path(
        &agents_home
            .join("agents")
            .join(format!("{}.md", unknown.word)),
    );
    format!(
        "{heading}\n{}\ndetails:      rimz agents profiles · rimz teams list\nnew team:     {team}\nnew profile:  {profile}",
        unknown.choices()
    )
}

struct Suggestion {
    name: String,
    label: &'static str,
    summary: Option<String>,
}

fn verb_suggestions(scene: &str, include: bool) -> Vec<Suggestion> {
    if !include {
        return Vec::new();
    }
    let mut verbs = scene_verbs(scene);
    verbs.sort();
    verbs
        .into_iter()
        .map(|name| Suggestion {
            name,
            label: "verb",
            summary: None,
        })
        .collect()
}

fn closest<'a>(word: &str, candidates: &'a [Suggestion]) -> Vec<&'a Suggestion> {
    let mut seen = std::collections::BTreeSet::new();
    rimz::utils::suggest::nearest_names(
        word,
        candidates
            .iter()
            .map(|candidate| candidate.name.as_str())
            .filter(|name| seen.insert(*name)),
    )
    .into_iter()
    .filter_map(|name| candidates.iter().find(|candidate| candidate.name == name))
    .collect()
}

fn suggestion_block(
    scene: &str,
    word: &str,
    closest: &[&Suggestion],
    argv: &[String],
    single_inline: bool,
) -> String {
    let commands = closest
        .iter()
        .map(|candidate| corrected_command(scene, word, &candidate.name, argv))
        .collect::<Vec<_>>();
    if closest.len() == 1 && (single_inline || closest[0].label == "verb") {
        return format!("did you mean: {}", commands[0]);
    }
    let width = commands
        .iter()
        .map(|command| command.width())
        .max()
        .unwrap_or(0);
    let mut lines = vec!["did you mean:".to_owned()];
    for (candidate, command) in closest.iter().zip(commands.iter()) {
        let summary = candidate
            .summary
            .as_ref()
            .map_or_else(String::new, |summary| format!(" · {summary}"));
        lines.push(format!(
            "  {command}{}{}{summary}",
            " ".repeat(width - command.width() + 5),
            candidate.label
        ));
    }
    lines.join("\n")
}

fn corrected_command(scene: &str, word: &str, name: &str, argv: &[String]) -> String {
    let Some((index, _)) = argv
        .iter()
        .enumerate()
        .skip(1)
        .skip_while(|(_, arg)| arg.as_str() != scene)
        .skip(1)
        .find(|(_, arg)| {
            arg.as_str() == word
                || arg
                    .strip_prefix(word)
                    .is_some_and(|suffix| suffix.starts_with('#'))
        })
    else {
        return shell_command(["rimz", scene, name]);
    };
    let mut corrected = argv.to_vec();
    corrected[index] = format!("{name}{}", &argv[index][word.len()..]);
    shell_command(corrected.iter().map(String::as_str))
}

#[cfg(test)]
mod tests;
