use std::collections::BTreeSet;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use clap::{Args, Subcommand};
use rimz::agents::skill_links::{self, Desired};
use rimz::agents::skills::SkillChain;
use rimz::config::Isolation;
use rimz::config::definitions::SkillCheck;
use rimz::config::definitions::customization::{
    self, OverrideDiff, OverrideErr, PackOverride, ResetOutcome,
};
use rimz::config::definitions::inventory::{ItemMatchErr, PackInventory, PackView};
use rimz::config::definitions::review::{self, PackPreview};
use rimz::disk::paths;
use rimz::packs::transaction::{PackChange, RetiredCheckouts, Transaction};
use rimz::packs::{
    self, ExclusionOutcome, ExclusionTarget, GitSource, InstallSource, InstallStatus, ItemFate,
    ItemType, PackErr, PackName, Problem,
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
    /// Install Git packs with a review, or link a local checkout for live edits.
    Install {
        /// crew, skills, github:owner/repo, a Git URL, or an explicit local path.
        #[arg(value_name = "SOURCE")]
        source: InstallSource,
        /// Skip the Git install question (required without a terminal).
        #[arg(long)]
        yes: bool,
    },
    /// Update unpinned Git sources, holding changes that widen profile grants.
    Update {
        /// Update the source containing this pack, or all installed sources.
        #[arg(value_name = "PACK")]
        pack: Option<PackName>,
        /// Apply widening updates without asking.
        #[arg(long)]
        yes: bool,
    },
    /// Show a pack's items, overrides, exclusions, and failures.
    Show {
        #[arg(value_name = "PACK")]
        pack: PackName,
        /// Emit the pack and each item's fate as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Stop loading a pack and prune its host skill links; unlink its final entry.
    Remove {
        #[arg(value_name = "PACK")]
        pack: PackName,
    },
    /// Stop loading one item or a whole type from a pack.
    Exclude(Selection),
    /// Restore an excluded item or type.
    Include(Selection),
    /// Create a home override of a shipped item.
    Customize {
        #[command(flatten)]
        selection: ItemSelection,
        /// Copy the shipped item instead of inheriting it.
        #[arg(long)]
        copy: bool,
    },
    /// Compare a home override with its shipped item.
    Diff(ItemSelection),
    /// Remove a home override and load the shipped item again.
    Reset {
        #[command(flatten)]
        selection: Selection,
        /// Remove only byte-identical overrides; accepts a whole pack.
        #[arg(long)]
        identical: bool,
    },
}

#[derive(Debug, Args)]
struct ItemSelection {
    #[arg(value_name = "PACK:NAME")]
    target: String,
    /// Item type: agents, subagents, teams, traits, or skills.
    #[arg(long = "type", value_name = "TYPE", value_parser = parse_type)]
    kind: Option<ItemType>,
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

fn editor_command(
    visual: Option<&str>,
    editor: Option<&str>,
    path: &Path,
) -> Result<Option<Vec<std::ffi::OsString>>> {
    let Some(editor) = visual
        .filter(|value| !value.trim().is_empty())
        .or_else(|| editor.filter(|value| !value.trim().is_empty()))
    else {
        return Ok(None);
    };
    let words = shlex::split(editor)
        .ok_or_else(|| anyhow::anyhow!("editor contains invalid shell quoting"))?;
    if words.is_empty() {
        bail!("editor command is empty");
    }
    let mut words: Vec<std::ffi::OsString> = words.into_iter().map(Into::into).collect();
    words.push(path.as_os_str().to_owned());
    Ok(Some(words))
}

pub(super) fn run(args: PacksArgs) -> Result<()> {
    let home = paths::agents_home();
    let mut receipt = Vec::new();
    let mut edit_path = None;
    let mut warnings = Vec::new();
    let mut failures = Vec::new();
    let mut failure_heading = "Pack updates incomplete";
    let mut retired = Vec::new();
    let mut installed = Vec::new();
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
            yes: _,
        } => installed = install(&home, &source, &mut receipt, &mut warnings)?,
        PacksSubcmd::Install {
            source: InstallSource::Git(source),
            yes,
        } => {
            installed = match install_git(&home, &source, yes, &mut receipt)? {
                Some(names) => names,
                None => return Ok(()),
            };
        }
        PacksSubcmd::Update { pack, yes } => {
            failures = update(&home, pack.as_ref(), yes, &mut receipt, &mut retired)?;
        }
        PacksSubcmd::Remove { pack } => {
            let outcome = packs::remove(&home, &pack)?;
            writeln!(receipt, "Removed {} from {}.", outcome.pack, outcome.source)?;
            if let Some(warning) = outcome.cleanup_warning {
                warnings.push(warning);
            }
        }
        PacksSubcmd::Exclude(selection) => change(&home, selection, false, &mut receipt)?,
        PacksSubcmd::Include(selection) => change(&home, selection, true, &mut receipt)?,
        PacksSubcmd::Customize { selection, copy } => {
            edit_path = Some(customize(&home, selection, copy, &mut receipt)?);
        }
        PacksSubcmd::Diff(selection) => return diff(&home, selection),
        PacksSubcmd::Reset {
            selection,
            identical,
        } => {
            failures = reset(&home, selection, identical, &mut receipt)?;
            failure_heading = "Pack reset incomplete";
        }
    }
    let inventory = load_inventory(&home);
    for pack in inventory
        .packs
        .iter()
        .filter(|pack| installed.contains(&pack.name))
    {
        let name = &pack.name;
        let count = pack
            .items
            .iter()
            .filter(|item| {
                matches!(item.fate, ItemFate::Overridden { .. })
                    && PackOverride::new(&home, item)
                        .identical()
                        .is_ok_and(|same| same)
            })
            .count();
        if count > 0 {
            writeln!(
                receipt,
                "{name}: {count} identical override{}; remove with `rimz packs reset {name} --identical`.",
                if count == 1 { "" } else { "s" }
            )?;
        }
    }
    let cleanup = prune(&home, &mut receipt);
    let write = render::out().write_all(&receipt);
    let mut seen = BTreeSet::new();
    for problem in warnings
        .iter()
        .chain(&inventory.problems)
        .chain(&inventory.warnings)
    {
        let path = std::fs::canonicalize(&problem.path).unwrap_or_else(|_| problem.path.clone());
        if seen.insert((path, &problem.message)) {
            warn(problem)?;
        }
    }
    if let Err(error) = cleanup {
        if failures.is_empty() {
            return Err(error);
        }
        failures.push(error.to_string());
    }
    render::finish(write)?;
    if !failures.is_empty() {
        bail!("{failure_heading}:\n{}", failures.join("\n"));
    }
    if let Some(path) = edit_path {
        edit(&path)?;
    }
    Ok(())
}

