//! Merge the provider skill root, the checkout's project tier for a kind that does not read it,
//! and the RimZ skill chain into a read-only launch view.
//! Unlisted skills that cannot be prepared are omitted and reported.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::agents::ManualSkill;
use crate::agents::skills::{ProjectTier, SkillChain, SkillErr};

use super::{DirEntry, DirEntryKind, DirView, SandboxErr, SkillInputs, SkippedSkill, rewrite};

#[derive(Default)]
pub(super) struct SkillView {
    pub dir: Option<DirView>,
    pub skipped: Vec<SkippedSkill>,
    pub copies: Vec<rewrite::PlannedCopy>,
    /// The folder of project skills the provider reads by itself, and those of them the
    /// launch's policy leaves out, which stay model-callable.
    pub ungoverned: Option<(PathBuf, Vec<String>)>,
}

#[derive(Default)]
struct ProviderEntries {
    entries: BTreeMap<String, DirEntry>,
    owned: BTreeMap<String, PathBuf>,
}

pub(super) fn plan(
    env: &BTreeMap<String, String>,
    skills_dir: &Path,
    checkout: &Path,
    inputs: &SkillInputs<'_>,
) -> Result<SkillView, SandboxErr> {
    match plan_view(env, skills_dir, checkout, inputs) {
        Err(error) if inputs.callable.is_none() && inputs.grant.is_none() => {
            tracing::debug!(%error, "keeping native skill discovery because the optional view is unavailable");
            Ok(SkillView::default())
        }
        result => result,
    }
}

