use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;

use anyhow::{Result, bail};
use clap::{Args, Subcommand};
use rimz::agents::skill_links::{self, Desired};
use rimz::agents::skills::SkillChain;
use rimz::config::Isolation;
use rimz::config::definitions::SkillCheck;
use rimz::config::definitions::inventory::{ItemMatchErr, PackInventory, PackView};
use rimz::disk::paths;
use rimz::packs::{
    self, ExclusionOutcome, ExclusionTarget, InstallSource, InstallStatus, ItemFate, ItemType,
    PackErr, PackName, Problem,
};

use super::render::{self, palette, status::StateRole};
use super::usage::{UsageError, shell_command};

const TYPES: [ItemType; 5] = [
    ItemType::Agents,
    ItemType::Subagents,
    ItemType::Teams,
    ItemType::Traits,
    ItemType::Skills,
];

#[derive(Debug, Default, Args)]
struct ListArgs {
    /// Emit installed packs, counts, exclusions, and problems as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub(super) struct PacksArgs {
    #[command(flatten)]
    listing: ListArgs,
    #[command(subcommand)]
    command: Option<PacksSubcmd>,
}

#[derive(Debug, Subcommand)]
enum PacksSubcmd {
    /// List installed packs and load problems.
    List(ListArgs),
    /// Link every pack in a local checkout; edits load without reinstalling.
    Install {
        /// Local source directory: ., .., /path, ./path, ../path, or ~/path.
        #[arg(value_name = "PATH")]
        source: InstallSource,
    },
    /// Show a pack's items, overrides, exclusions, and failures.
    Show {
        #[arg(value_name = "PACK")]
        pack: PackName,
        /// Emit the pack and each item's fate as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Stop loading a pack and prune its host skill links; keep the linked checkout.
    Remove {
        #[arg(value_name = "PACK")]
        pack: PackName,
    },
    /// Stop loading one item or a whole type from a pack.
    Exclude(Selection),
    /// Restore an excluded item or type.
    Include(Selection),
}

#[derive(Debug, Args)]
struct Selection {
    #[arg(value_name = "PACK[:NAME]")]
    target: String,
    /// Item type: agents, subagents, teams, traits, or skills.
    #[arg(long = "type", value_name = "TYPE", value_parser = parse_type)]
    kind: Option<ItemType>,
}

fn parse_type(value: &str) -> std::result::Result<ItemType, String> {
    TYPES
        .into_iter()
        .find(|kind| kind.as_str() == value)
        .ok_or_else(|| "use agents, subagents, teams, traits, or skills".to_owned())
}

pub(super) fn run(args: PacksArgs) -> Result<()> {
    let home = paths::agents_home();
    let mut receipt = Vec::new();
    let mut warnings = Vec::new();
    match args.command.unwrap_or(PacksSubcmd::List(args.listing)) {
        PacksSubcmd::List(args) => return list(&load_inventory(&home), args.json),
        PacksSubcmd::Show { pack, json } => {
            let inventory = load_inventory(&home);
            let pack = inventory.pack(&pack)?;
            return if json {
                render::json_pretty(pack)
            } else {
                show(pack)
            };
        }
        PacksSubcmd::Install {
            source: InstallSource::Linked(source),
        } => install(&home, &source, &mut receipt)?,
        PacksSubcmd::Remove { pack } => {
            let outcome = packs::remove(&home, &pack)?;
            writeln!(receipt, "Removed {} from {}.", outcome.pack, outcome.source)?;
            if let Some(warning) = outcome.cleanup_warning {
                warnings.push(warning);
            }
        }
        PacksSubcmd::Exclude(selection) => change(&home, selection, false, &mut receipt)?,
        PacksSubcmd::Include(selection) => change(&home, selection, true, &mut receipt)?,
    }
    let inventory = load_inventory(&home);
    let cleanup = prune(&home, &mut receipt);
    let write = render::out().write_all(&receipt);
    for problem in warnings
        .iter()
        .chain(&inventory.problems)
        .chain(&inventory.warnings)
    {
        warn(problem)?;
    }
    cleanup?;
    render::finish(write)
}

fn install(home: &Path, source: &Path, out: &mut impl Write) -> Result<()> {
    let outcome = packs::install(home, source).map_err(|error| {
        let manifest_error = matches!(&error, PackErr::Invalid(problem)
            if problem.path.ends_with("rimz-packs.toml"));
        let error = anyhow::Error::new(error);
        if manifest_error {
            error.context(
                "manifest shape: [[pack]] with name = 'crew', description = 'My crew', path = '.'",
            )
        } else {
            error
        }
    })?;
    let action = match outcome.status {
        InstallStatus::Installed => "Installed",
        InstallStatus::Refreshed => "Refreshed",
        InstallStatus::UpToDate => "Already up to date:",
    };
    for pack in &outcome.packs {
        writeln!(out, "{action} {pack} from {} (linked).", source.display())?;
        writeln!(out, "  Next: rimz packs show {pack}")?;
    }
    if outcome.status == InstallStatus::Refreshed {
        for (label, names) in [("added", &outcome.added), ("dropped", &outcome.dropped)] {
            if !names.is_empty() {
                writeln!(
                    out,
                    "{label}: {}",
                    names
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                )?;
            }
        }
    }
    Ok(())
}

fn change(home: &Path, selection: Selection, include: bool, out: &mut impl Write) -> Result<()> {
    let (name, item) = selection
        .target
        .split_once(':')
        .map_or((selection.target.as_str(), None), |(pack, item)| {
            (pack, Some(item))
        });
    let name: PackName = name
        .parse()
        .map_err(|error: PackErr| UsageError::new(error.to_string()))?;
    if item.is_some_and(str::is_empty) {
        return Err(
            UsageError::new("use <pack>:<name> with a non-empty item name".to_owned()).into(),
        );
    }
    let before = load_inventory(home);
    let pack = before.pack(&name)?;
    let target = match item {
        Some(item) => pack.match_item(item, selection.kind, include).map_err(item_match_error)?,
        None => ExclusionTarget::Type(selection.kind.ok_or_else(|| UsageError::new(format!(
            "name an item as {name}:<name> or a whole --type; to remove the pack, run `rimz packs remove {name}`"
        )))?),
    };
    let outcome = if include {
        packs::include(home, &name, &target)?
    } else {
        packs::exclude(home, &name, &target)?
    };
    let label = match &target {
        ExclusionTarget::Type(kind) => format!("{name} --type {}", kind.as_str()),
        ExclusionTarget::Item(item) => shell_command([
            &format!("{name}:{}", item.name),
            "--type",
            item.kind.as_str(),
        ]),
    };
    match outcome {
        ExclusionOutcome::AlreadyExcluded => writeln!(out, "{label} is already excluded.")?,
        ExclusionOutcome::AlreadyIncluded => writeln!(out, "{label} is already included.")?,
        ExclusionOutcome::Changed => writeln!(
            out,
            "{} {label}.",
            if include { "Included" } else { "Excluded" }
        )?,
    }
    let after = load_inventory(home);
    for item in &after.pack(&name)?.items {
        let selected = match &target {
            ExclusionTarget::Type(kind) => *kind == item.kind,
            ExclusionTarget::Item(key) => key.kind == item.kind && key.name == item.name,
        };
        if selected {
            writeln!(
                out,
                "  {}/{}: {}",
                item.kind.as_str(),
                item.name,
                fate_label(&item.fate, &name)
            )?;
        }
    }
    let old = before.conflicts();
    let new = after.conflicts();
    for message in old.difference(&new) {
        writeln!(out, "conflict resolved: {message}")?;
    }
    for message in new.difference(&old) {
        writeln!(out, "conflict opened: {message}")?;
    }
    Ok(())
}

fn item_match_error(error: ItemMatchErr) -> anyhow::Error {
    let ItemMatchErr::Ambiguous { pack, name, kinds } = &error else {
        return error.into();
    };
    UsageError::new(format!(
        "{error}; choose one:\n{}",
        kinds
            .iter()
            .map(|kind| format!(
                "  {}",
                shell_command([&format!("{pack}:{name}"), "--type", kind.as_str()])
            ))
            .collect::<Vec<_>>()
            .join("\n")
    ))
    .into()
}

fn prune(home: &Path, out: &mut impl Write) -> Result<()> {
    let chain = SkillChain::from_home(home);
    let (logins, config_error) =
        rimz::agents::provider_home_logins(&super::machine_config().accounts);
    let roots: BTreeSet<_> = logins
        .iter()
        .filter_map(|(_, adapter, env)| adapter.skills_home(env))
        .collect();
    let mut errors = Vec::new();
    for root in roots {
        let result = skill_links::plan(&root, &chain, Desired::Prune)
            .and_then(|plan| skill_links::apply(&plan));
        match result {
            Ok(outcome) if !outcome.unlinked_names.is_empty() => writeln!(
                out,
                "Skill links removed from {}: {}",
                root.display(),
                outcome.unlinked_names.join(", ")
            )?,
            Ok(_) => {}
            Err(error) => errors.push(error.to_string()),
        }
    }
    if let Some(error) = config_error {
        errors.push(format!("{error}; only native provider homes were pruned"));
    }
    if !errors.is_empty() {
        bail!(
            "pack change committed, but host skill link cleanup is incomplete:\n{}",
            errors.join("\n")
        );
    }
    Ok(())
}

fn load_inventory(home: &Path) -> PackInventory {
    let machine = super::machine_config();
    let env = rimz::agents::ambient_env();
    let check = if Isolation::ambient(&env).is_some_and(Isolation::is_scoped) {
        SkillCheck::Skip
    } else {
        SkillCheck::Check {
            env: &env,
            machine_isolation: machine.agents.isolation,
        }
    };
    PackInventory::load(
        home,
        check,
        &machine.agents.commands,
        &rimz::config::MachineConfig::config_path(),
        &machine.tiers,
        machine.harness.auto_compact,
    )
}

fn fate_label(fate: &ItemFate, pack: &PackName) -> String {
    match fate {
        ItemFate::Loads => "loads".to_owned(),
        ItemFate::Overridden { path } => format!("overridden by {}", path.display()),
        ItemFate::Excluded => "excluded".to_owned(),
        ItemFate::ExcludedType => "excluded (type)".to_owned(),
        ItemFate::Failed { message } => format!("failed: {message}"),
        ItemFate::Conflict { claims, .. } => format!(
            "conflicts with {}",
            claims
                .iter()
                .filter(
                    |claim| claim.pack != *pack || claims.iter().all(|claim| claim.pack == *pack)
                )
                .map(|claim| format!("{} ({})", claim.pack, claim.path.display()))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn fate_role(fate: &ItemFate) -> StateRole {
    match fate {
        ItemFate::Loads => StateRole::Success,
        ItemFate::Overridden { .. } | ItemFate::Excluded | ItemFate::ExcludedType => {
            StateRole::Neutral
        }
        ItemFate::Conflict { .. } | ItemFate::Failed { .. } => StateRole::Failed,
    }
}

fn list(inventory: &PackInventory, json: bool) -> Result<()> {
    if json {
        let packs: Vec<_> = inventory
            .packs
            .iter()
            .map(|pack| {
                serde_json::json!({
                    "name": pack.name, "description": pack.description, "source": pack.source,
                    "linked": pack.linked, "entry": pack.entry, "excluded": pack.excluded,
                    "counts": pack.counts, "problems": pack.problems,
                })
            })
            .collect();
        return render::json_pretty(
            &serde_json::json!({"packs": packs, "problems": inventory.problems, "warnings": inventory.warnings}),
        );
    }
    let mut out = render::out();
    if inventory.packs.is_empty() && inventory.problems.is_empty() {
        writeln!(
            out,
            "No packs installed. Install a local source: rimz packs install ./path"
        )?;
    } else if !inventory.packs.is_empty() {
        let mut table = render::Table::new(["PACK", "LOADED", "SOURCE", "PROBLEMS"]).right(&[1]);
        for pack in &inventory.packs {
            table.row([
                render::cell(pack.name.to_string()).fg(palette::accent()),
                render::cell(pack.counts.values().sum::<usize>().to_string()),
                render::cell(format!(
                    "{}{}",
                    pack.source,
                    if pack.linked { " (linked)" } else { "" }
                )),
                render::cell(if pack.problems.is_empty() {
                    "-".to_owned()
                } else {
                    pack.problems.len().to_string()
                })
                .dash(),
            ]);
        }
        table.render(&mut out)?;
    }
    write_problems(&mut out, &inventory.problems)?;
    for warning in &inventory.warnings {
        warn(warning)?;
    }
    Ok(())
}

fn show(pack: &PackView) -> Result<()> {
    let mut out = render::out();
    writeln!(
        out,
        "{}",
        render::paint(palette::header(), pack.name.as_str())
    )?;
    if let Some(description) = &pack.description {
        writeln!(out, "{description}")?;
    }
    writeln!(
        out,
        "Source: {}{}",
        pack.source,
        if pack.linked { " (linked)" } else { "" }
    )?;
    for kind in TYPES {
        let items: Vec<_> = pack.items.iter().filter(|item| item.kind == kind).collect();
        if items.is_empty() {
            continue;
        }
        writeln!(out, "\n{}", render::paint(palette::header(), kind.as_str()))?;
        for item in items {
            writeln!(
                out,
                "  {}: {}",
                item.name,
                render::paint(
                    render::status::role(fate_role(&item.fate)),
                    &fate_label(&item.fate, &pack.name)
                )
            )?;
        }
    }
    if !pack.excluded.types.is_empty() {
        writeln!(
            out,
            "Excluded types: {}",
            pack.excluded
                .types
                .iter()
                .map(|kind| kind.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )?;
    }
    for item in &pack.unshipped_exclusions {
        writeln!(
            out,
            "{}/{}: excluded (no longer shipped)",
            item.kind.as_str(),
            item.name
        )?;
    }
    write_problems(&mut out, &pack.problems)?;
    Ok(())
}

fn write_problems(out: &mut impl Write, problems: &[Problem]) -> Result<()> {
    for problem in problems {
        writeln!(
            out,
            "{} {problem}",
            render::paint(render::status::role(StateRole::Failed), "problem:")
        )?;
    }
    Ok(())
}

fn warn(problem: &Problem) -> Result<()> {
    writeln!(
        render::err(),
        "rimz: {} {problem}",
        render::paint(palette::warn(), "warning:")
    )?;
    Ok(())
}