fn install(
    home: &Path,
    source: &Path,
    out: &mut impl Write,
    warnings: &mut Vec<Problem>,
) -> Result<Vec<PackName>> {
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
    warnings.extend(outcome.warnings);
    Ok(outcome.packs)
}

fn install_git(
    home: &Path,
    source: &GitSource,
    yes: bool,
    out: &mut impl Write,
) -> Result<Option<Vec<PackName>>> {
    let transaction = Transaction::stage(home, source)?;
    let machine = super::machine_config();
    let env = rimz::agents::ambient_env();
    let check = skill_check(&env, machine.agents.isolation);
    let candidate = transaction.candidate()?;
    let preview = review::compare(home, check, &machine, &transaction.current()?, &candidate);
    let changes = transaction.changes();
    let mut stdout = render::out();
    writeln!(stdout, "Pack install review")?;
    summary(&mut stdout, &changes, &preview, Some("named"))?;
    stdout.flush()?;
    let unchanged = transaction.up_to_date();
    if !unchanged {
        match decision(yes, "Install these packs?")? {
            Decision::Apply => {}
            Decision::Decline => {
                writeln!(stdout, "Install aborted; nothing changed.")?;
                return Ok(None);
            }
            Decision::NoTerminal => bail!(
                "pass --yes to confirm this Git pack install without a terminal; nothing changed"
            ),
        }
    }
    let names: Vec<_> = if unchanged {
        candidate
            .record
            .sources
            .iter()
            .filter(|installed| installed.source == source.source)
            .flat_map(|source| &source.packs)
            .map(|pack| pack.name.clone())
            .collect()
    } else {
        changes.iter().map(|change| change.name.clone()).collect()
    };
    transaction.commit()?;
    for name in &names {
        writeln!(
            out,
            "{} {name}.",
            if unchanged {
                "Already up to date:"
            } else {
                "Installed"
            }
        )?;
        writeln!(out, "  Next: rimz packs show {name}")?;
    }
    Ok(Some(names))
}