fn plan_view(
    env: &BTreeMap<String, String>,
    skills_dir: &Path,
    checkout: &Path,
    inputs: &SkillInputs<'_>,
) -> Result<SkillView, SandboxErr> {
    let Some(home) = &inputs.home else {
        return if inputs.callable.is_some() {
            Err(SandboxErr::SkillsNeedRoot {
                kind: inputs.kind.to_owned(),
            })
        } else {
            Ok(SkillView::default())
        };
    };
    super::validate_path(home)?;
    if (inputs.callable.is_some() || (!inputs.withheld.is_empty() && inputs.grant.is_none()))
        && inputs.manual == ManualSkill::Unsupported
    {
        return Err(SandboxErr::ManualSkillsUnsupported {
            kind: inputs.kind.to_owned(),
        });
    }
    let chain = SkillChain::from_env(env);
    let ProviderEntries {
        mut entries,
        mut owned,
    } = list_dir(home, chain.as_ref())?;
    let project = crate::agents::find_definition(inputs.kind)
        .map(|adapter| ProjectTier::new(checkout.to_owned(), adapter));
    let mut roots = vec![home.clone()];
    let mut changed = false;
    let mut skipped = Vec::new();
    let mut copies = Vec::new();
    let mut conflicts = BTreeMap::new();
    // A project folder that is the provider root is that root, and no project folder supplies
    // a `rimz-*` name.
    let is_project = |folder: &PathBuf| !crate::agents::skills::same_dir(folder, home);
    let native = project
        .as_ref()
        .and_then(ProjectTier::native_folder)
        .filter(is_project);
    let tier = project.as_ref().map(ProjectTier::root).filter(is_project);
    let mut tier_skills = BTreeMap::new();
    if let Some(tier) = &tier {
        super::validate_path(tier)?;
        roots.push(tier.clone());
        tier_skills = crate::agents::skills::project_skills(tier, |name| {
            !name.starts_with("rimz-") && !entries.contains_key(name)
        })?;
    }
    if let Some(native) = native
        .as_ref()
        .filter(|native| tier.as_ref() != Some(*native))
    {
        roots.push(native.clone());
    }
    // A kind that reads the tier keeps it native, so the view only drops the lower copies the
    // tier shadows; for any other kind the tier joins the view above the chain.
    if native != tier {
        for (name, skill) in &tier_skills {
            changed = true;
            entries.insert(
                name.clone(),
                DirEntry {
                    name: name.clone(),
                    source: skill.source.clone(),
                    kind: DirEntryKind::Bind,
                },
            );
        }
    }
    if let Some(chain) = &chain {
        for root in chain.roots() {
            super::validate_path(&root)?;
            roots.push(root);
        }
        let inventory = chain
            .inventory(|name| !entries.contains_key(name) && !tier_skills.contains_key(name))?;
        conflicts = inventory.conflicts;
        for (name, skill) in inventory.skills {
            if let std::collections::btree_map::Entry::Vacant(entry) = entries.entry(name.clone()) {
                let kind = match owned.remove(&name) {
                    Some(target)
                        if crate::utils::path::normalize_path_lexical(&home.join(&target))
                            == skill.path =>
                    {
                        DirEntryKind::Symlink { target }
                    }
                    _ => {
                        changed = true;
                        DirEntryKind::Bind
                    }
                };
                entry.insert(DirEntry {
                    name,
                    source: skill.source,
                    kind,
                });
            }
        }
    }
    changed |= !owned.is_empty();
    // A grant restricts only the capability skills it would withhold, and a withheld skill
    // restricts only itself, so a kind that cannot mark skills user-only refuses only when the
    // inventory holds one.
    if inputs.manual == ManualSkill::Unsupported && inputs.grant.is_some() {
        let on_disk = |entry: &DirEntry| entry.source.join("SKILL.md").is_file();
        if entries.iter().any(|(name, entry)| {
            on_disk(entry) && !crate::agents::skills::launch_callable(None, &[], inputs.grant, name)
        }) {
            return Err(SandboxErr::GrantNeedsView {
                kind: inputs.kind.to_owned(),
            });
        }
        if inputs
            .withheld
            .iter()
            .any(|name| entries.get(*name).is_some_and(on_disk))
        {
            return Err(SandboxErr::ManualSkillsUnsupported {
                kind: inputs.kind.to_owned(),
            });
        }
    }
    let mut shadows = BTreeMap::new();
    if let Some(callable) = inputs.callable {
        crate::config::validate_skill_list(callable)?;
        for name in callable {
            let natively_read = native.as_ref().is_some_and(|folder| {
                !name.as_str().starts_with("rimz-")
                    && folder.join(name.as_str()).join("SKILL.md").exists()
            });
            let winner = match (entries.get(name.as_str()), &native) {
                (Some(_), _) => tier_skills
                    .get(name.as_str())
                    .map(|skill| skill.path.clone())
                    .or_else(|| entries.get(name.as_str()).map(|entry| entry.source.clone())),
                (None, Some(folder)) if natively_read => Some(folder.join(name.as_str())),
                (None, _) => None,
            };
            if let (Some(winner), Some(project)) = (winner, &project)
                && let Some(refusal) = project.user_only(name, &winner)
            {
                return Err(refusal.into());
            }
            if !entries.contains_key(name.as_str()) && !natively_read {
                if let Some(conflict) = conflicts.get(name.as_str()) {
                    return Err(SkillErr::Conflict {
                        message: conflict.message.clone(),
                    }
                    .into());
                }
                return Err(SandboxErr::UnknownSkill {
                    name: name.to_string(),
                    roots,
                });
            }
        }
    }
    if inputs.callable.is_some() || !inputs.withheld.is_empty() || inputs.grant.is_some() {
        let mut policies = BTreeMap::new();
        for (name, entry) in &entries {
            if !entry.source.is_dir() || !entry.source.join("SKILL.md").is_file() {
                continue;
            }
            let listed = crate::agents::skills::launch_callable(
                inputs.callable,
                inputs.withheld,
                inputs.grant,
                name,
            );
            if let Some((other_name, other_listed)) = policies.get(&entry.source) {
                if listed != *other_listed {
                    return Err(SandboxErr::ConflictingSkillAliases {
                        listed: if listed { name } else { other_name }.clone(),
                        unlisted: if listed { other_name } else { name }.clone(),
                        path: entry.source.clone(),
                    });
                }
                continue;
            }
            policies.insert(entry.source.clone(), (name.clone(), listed));
        }
        for (name, entry) in &entries {
            if !matches!(policies.get(&entry.source), Some((_, false)))
                || shadows.contains_key(&entry.source)
            {
                continue;
            }
            let confined = tier_skills.contains_key(name);
            match rewrite::plan(skills_dir, &entry.source, inputs.manual, confined) {
                Ok(copy) => {
                    shadows.insert(entry.source.clone(), copy.target.clone());
                    copies.push(copy);
                }
                Err(rewrite::MaterializeErr::Unusable { path, reason }) => {
                    tracing::debug!(skill = %name, path = %path.display(), "omitting skill the view cannot prepare");
                    skipped.push(SkippedSkill {
                        name: name.clone(),
                        path,
                        reason,
                    });
                }
                Err(rewrite::MaterializeErr::Sandbox(error)) => return Err(error),
            }
            changed = true;
        }
    }
    let ungoverned = native
        .filter(|_| {
            inputs.callable.is_some() || !inputs.withheld.is_empty() || inputs.grant.is_some()
        })
        .and_then(|folder| {
            // A copy its author marks user-only stays out of the model's reach, and a kind that
            // loads its user root first loads the view's copy of a name the view holds.
            let user_first = project.as_ref().is_some_and(ProjectTier::user_first);
            let left_out: Vec<String> = crate::agents::skills::project_skills(&folder, |name| {
                !crate::agents::skills::launch_callable(
                    inputs.callable,
                    inputs.withheld,
                    inputs.grant,
                    name,
                ) && !(user_first && entries.contains_key(name))
            })
            .ok()?
            .into_iter()
            .filter(|(_, skill)| {
                !matches!(
                    crate::agents::skills::is_user_only(&skill.source, inputs.manual, |path| {
                        std::fs::read_to_string(path)
                    }),
                    Ok(true)
                )
            })
            .map(|(name, _)| name)
            .collect();
            (!left_out.is_empty()).then_some((folder, left_out))
        });
    if !changed {
        return Ok(SkillView {
            ungoverned,
            ..SkillView::default()
        });
    }
    for skill in &skipped {
        entries.remove(&skill.name);
    }
    let root = match home.canonicalize() {
        Ok(root) => root,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            crate::utils::path::normalize_path_lexical(home)
        }
        Err(source) => {
            return Err(SandboxErr::Io {
                path: home.clone(),
                source,
            });
        }
    };
    super::validate_path(&root)?;
    for entry in entries.values_mut() {
        super::validate_path(&entry.source)?;
        if matches!(entry.kind, DirEntryKind::Bind)
            && let Some(copy) = shadows.get(&entry.source)
        {
            entry.source = copy.clone();
        }
    }
    shadows.retain(|target, _| {
        entries.values().any(|entry| {
            matches!(entry.kind, DirEntryKind::Symlink { .. }) && entry.source == *target
        }) && !entries
            .values()
            .any(|entry| root.join(&entry.name) == *target)
    });
    Ok(SkillView {
        dir: Some(DirView {
            root,
            entries: entries.into_values().collect(),
            shadows,
        }),
        skipped,
        copies,
        ungoverned,
    })
}

