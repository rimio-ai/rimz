//! Share provider settings without sharing account credentials or history.

use std::fs;
use std::path::{Path, PathBuf};

use super::AgentDefinition;
use super::capabilities::{SharedHomeEntry, SharedHomeKind};
use super::skill_links;
use crate::utils::path::normalize_path_lexical;

#[derive(Debug, thiserror::Error)]
pub enum ShareErr {
    #[error("`--home` names the provider's own home, which is the `default` account ({}); choose another home", home.display())]
    SameHome { home: PathBuf },
    #[error("cannot move {} aside: {} exists; remove or rename {}, then rerun `rimz accounts add`", slot.display(), orig.display(), orig.display())]
    OrigExists { slot: PathBuf, orig: PathBuf },
    #[error("cannot share settings at {}: {source}; fix access to that path, then rerun `rimz accounts add`", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot adopt {}: {source}; fix the settings file, then rerun `rimz accounts add`", path.display())]
    Adopt {
        path: PathBuf,
        source: super::AgentErr,
    },
}

#[derive(Debug, Default)]
pub struct ShareReport {
    pub default_home: PathBuf,
    pub linked: Vec<&'static str>,
    pub current: Vec<&'static str>,
    pub moved_aside: Vec<(PathBuf, PathBuf)>,
    pub notes: Vec<String>,
}

impl std::fmt::Display for ShareReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (slot, orig) in &self.moved_aside {
            writeln!(f, "moved aside {} → {}", slot.display(), orig.display())?;
        }
        for note in &self.notes {
            writeln!(f, "{note}")?;
        }
        if self.linked.is_empty() {
            write!(
                f,
                "settings already shared with {}",
                self.default_home.display()
            )
        } else {
            write!(
                f,
                "linked settings to {}: {}",
                self.default_home.display(),
                self.linked.join(", ")
            )
        }
    }
}

enum Action {
    Link,
    Current,
    MoveAside {
        orig: PathBuf,
        file_type: fs::FileType,
    },
}

struct PlannedLink {
    entry: &'static SharedHomeEntry,
    slot: PathBuf,
    target: PathBuf,
    action: Action,
}

fn io_err(path: &Path, source: std::io::Error) -> ShareErr {
    ShareErr::Io {
        path: path.into(),
        source,
    }
}

fn metadata(path: &Path) -> Result<Option<fs::Metadata>, ShareErr> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_err(path, error)),
    }
}

fn classify(slot: &Path, target: &Path, name: &str) -> Result<Action, ShareErr> {
    let Some(existing) = metadata(slot)? else {
        return Ok(Action::Link);
    };
    if existing.is_symlink() {
        let link = fs::read_link(slot).map_err(|error| io_err(slot, error))?;
        // Slots are named entries joined onto the account home.
        let parent = slot.parent().expect("settings slot has a parent");
        if normalize_path_lexical(&parent.join(link)) == target {
            return Ok(Action::Current);
        }
    }
    let orig = slot.with_file_name(format!("{name}.orig"));
    if metadata(&orig)?.is_some() {
        return Err(ShareErr::OrigExists {
            slot: slot.into(),
            orig,
        });
    }
    Ok(Action::MoveAside {
        orig,
        file_type: existing.file_type(),
    })
}

pub fn check_distinct_homes(named_home: &Path, default_home: &Path) -> Result<(), ShareErr> {
    let named_canonical = match named_home.canonicalize() {
        Ok(home) => home,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_err(named_home, error)),
    };
    match default_home.canonicalize() {
        Ok(home) if home == named_canonical => Err(ShareErr::SameHome { home }),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_err(default_home, error)),
    }
}