fn update(
    home: &Path,
    pack: Option<&PackName>,
    yes: bool,
    out: &mut impl Write,
    retired: &mut Vec<RetiredCheckouts>,
) -> Result<Vec<String>> {
    let record = packs::Record::read(home)?;
    let sources = if let Some(name) = pack {
        vec![
            record
                .sources
                .iter()
                .find(|source| source.packs.iter().any(|pack| pack.name == *name))
                .ok_or_else(|| PackErr::UnknownPack {
                    name: name.clone(),
                    installed: record
                        .sources
                        .iter()
                        .flat_map(|source| &source.packs)
                        .map(|pack| pack.name.to_string())
                        .collect::<Vec<_>>()
                        .join(", "),
                })?,
        ]
    } else {
        record.sources.iter().collect()
    };
    if sources.is_empty() {
        writeln!(
            out,
            "No installed packs. Install crew with: rimz packs install crew"
        )?;
    }
    let mut failures = Vec::new();
    for source in sources {
        if let Err(error) = update_source(home, &source.source, yes, out, retired) {
            let names = source
                .packs
                .iter()
                .map(|pack| pack.name.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            failures.push(format!("{names}: {error:#}"));
        }
    }
    Ok(failures)
}

fn update_source(
    home: &Path,
    url: &str,
    yes: bool,
    out: &mut impl Write,
    retired: &mut Vec<RetiredCheckouts>,
) -> Result<()> {
    let record = packs::Record::read(home)?;
    let source = record
        .sources
        .iter()
        .find(|source| source.source == url)
        .ok_or(PackErr::RecordChanged)?;
    let names = source
        .packs
        .iter()
        .map(|pack| pack.name.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    if source.linked {
        writeln!(out, "Skipped {names}: linked edits show at once.")?;
        return Ok(());
    }
    if source.pinned {
        let spelling = source.spelling();
        writeln!(
            out,
            "Skipped {names}: pinned at {}; follow updates again with `{}`.",
            source.reference.as_deref().unwrap_or("<unknown>"),
            shell_command(["rimz", "packs", "install", &spelling])
        )?;
        return Ok(());
    }
    let transaction = Transaction::stage_update(home, source)?;
    if transaction.up_to_date() {
        retired.push(transaction.commit()?);
        writeln!(out, "Already up to date: {names}.")?;
        return Ok(());
    }
    let machine = super::machine_config();
    let env = rimz::agents::ambient_env();
    let check = skill_check(&env, machine.agents.isolation);
    let preview = review::compare(
        home,
        check,
        &machine,
        &transaction.current()?,
        &transaction.candidate()?,
    );
    let changes = transaction.changes();
    let mut stdout = render::out();
    writeln!(stdout, "Pack update review")?;
    summary(&mut stdout, &changes, &preview, None)?;
    stdout.flush()?;
    if preview
        .profiles
        .iter()
        .any(|profile| !profile.widened.is_empty())
    {
        match decision(yes, "Apply this widening update?")? {
            Decision::Apply => {}
            answer => {
                let fields = preview
                    .profiles
                    .iter()
                    .filter(|profile| !profile.widened.is_empty())
                    .map(|profile| format!("{}: {}", profile.name, profile.widened.join(", ")))
                    .collect::<Vec<_>>()
                    .join("; ");
                writeln!(stdout, "Held {names}: {fields}.")?;
                let command = shell_command(
                    ["rimz", "packs", "update"]
                        .into_iter()
                        .chain(source.packs.first().map(|pack| pack.name.as_str())),
                );
                writeln!(stdout, "Review again: {command}")?;
                if matches!(answer, Decision::NoTerminal) {
                    writeln!(
                        stdout,
                        "Pass --yes to apply this widening update without a terminal."
                    )?;
                    bail!("widening update held; pass --yes to apply without a terminal");
                }
                return Ok(());
            }
        }
    }
    let names = changes
        .iter()
        .map(|change| change.name.to_string())
        .collect::<Vec<_>>();
    retired.push(transaction.commit()?);
    for name in names {
        writeln!(out, "Updated {name}.")?;
    }
    Ok(())
}

enum Decision {
    Apply,
    Decline,
    NoTerminal,
}

fn decision(yes: bool, prompt: &str) -> Result<Decision> {
    if yes {
        return Ok(Decision::Apply);
    }
    if !std::io::stdin().is_terminal() {
        return Ok(Decision::NoTerminal);
    }
    Ok(if super::confirm(prompt)? {
        Decision::Apply
    } else {
        Decision::Decline
    })
}

fn summary(
    out: &mut impl Write,
    changes: &[PackChange<'_>],
    preview: &PackPreview,
    named: Option<&str>,
) -> Result<()> {
    if changes.is_empty() {
        writeln!(out, "No changed packs.")?;
    }
    for change in changes {
        let source = change.source;
        let label = source_label(
            &source.source,
            source.url.as_deref(),
            source.linked,
            source.reference.as_deref(),
            source.commit.as_deref(),
            source.pinned,
        );
        let reason = change
            .required_by
            .map(|pack| format!("required by {pack}"))
            .or_else(|| named.map(str::to_owned));
        write!(out, "\n{}: {label}", change.name)?;
        if let Some(reason) = reason {
            write!(out, " ({reason})")?;
        }
        writeln!(out)?;
        if let Some(old) = change.previous {
            writeln!(
                out,
                "  from {} -> {label}",
                source_label(
                    &old.source,
                    old.url.as_deref(),
                    old.linked,
                    old.reference.as_deref(),
                    old.commit.as_deref(),
                    old.pinned
                )
            )?;
        }
        if source.packs.len() > 1 {
            writeln!(out, "  All packs in this source share its ref and commit.")?;
        }
        let pack = preview.inventory.pack(change.name)?;
        for kind in TYPES {
            for item in pack.items.iter().filter(|item| item.kind == kind) {
                writeln!(
                    out,
                    "  {}/{}: {}",
                    kind.as_str(),
                    item.name,
                    fate_label(&item.fate, &pack.name)
                )?;
            }
        }
        write_problems(out, &pack.problems)?;
    }
    writeln!(out, "\nProfiles")?;
    if preview.profiles.is_empty() {
        writeln!(out, "No changed profiles.")?;
    }
    for profile in &preview.profiles {
        writeln!(out, "{} ({})", profile.name, profile.namespace)?;
        let grants = &profile.grants;
        let tools = grants.tools.as_ref().map_or_else(
            || "all".to_owned(),
            |tools| {
                let names: Vec<_> = tools
                    .iter()
                    .map(|(base, types)| {
                        if base == "Agent" && !types.is_empty() {
                            format!(
                                "Agent({})",
                                types.iter().cloned().collect::<Vec<_>>().join(",")
                            )
                        } else {
                            base.clone()
                        }
                    })
                    .collect();
                if names.is_empty() {
                    "none".to_owned()
                } else {
                    names.join(", ")
                }
            },
        );
        let allowed = if grants.allowed_tools.is_empty() {
            "none".to_owned()
        } else {
            grants
                .allowed_tools
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        };
        for (field, value) in [
            ("kind", grants.kind.to_string()),
            ("mode", grants.mode.to_string()),
            ("isolation", grants.isolation.to_string()),
            ("tools", tools),
            ("allowed-tools", allowed),
        ] {
            let mark = if profile.widened.contains(&field) {
                render::paint(palette::warn(), " [widened]")
            } else {
                String::new()
            };
            writeln!(out, "  {field}: {value}{mark}")?;
        }
    }
    write_problems(out, &preview.inventory.problems)?;
    for warning in &preview.inventory.warnings {
        writeln!(out, "warning: {warning}")?;
    }
    Ok(())
}

fn source_label(
    source: &str,
    url: Option<&str>,
    linked: bool,
    reference: Option<&str>,
    commit: Option<&str>,
    pinned: bool,
) -> String {
    if linked {
        return format!("{source} (linked)");
    }
    let source = GitSource {
        source: source.to_owned(),
        url: url.unwrap_or(source).to_owned(),
        pack: None,
        reference: None,
    }
    .spelling();
    let mut label = source;
    if let Some(reference) = reference {
        label.push_str(&format!("@{reference}"));
    }
    if let Some(commit) = commit {
        label.push_str(&format!(" {}", commit.get(..8).unwrap_or(commit)));
    }
    if pinned {
        label.push_str(" pinned");
    }
    label
}

fn parse_selection(target: &str) -> Result<(PackName, Option<&str>)> {
    let (name, item) = target
        .split_once(':')
        .map_or((target, None), |(pack, item)| (pack, Some(item)));
    let name: PackName = name
        .parse()
        .map_err(|error: PackErr| UsageError::new(error.to_string()))?;
    if item.is_some_and(str::is_empty) {
        return Err(
            UsageError::new("use <pack>:<name> with a non-empty item name".to_owned()).into(),
        );
    }
    Ok((name, item))
}

fn selected_item<'a>(
    inventory: &'a PackInventory,
    selection: &ItemSelection,
) -> Result<&'a rimz::packs::PackItem> {
    let (pack, name) = parse_selection(&selection.target)?;
    let name = name.ok_or_else(|| UsageError::new("name an item as <pack>:<name>".to_owned()))?;
    inventory
        .pack(&pack)?
        .item(name, selection.kind)
        .map_err(item_match_error)
}

fn customize(
    home: &Path,
    selection: ItemSelection,
    copy: bool,
    out: &mut impl Write,
) -> Result<PathBuf> {
    let inventory = load_inventory(home);
    let item = selected_item(&inventory, &selection)?;
    let path = PackOverride::new(home, item)
        .customize(copy)
        .map_err(|error| {
            if matches!(error, OverrideErr::CopyOnly { .. }) {
                anyhow::Error::new(UsageError::new(error.to_string()))
            } else {
                error.into()
            }
        })?;
    let key = format!("{}:{}", item.pack, item.name);
    if copy {
        writeln!(
            out,
            "Created {} (copy, no longer follows {}).",
            path.display(),
            item.pack
        )?;
    } else {
        writeln!(out, "Created {} (inherits {key}).", path.display())?;
    }
    writeln!(
        out,
        "Next: {}",
        shell_command(["rimz", "packs", "diff", &key, "--type", item.kind.as_str()])
    )?;
    if item.kind != ItemType::Skills {
        writeln!(out, "Next: rimz agents validate")?;
    }
    Ok(if item.kind == ItemType::Skills {
        path.join("SKILL.md")
    } else {
        path
    })
}

fn edit(path: &Path) -> Result<()> {
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        let visual = std::env::var("VISUAL").ok();
        let editor = std::env::var("EDITOR").ok();
        let result = editor_command(visual.as_deref(), editor.as_deref(), path).and_then(|words| {
            let Some(words) = words else { return Ok(false) };
            let result = std::process::Command::new(&words[0])
                .args(&words[1..])
                .status()?;
            if !result.success() {
                bail!("editor exited with {result}");
            }
            Ok(true)
        });
        match result {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(error) => writeln!(
                render::err(),
                "warning: {error}; override kept at {}",
                path.display()
            )?,
        }
    }
    render::finish(writeln!(render::out(), "Edit {}", path.display()))
}