fn list_dir(root: &Path, chain: Option<&SkillChain>) -> Result<ProviderEntries, SandboxErr> {
    let listing = match std::fs::read_dir(root) {
        Ok(listing) => listing,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ProviderEntries::default());
        }
        Err(source) => {
            return Err(SandboxErr::Io {
                path: root.to_path_buf(),
                source,
            });
        }
    };
    let mut entries = BTreeMap::new();
    let mut owned = BTreeMap::new();
    for entry in listing {
        let entry = entry.map_err(|source| SandboxErr::Io {
            path: root.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| SandboxErr::InvalidPath(path.clone()))?;
        if let Some(chain) = chain
            && let Some(target) = chain.owned_link(&path)?
        {
            if target.to_str().is_none() {
                return Err(SandboxErr::InvalidPath(path));
            }
            owned.insert(name, target);
            continue;
        }
        let source = match path.canonicalize() {
            Ok(source) => source,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(path = %path.display(), "skipping broken skill symlink");
                continue;
            }
            Err(source) => return Err(SandboxErr::Io { path, source }),
        };
        let kind = if path.is_symlink() {
            let target = std::fs::read_link(&path).map_err(|source| SandboxErr::Io {
                path: path.clone(),
                source,
            })?;
            if target.to_str().is_none() {
                return Err(SandboxErr::InvalidPath(path));
            }
            DirEntryKind::Symlink { target }
        } else {
            DirEntryKind::Bind
        };
        entries.insert(name.clone(), DirEntry { name, source, kind });
    }
    Ok(ProviderEntries { entries, owned })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _temp: tempfile::TempDir,
        env: BTreeMap<String, String>,
        root: PathBuf,
        checkout: PathBuf,
        library: PathBuf,
        skills_dir: PathBuf,
    }

    /// A provider root, a library holding `shared` and `lib`, and a checkout whose
    /// `.agents/skills` holds `proj` and `shared`.
    fn fixture() -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let root = base.join("home/provider-skills");
        let library = base.join("agents/skills");
        let checkout = base.join("checkout");
        std::fs::create_dir_all(&root).unwrap();
        for dir in [
            library.join("shared"),
            library.join("lib"),
            checkout.join(".agents/skills/proj"),
            checkout.join(".agents/skills/shared"),
        ] {
            write_skill(&dir);
        }
        Fixture {
            env: BTreeMap::from([
                ("HOME".into(), base.join("home").display().to_string()),
                (
                    "RIMZ_AGENTS_HOME".into(),
                    base.join("agents").display().to_string(),
                ),
            ]),
            skills_dir: base.join("state/skills"),
            _temp: temp,
            root,
            checkout,
            library,
        }
    }

    fn write_skill(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            "---\ndescription: Skill.\n---\nSkill.\n",
        )
        .unwrap();
    }

    fn view(fixture: &Fixture, kind: &str, listed: &[&str]) -> Result<SkillView, SandboxErr> {
        let adapter = crate::agents::find_definition(kind).unwrap();
        let listed: Vec<crate::config::SkillName> =
            listed.iter().map(|name| name.parse().unwrap()).collect();
        plan_view(
            &fixture.env,
            &fixture.skills_dir,
            &fixture.checkout,
            &SkillInputs {
                kind,
                home: Some(fixture.root.clone()),
                manual: adapter.manual_skill(),
                withheld: &[],
                callable: Some(&listed),
                grant: None,
            },
        )
    }

    fn sources(view: &SkillView) -> BTreeMap<String, PathBuf> {
        view.dir
            .iter()
            .flat_map(|dir| &dir.entries)
            .map(|entry| (entry.name.clone(), entry.source.clone()))
            .collect()
    }

    #[test]
    fn a_kind_that_does_not_read_the_tier_gets_it_merged_above_the_library() {
        let fixture = fixture();
        let tier = fixture.checkout.join(".agents/skills");
        let listed = view(&fixture, "claude", &["proj", "shared", "lib"]).unwrap_or_else(|error| {
            panic!("a listed project skill resolves through the merged view: {error}")
        });
        assert_eq!(
            sources(&listed),
            BTreeMap::from([
                ("lib".to_owned(), fixture.library.join("lib")),
                ("proj".to_owned(), tier.join("proj")),
                ("shared".to_owned(), tier.join("shared")),
            ]),
            "the project copy of shared outranks the library's"
        );
        let unlisted = view(&fixture, "claude", &["lib"]).unwrap();
        let copied: Vec<_> = unlisted.copies.iter().map(|copy| &copy.source).collect();
        assert_eq!(
            copied,
            [&tier.join("proj"), &tier.join("shared")],
            "an unlisted project skill gets the rewritten user-only copy a library skill gets"
        );
    }

    #[test]
    fn a_native_reader_keeps_the_tier_native_and_drops_the_library_copies_it_shadows() {
        let fixture = fixture();
        let listed = view(&fixture, "codex", &["proj", "lib"]).unwrap_or_else(|error| {
            panic!("a listed name the tier holds resolves for a native reader: {error}")
        });
        assert_eq!(
            sources(&listed),
            BTreeMap::from([("lib".to_owned(), fixture.library.join("lib"))]),
            "the tier stays out of the view, and the library's shared is shadowed by the tier's"
        );
        assert_eq!(
            listed.ungoverned,
            Some((
                fixture.checkout.join(".agents/skills"),
                vec!["shared".to_owned()]
            ))
        );
    }

    #[test]
    fn a_kinds_own_project_folder_left_out_of_the_list_is_reported_ungoverned() {
        let fixture = fixture();
        let own = fixture.checkout.join(".claude/skills");
        write_skill(&own.join("own"));
        write_skill(&own.join("own-listed"));
        let view = view(&fixture, "claude", &["own-listed"]).unwrap();
        assert_eq!(view.ungoverned, Some((own, vec!["own".to_owned()])));
    }

    #[test]
    fn the_checkout_tier_stays_a_read_only_protected_mount() {
        let fixture = fixture();
        let unit = fixture.checkout.parent().unwrap().join("unit");
        std::fs::create_dir_all(&unit).unwrap();
        let listed: Vec<crate::config::SkillName> = vec!["lib".parse().unwrap()];
        for (kind, manual) in [
            ("codex", ManualSkill::OpenAiPolicy),
            ("claude", ManualSkill::Frontmatter),
        ] {
            let plan = super::super::plan(&super::super::SandboxInputs {
                env: &fixture.env,
                cwd: &fixture.checkout,
                project_root: &fixture.checkout,
                worktree: None,
                checkout: Some(&fixture.checkout),
                add_dirs: &[],
                read_only: Vec::new(),
                tmp_dir: &unit,
                skills_dir: &fixture.skills_dir,
                ssh_config_file: Path::new("/state/missing-ssh-config"),
                ssh_config_copies_dir: Path::new("/state/ssh-config-copies"),
                provider_home: None,
                provider_home_env_keys: &[],
                default_home: None,
                skills: SkillInputs {
                    kind,
                    home: Some(fixture.root.clone()),
                    manual,
                    withheld: &[],
                    callable: Some(&listed),
                    grant: None,
                },
            })
            .unwrap();
            let agents = fixture.checkout.join(".agents");
            let touching: Vec<_> = plan
                .plan
                .mounts
                .iter()
                .filter(|mount| {
                    use super::super::Mount;
                    matches!(mount,
                        Mount::Bind { target, .. }
                        | Mount::RoBind { target, .. }
                        | Mount::RoBindTry { target, .. }
                        | Mount::Tmpfs { target }
                        | Mount::RemountRo { target }
                        | Mount::Symlink { path: target, .. } if target.starts_with(&agents))
                })
                .collect();
            assert_eq!(
                touching,
                [&super::super::Mount::RoBindTry {
                    source: agents.clone(),
                    target: agents.clone(),
                }],
                "{kind}"
            );
        }
    }

    #[test]
    fn a_project_folder_never_replaces_an_installed_rimz_skill() {
        let fixture = fixture();
        write_skill(&fixture.library.join("rimz-lsp"));
        write_skill(&fixture.checkout.join(".agents/skills/rimz-lsp"));
        for kind in ["claude", "codex"] {
            let view = view(&fixture, kind, &["rimz-lsp"]).unwrap();
            assert_eq!(
                sources(&view).get("rimz-lsp"),
                Some(&fixture.library.join("rimz-lsp")),
                "{kind}: the view keeps the installed rimz-lsp, so its routine grant reaches it"
            );
        }
    }

    #[test]
    fn the_ungoverned_report_skips_user_only_and_view_held_names() {
        let fixture = fixture();
        let own = fixture.checkout.join(".claude/skills");
        write_skill(&own.join("plain"));
        write_skill(&own.join("lib"));
        std::fs::create_dir_all(own.join("marked")).unwrap();
        std::fs::write(
            own.join("marked/SKILL.md"),
            "---\ndescription: Marked.\ndisable-model-invocation: true\n---\nMarked.\n",
        )
        .unwrap();
        let view = view(&fixture, "claude", &["proj"]).unwrap();
        assert_eq!(
            view.ungoverned,
            Some((own, vec!["plain".to_owned()])),
            "marked is user-only by its author, and Claude loads the view's lib over its own"
        );
    }

    #[test]
    fn a_tier_that_is_the_provider_root_is_not_a_project_tier() {
        let fixture = fixture();
        let home = fixture.checkout.join(".agents/skills");
        std::os::unix::fs::symlink(fixture.library.join("lib"), home.join("lib")).unwrap();
        let adapter = crate::agents::find_definition("codex").unwrap();
        let listed: Vec<crate::config::SkillName> = vec!["lib".parse().unwrap()];
        let view = plan_view(
            &fixture.env,
            &fixture.skills_dir,
            &fixture.checkout,
            &SkillInputs {
                kind: "codex",
                home: Some(home),
                manual: adapter.manual_skill(),
                withheld: &[],
                callable: Some(&listed),
                grant: None,
            },
        )
        .unwrap();
        assert_eq!(
            sources(&view).get("lib"),
            Some(&fixture.library.join("lib")),
            "RimZ's own link in the provider root keeps the library skill in the view"
        );
    }

    #[test]
    fn a_scoped_list_refuses_a_listed_skill_its_author_marks_user_only() {
        let fixture = fixture();
        let skill = fixture.checkout.join(".agents/skills/proj");
        std::fs::write(
            skill.join("SKILL.md"),
            "---\ndescription: Marked.\ndisable-model-invocation: true\n---\nMarked.\n",
        )
        .unwrap();
        match view(&fixture, "claude", &["proj"]) {
            Err(SandboxErr::SkillDiscovery(error @ SkillErr::UserOnly { .. })) => assert_eq!(
                error.to_string(),
                format!(
                    "listed skill 'proj' is marked user-only for claude at {}; listing cannot \
                     lift that marker: drop it from skills or remove the marker",
                    skill.join("SKILL.md").display()
                )
            ),
            Err(error) => panic!("a listed user-only skill refuses: {error}"),
            Ok(_) => panic!("a listed user-only skill refuses"),
        }
    }

    #[test]
    fn a_scoped_unknown_skill_names_the_kinds_own_project_folder() {
        let fixture = fixture();
        let Err(SandboxErr::UnknownSkill { roots, .. }) = view(&fixture, "claude", &["absent"])
        else {
            panic!("an absent name is unknown")
        };
        assert!(
            roots.contains(&fixture.checkout.join(".claude/skills")),
            "the view searched Claude's own folder: {roots:?}"
        );
    }
}