pub fn share_settings(
    adapter: &AgentDefinition,
    named_home: &Path,
    default_home: &Path,
) -> Result<ShareReport, ShareErr> {
    check_distinct_homes(named_home, default_home)?;
    let named_home = std::path::absolute(named_home)
        .map(|home| normalize_path_lexical(&home))
        .map_err(|error| io_err(named_home, error))?;
    let default_home = std::path::absolute(default_home)
        .map(|home| normalize_path_lexical(&home))
        .map_err(|error| io_err(default_home, error))?;
    let mut plan = Vec::new();
    for entry in adapter.shared_home_entries() {
        let slot = named_home.join(entry.name);
        let target = default_home.join(entry.name);
        let action = classify(&slot, &target, entry.name)?;
        plan.push(PlannedLink {
            entry,
            slot,
            target,
            action,
        });
    }
    let mut report = ShareReport {
        default_home,
        ..ShareReport::default()
    };
    for PlannedLink {
        entry,
        slot,
        target,
        action,
    } in plan
    {
        if matches!(entry.kind, SharedHomeKind::Dir) {
            fs::create_dir_all(&target).map_err(|error| io_err(&target, error))?;
        }
        match action {
            Action::Current => {
                report.current.push(entry.name);
                continue;
            }
            Action::MoveAside { orig, file_type } => {
                if file_type.is_file()
                    && let Some(note) = adapter
                        .adopt_shared_file(entry.name, &slot, &target)
                        .map_err(|source| ShareErr::Adopt {
                            path: slot.clone(),
                            source,
                        })?
                {
                    report.notes.push(note);
                }
                let empty_skill_root = if entry.name == "skills" && file_type.is_dir() {
                    skill_links::plan(
                        &slot,
                        &crate::disk::paths::skills_library(),
                        skill_links::Desired::None,
                    )
                    .and_then(|plan| skill_links::apply(&plan))
                    .map_err(
                        |skill_links::SkillLinkErr::Io { path, source }| io_err(&path, source),
                    )?;
                    fs::read_dir(&slot)
                        .map_err(|error| io_err(&slot, error))?
                        .next()
                        .is_none()
                } else {
                    false
                };
                if empty_skill_root {
                    fs::remove_dir(&slot).map_err(|error| io_err(&slot, error))?;
                } else {
                    fs::rename(&slot, &orig).map_err(|error| io_err(&slot, error))?;
                    report.moved_aside.push((slot.clone(), orig));
                }
            }
            Action::Link => {}
        }
        std::os::unix::fs::symlink(&target, &slot).map_err(|error| io_err(&slot, error))?;
        report.linked.push(entry.name);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    fn names(path: &Path) -> Vec<std::ffi::OsString> {
        let mut names = fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    #[test]
    fn migration_removes_owned_skill_links_and_preserves_user_entries() {
        for user_entry in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let named = temp.path().join("named");
            let native = temp.path().join("native");
            let skills = named.join("skills");
            fs::create_dir_all(&skills).unwrap();
            symlink(
                crate::disk::paths::skills_library().join("owned"),
                skills.join("owned"),
            )
            .unwrap();
            if user_entry {
                fs::create_dir(skills.join("user")).unwrap();
            }
            share_settings(
                super::super::definition_by_kind("claude").unwrap(),
                &named,
                &native,
            )
            .unwrap();
            assert_eq!(fs::read_link(&skills).unwrap(), native.join("skills"));
            let orig = named.join("skills.orig");
            assert_eq!(orig.exists(), user_entry);
            if user_entry {
                assert_eq!(names(&orig), [std::ffi::OsString::from("user")]);
            }
        }
    }

    #[test]
    fn fresh_homes_share_expected_entries_and_rerun_changes_nothing() {
        for (kind, files, dirs) in [
            ("codex", &["config.toml", "AGENTS.md"][..], &[][..]),
            (
                "claude",
                &["settings.json", "settings.local.json", "CLAUDE.md"][..],
                &["skills", "plugins", "agents", "commands", "output-styles"][..],
            ),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let named = temp.path().join("named");
            let native = temp.path().join("native");
            fs::create_dir(&named).unwrap();
            let adapter = super::super::definition_by_kind(kind).unwrap();
            let report = share_settings(adapter, &named, &native).unwrap();
            assert_eq!(report.linked.len(), files.len() + dirs.len());
            assert!(report.current.is_empty());
            for name in files.iter().chain(dirs) {
                assert_eq!(fs::read_link(named.join(name)).unwrap(), native.join(name));
                assert_eq!(native.join(name).is_dir(), dirs.contains(name));
                if files.contains(name) {
                    assert!(
                        !native.join(name).exists(),
                        "missing files stay unconfigured"
                    );
                }
            }
            let before = names(&named);
            let again = share_settings(adapter, &named, &native).unwrap();
            assert!(again.linked.is_empty());
            assert_eq!(again.current, report.linked);
            assert!(again.moved_aside.is_empty());
            assert!(again.notes.is_empty());
            assert_eq!(names(&named), before);
            for name in files.iter().chain(dirs) {
                assert_eq!(fs::read_link(named.join(name)).unwrap(), native.join(name));
            }
            assert_eq!(
                again.to_string(),
                format!("settings already shared with {}", native.display())
            );
        }
    }

    #[test]
    fn relative_links_to_the_native_entries_are_current() {
        let temp = tempfile::tempdir().unwrap();
        let named = temp.path().join("named");
        let native = temp.path().join("native");
        fs::create_dir(&named).unwrap();
        for name in ["config.toml", "AGENTS.md"] {
            symlink(Path::new("../native").join(name), named.join(name)).unwrap();
        }
        fs::write(named.join("config.toml.orig"), "preserved").unwrap();
        let report = share_settings(
            super::super::definition_by_kind("codex").unwrap(),
            &named,
            &native,
        )
        .unwrap();
        assert_eq!(report.current, ["config.toml", "AGENTS.md"]);
        assert!(report.linked.is_empty());
        assert_eq!(
            fs::read_link(named.join("config.toml")).unwrap(),
            Path::new("../native/config.toml")
        );
        assert_eq!(
            fs::read_to_string(named.join("config.toml.orig")).unwrap(),
            "preserved"
        );
    }

    #[test]
    fn moves_files_directories_and_foreign_links_aside() {
        let temp = tempfile::tempdir().unwrap();
        let named = temp.path().join("named");
        let native = temp.path().join("native");
        fs::create_dir_all(named.join("skills")).unwrap();
        fs::write(named.join("settings.json"), "{}").unwrap();
        fs::write(named.join("skills/private"), "mine").unwrap();
        symlink("elsewhere", named.join("plugins")).unwrap();
        let report = share_settings(
            super::super::definition_by_kind("claude").unwrap(),
            &named,
            &native,
        )
        .unwrap();
        assert_eq!(report.moved_aside.len(), 3);
        assert_eq!(
            fs::read_to_string(named.join("settings.json.orig")).unwrap(),
            "{}"
        );
        assert_eq!(
            fs::read_to_string(named.join("skills.orig/private")).unwrap(),
            "mine"
        );
        assert_eq!(
            fs::read_link(named.join("plugins.orig")).unwrap(),
            Path::new("elsewhere")
        );
        for (slot, orig) in &report.moved_aside {
            assert!(report.to_string().contains(&format!(
                "{} → {}",
                slot.display(),
                orig.display()
            )));
            assert!(fs::symlink_metadata(slot).unwrap().is_symlink());
        }
    }

    #[test]
    fn orig_conflict_refuses_before_any_changes() {
        let temp = tempfile::tempdir().unwrap();
        let named = temp.path().join("named");
        let native = temp.path().join("native");
        fs::create_dir(&named).unwrap();
        let slot = named.join("settings.local.json");
        let orig = named.join("settings.local.json.orig");
        fs::write(&slot, "mine").unwrap();
        symlink("missing", &orig).unwrap();
        let before = names(&named);
        let error = share_settings(
            super::super::definition_by_kind("claude").unwrap(),
            &named,
            &native,
        )
        .unwrap_err();
        assert!(matches!(error, ShareErr::OrigExists { .. }));
        assert!(error.to_string().contains("remove or rename"));
        assert_eq!(names(&named), before);
        assert_eq!(fs::read_to_string(slot).unwrap(), "mine");
        assert_eq!(fs::read_link(orig).unwrap(), Path::new("missing"));
        assert!(!native.exists());
    }

    #[test]
    fn same_home_through_symlink_refuses_without_changes() {
        let temp = tempfile::tempdir().unwrap();
        let native = temp.path().join("native");
        let named = temp.path().join("named");
        fs::create_dir(&native).unwrap();
        symlink(&native, &named).unwrap();
        let error = share_settings(
            super::super::definition_by_kind("codex").unwrap(),
            &named,
            &native,
        )
        .unwrap_err();
        assert!(matches!(error, ShareErr::SameHome { .. }));
        assert!(names(&native).is_empty());
        assert_eq!(fs::read_link(named).unwrap(), native);
    }
}