fn diff(home: &Path, selection: ItemSelection) -> Result<()> {
    let inventory = load_inventory(home);
    let item = selected_item(&inventory, &selection)?;
    let mut out = render::out();
    match PackOverride::new(home, item).diff()? {
        OverrideDiff::None => writeln!(
            out,
            "{}:{} has no override; shipped at {}.",
            item.pack,
            item.name,
            item.path.display()
        )?,
        OverrideDiff::Inherits { text } => {
            writeln!(
                out,
                "Override inherits {}:{}; fields replace shipped settings, body follows the shipped prompt:",
                item.pack, item.name
            )?;
            write!(out, "{text}")?;
        }
        OverrideDiff::Command { path } => writeln!(
            out,
            "Overridden by configured command {} in {}.",
            item.name,
            path.display()
        )?,
        OverrideDiff::Files { path, changed } if changed.is_empty() => writeln!(
            out,
            "{}:{} override at {} matches the shipped item.",
            item.pack,
            item.name,
            path.display()
        )?,
        OverrideDiff::Files { changed, .. } => {
            for file in changed {
                let original = file.shipped.as_deref().map(std::str::from_utf8).transpose();
                let candidate = file.custom.as_deref().map(std::str::from_utf8).transpose();
                match (original, candidate) {
                    (Ok(original), Ok(candidate)) => render::diff::preview_diff(
                        &mut out,
                        original.map(|text| (file.shipped_path.as_path(), text)),
                        candidate.map(|text| (file.path.as_path(), text)),
                    )?,
                    _ => writeln!(out, "Binary files differ: {}", file.path.display())?,
                }
            }
        }
    }
    Ok(())
}

fn reset(
    home: &Path,
    selection: Selection,
    identical: bool,
    out: &mut impl Write,
) -> Result<Vec<String>> {
    let (pack_name, name) = parse_selection(&selection.target)?;
    if name.is_none() && !identical {
        return Err(UsageError::new("a bare reset <pack> requires --identical".to_owned()).into());
    }
    let inventory = load_inventory(home);
    let pack = inventory.pack(&pack_name)?;
    let mut outcomes = if let Some(name) = name {
        let item = pack.item(name, selection.kind).map_err(item_match_error)?;
        match PackOverride::new(home, item).reset(identical)? {
            Some(path) => vec![ResetOutcome::Removed { item, path }],
            None if matches!(item.fate, ItemFate::Overridden { .. }) => {
                vec![ResetOutcome::Kept { item, reason: None }]
            }
            None => Vec::new(),
        }
    } else {
        customization::reset_identical(
            home,
            pack.items
                .iter()
                .filter(|item| selection.kind.is_none_or(|kind| item.kind == kind)),
        )
    };
    outcomes.sort_by_key(|outcome| !matches!(outcome, ResetOutcome::Removed { .. }));
    let after = load_inventory(home);
    let pack = after.packs.iter().find(|pack| pack.name == pack_name);
    if outcomes.is_empty() {
        writeln!(
            out,
            "{} already has no eligible override to reset.",
            selection.target
        )?;
    }
    let mut failures = Vec::new();
    let mut removed = BTreeSet::new();
    for outcome in outcomes {
        match outcome {
            ResetOutcome::Removed { item, path } => {
                if removed.insert(path.clone()) {
                    writeln!(out, "Removed {}.", path.display())?;
                }
                let Some(item) = pack.and_then(|pack| {
                    pack.items.iter().find(|current| {
                        current.kind == item.kind
                            && current.name == item.name
                            && current.path == item.path
                    })
                }) else {
                    continue;
                };
                writeln!(
                    out,
                    "  {}/{}: {}",
                    item.kind.as_str(),
                    item.name,
                    fate_label(&item.fate, &pack_name)
                )?;
            }
            ResetOutcome::Kept { item, reason } => {
                let key = format!("{pack_name}:{}", item.name);
                if let Some(reason) = reason {
                    writeln!(out, "Kept {key} ({}): {reason}", item.kind.as_str())?;
                } else {
                    writeln!(
                        out,
                        "Kept {key} ({}); diff: {}",
                        item.kind.as_str(),
                        shell_command([
                            "rimz",
                            "packs",
                            "diff",
                            &key,
                            "--type",
                            item.kind.as_str()
                        ])
                    )?;
                }
            }
            ResetOutcome::Failed { item, error } => {
                failures.push(format!(
                    "{pack_name}:{} ({}): {error}",
                    item.name,
                    item.kind.as_str()
                ));
            }
        }
    }
    Ok(failures)
}

fn change(home: &Path, selection: Selection, include: bool, out: &mut impl Write) -> Result<()> {
    let (name, item) = parse_selection(&selection.target)?;
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
            "host skill link cleanup is incomplete:\n{}",
            errors.join("\n")
        );
    }
    Ok(())
}

fn load_inventory(home: &Path) -> PackInventory {
    let machine = super::machine_config();
    let env = rimz::agents::ambient_env();
    let check = skill_check(&env, machine.agents.isolation);
    PackInventory::load(
        home,
        check,
        &machine.agents.commands,
        &rimz::config::MachineConfig::config_path(),
        &machine.tiers,
        machine.harness.auto_compact,
    )
}

fn skill_check(
    env: &std::collections::BTreeMap<String, String>,
    machine_isolation: Isolation,
) -> SkillCheck<'_> {
    if Isolation::ambient(env).is_some_and(Isolation::is_scoped) {
        SkillCheck::Skip
    } else {
        SkillCheck::Check {
            env,
            machine_isolation,
        }
    }
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
                let mut row = serde_json::json!({
                    "name": pack.name, "description": pack.description, "source": pack.source,
                    "linked": pack.linked, "entry": pack.entry, "excluded": pack.excluded,
                    "ref": pack.reference, "pinned": pack.pinned, "commit": pack.commit,
                    "counts": pack.counts, "problems": pack.problems,
                });
                if let Some(url) = &pack.url {
                    row["url"] = url.clone().into();
                }
                row
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
            "No packs installed. Install a source: rimz packs install crew"
        )?;
    } else if !inventory.packs.is_empty() {
        let mut table = render::Table::new(["PACK", "LOADED", "SOURCE", "PROBLEMS"]).right(&[1]);
        for pack in &inventory.packs {
            table.row([
                render::cell(pack.name.to_string()).fg(palette::accent()),
                render::cell(pack.counts.values().sum::<usize>().to_string()),
                render::cell(source_label(
                    &pack.source,
                    pack.url.as_deref(),
                    pack.linked,
                    pack.reference.as_deref(),
                    pack.commit.as_deref(),
                    pack.pinned,
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
        "Source: {}",
        source_label(
            &pack.source,
            pack.url.as_deref(),
            pack.linked,
            pack.reference.as_deref(),
            pack.commit.as_deref(),
            pack.pinned
        )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_resolution_prefers_visual_splits_words_and_appends_the_path() {
        let path = Path::new("/some path/coder.md");
        assert_eq!(
            editor_command(Some("code --wait 'two words'"), Some("vi"), path).unwrap(),
            Some(vec![
                "code".into(),
                "--wait".into(),
                "two words".into(),
                "/some path/coder.md".into()
            ])
        );
        assert_eq!(
            editor_command(None, Some("vi -f"), path).unwrap(),
            Some(vec!["vi".into(), "-f".into(), "/some path/coder.md".into()])
        );
        assert!(editor_command(None, None, path).unwrap().is_none());
        assert!(editor_command(Some("'unfinished"), None, path).is_err());
    }
}
