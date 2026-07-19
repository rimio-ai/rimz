//! Per-machine remote aliases persisted at `$XDG_CONFIG_HOME/rimz/remote.toml`.
//!
//! Schema (TOML):
//!
//! ```toml
//! [[remote]]
//! name = "prod"
//! target = "agent@prod-box:query-engine"
//! reconnect = true
//! no_resume = false
//! mux = "tmux"
//! forward = ["3000", "8080:3000"]
//! ```

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ids::MuxName;
use crate::remote::forward::{self, PortForward, PortForwardError};
use crate::remote::{RemoteTarget, RemoteTargetError};
use crate::store::atomic;
use crate::store::paths::config_home;

pub const REMOTE_FILE: &str = "remote.toml";
pub const REMOTE_TEMPLATE: &str = include_str!("../config/templates/remote.template.toml");
const RIMZ_CONFIG_SUBDIR: &str = "rimz";
const ALIAS_NAME_MAX_LEN: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum AliasErr {
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing remote aliases at {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("serializing remote aliases: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("writing remote aliases: {0}")]
    Atomic(#[from] atomic::AtomicErr),
    #[error("remote alias `{0}` already exists")]
    DuplicateName(String),
    #[error("remote alias `{0}` does not exist")]
    NotFound(String),
    #[error(
        "invalid remote alias name `{0}`; use 1..={max} ASCII alphanumeric, `-`, or `_` characters, starting with an alphanumeric or `_`",
        max = ALIAS_NAME_MAX_LEN
    )]
    InvalidName(String),
    #[error(transparent)]
    InvalidTarget(#[from] RemoteTargetError),
    #[error("remote alias `{name}` has an invalid forward: {source}")]
    InvalidForward {
        name: String,
        #[source]
        source: PortForwardError,
    },
}

pub type Result<T> = std::result::Result<T, AliasErr>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteAlias {
    pub name: String,
    pub target: String,
    #[serde(
        default = "default_reconnect",
        skip_serializing_if = "is_default_reconnect"
    )]
    pub reconnect: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub no_resume: bool,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub mux: Option<MuxName>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forward: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteAliases {
    #[serde(default, rename = "remote")]
    entries: Vec<RemoteAlias>,
}

impl RemoteAliases {
    /// Load aliases from `$XDG_CONFIG_HOME/rimz/remote.toml`. A missing file is
    /// an empty alias set.
    pub fn load() -> Result<Self> {
        Self::load_from(&default_path())
    }

    /// Save aliases to `$XDG_CONFIG_HOME/rimz/remote.toml`.
    pub fn save(&self) -> Result<()> {
        self.save_to(&default_path())
    }

    pub fn config_path() -> PathBuf {
        default_path()
    }

    pub fn ensure_template() -> Result<bool> {
        let path = Self::config_path();
        Self::ensure_template_at(&path)
    }

    fn ensure_template_at(path: &Path) -> Result<bool> {
        if path.exists() {
            return Ok(false);
        }
        atomic::write_bytes_atomically(path, REMOTE_TEMPLATE.as_bytes())?;
        Ok(true)
    }

    /// Test-only loader. Production callers go through [`Self::load`].
    pub fn load_from(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let parsed: Self = toml::from_str(&text).map_err(|source| AliasErr::Parse {
                    path: path.to_path_buf(),
                    source,
                })?;
                Ok(parsed.validated()?.sorted())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(AliasErr::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    /// Test-only writer. Production callers go through [`Self::save`].
    pub fn save_to(&self, path: &Path) -> Result<()> {
        let sorted = self.clone().validated()?.sorted();
        let text = toml::to_string_pretty(&sorted)?;
        atomic::write_bytes_atomically(path, text.as_bytes())?;
        Ok(())
    }

    pub fn entries(&self) -> &[RemoteAlias] {
        &self.entries
    }

    pub fn contains(&self, name: &str) -> bool {
        self.entries.iter().any(|entry| entry.name == name)
    }

    pub fn get(&self, name: &str) -> Option<&RemoteAlias> {
        self.entries.iter().find(|entry| entry.name == name)
    }

    pub fn add(&mut self, mut entry: RemoteAlias) -> Result<()> {
        validate_name(&entry.name)?;
        RemoteTarget::parse(&entry.target)?;
        validate_forwards(&mut entry)?;
        if self.contains(&entry.name) {
            return Err(AliasErr::DuplicateName(entry.name));
        }
        self.entries.push(entry);
        self.entries.sort_by(sort_key);
        Ok(())
    }

    pub fn update(&mut self, mut entry: RemoteAlias) -> Result<()> {
        validate_name(&entry.name)?;
        RemoteTarget::parse(&entry.target)?;
        validate_forwards(&mut entry)?;
        let slot = self
            .entries
            .iter_mut()
            .find(|existing| existing.name == entry.name)
            .ok_or_else(|| AliasErr::NotFound(entry.name.clone()))?;
        *slot = entry;
        Ok(())
    }

    pub fn remove(&mut self, name: &str) -> Result<()> {
        let len_before = self.entries.len();
        self.entries.retain(|entry| entry.name != name);
        if self.entries.len() == len_before {
            return Err(AliasErr::NotFound(name.to_owned()));
        }
        Ok(())
    }

    pub fn rename(&mut self, old: &str, new: String) -> Result<()> {
        validate_name(&new)?;
        if self.contains(&new) {
            return Err(AliasErr::DuplicateName(new));
        }
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.name == old)
            .ok_or_else(|| AliasErr::NotFound(old.to_owned()))?;
        entry.name = new;
        self.entries.sort_by(sort_key);
        Ok(())
    }

    fn sorted(mut self) -> Self {
        self.entries.sort_by(sort_key);
        self
    }

    fn validated(mut self) -> Result<Self> {
        for entry in &mut self.entries {
            validate_name(&entry.name)?;
            validate_forwards(entry)?;
        }
        Ok(self)
    }
}

fn validate_forwards(entry: &mut RemoteAlias) -> Result<()> {
    let parsed = entry
        .forward
        .iter()
        .map(|spec| spec.parse::<PortForward>())
        .collect::<std::result::Result<Vec<_>, _>>()
        .and_then(|parsed| forward::merged(&[], &parsed))
        .map_err(|source| AliasErr::InvalidForward {
            name: entry.name.clone(),
            source,
        })?;
    entry.forward = parsed
        .into_iter()
        .map(|forward| forward.to_string())
        .collect();
    Ok(())
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > ALIAS_NAME_MAX_LEN
        || name.starts_with('-')
        || name
            .chars()
            .any(|ch| !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '_'))
    {
        return Err(AliasErr::InvalidName(name.to_owned()));
    }
    Ok(())
}

fn sort_key(a: &RemoteAlias, b: &RemoteAlias) -> std::cmp::Ordering {
    a.name.cmp(&b.name)
}

fn default_path() -> PathBuf {
    config_home().join(RIMZ_CONFIG_SUBDIR).join(REMOTE_FILE)
}

fn default_reconnect() -> bool {
    true
}

fn is_default_reconnect(value: &bool) -> bool {
    *value
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn alias(name: &str, target: &str) -> RemoteAlias {
        RemoteAlias {
            name: name.to_owned(),
            target: target.to_owned(),
            reconnect: true,
            no_resume: false,
            mux: None,
            forward: Vec::new(),
        }
    }

    #[test]
    fn template_and_missing_file_contract() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("remote.toml");
        let list = RemoteAliases::load_from(&path).unwrap();
        assert!(list.entries().is_empty());

        let template_path = dir.path().join("rimz").join("remote.toml");
        assert!(RemoteAliases::ensure_template_at(&template_path).unwrap());
        assert_eq!(
            std::fs::read_to_string(&template_path).unwrap(),
            REMOTE_TEMPLATE
        );
        std::fs::write(
            &template_path,
            "[[remote]]\nname = \"dev\"\ntarget = \"dev-box:app\"\n",
        )
        .unwrap();
        assert!(!RemoteAliases::ensure_template_at(&template_path).unwrap());
        assert!(
            RemoteAliases::load_from(&template_path)
                .unwrap()
                .contains("dev")
        );

        let list: RemoteAliases = toml::from_str(REMOTE_TEMPLATE).unwrap();
        assert!(list.entries().is_empty());

        let uncommented = REMOTE_TEMPLATE
            .lines()
            .map(|line| line.strip_prefix("## ").unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n");
        let list: RemoteAliases = toml::from_str(&uncommented).unwrap();
        let entry = list.get("prod").unwrap();
        assert_eq!(entry.target, "agent@prod-box:query-engine");
        assert!(entry.reconnect);
        assert!(!entry.no_resume);
        assert_eq!(entry.mux, Some(MuxName::Tmux));
        assert_eq!(entry.forward, ["3000", "8080:3000"]);
    }

    #[test]
    fn toml_round_trip_defaults_and_sorting() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("remote.toml");
        let mut list = RemoteAliases::default();
        list.add(alias("prod", "prod-box:query-engine")).unwrap();
        list.add(alias("dev", "dev-box:query-engine")).unwrap();
        list.save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("reconnect = true"), "{text}");
        assert!(!text.contains("no_resume = false"), "{text}");
        assert!(!text.contains("mux ="), "{text}");
        assert!(!text.contains("forward ="), "{text}");

        let reloaded = RemoteAliases::load_from(&path).unwrap();
        let names: Vec<&str> = reloaded
            .entries()
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(names, vec!["dev", "prod"]);

        let parsed: RemoteAliases = toml::from_str(
            r#"
            [[remote]]
            name = "prod"
            target = "prod-box:query-engine"
            "#,
        )
        .unwrap();
        let entry = parsed.get("prod").unwrap();
        assert!(entry.reconnect);
        assert!(!entry.no_resume);
        assert_eq!(entry.mux, None);
        assert!(entry.forward.is_empty());
    }

    #[test]
    fn mutations_enforce_crud_contract() {
        let mut list = RemoteAliases::default();
        list.add(alias("prod", "prod-box:query-engine")).unwrap();
        let err = list
            .add(alias("prod", "other-box:query-engine"))
            .unwrap_err();
        assert!(matches!(err, AliasErr::DuplicateName(_)));

        list.update(alias("prod", "prod-box:other-engine")).unwrap();
        let entries = list.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].target, "prod-box:other-engine");

        let err = list
            .update(alias("missing", "prod-box:query-engine"))
            .unwrap_err();
        assert!(matches!(err, AliasErr::NotFound(_)));

        let remove = list.remove("missing").unwrap_err();
        assert!(matches!(remove, AliasErr::NotFound(_)));
        let rename = list.rename("missing", "new".to_owned()).unwrap_err();
        assert!(matches!(rename, AliasErr::NotFound(_)));

        list.add(alias("dev", "dev-box:query-engine")).unwrap();
        let err = list.rename("dev", "prod".to_owned()).unwrap_err();
        assert!(matches!(err, AliasErr::DuplicateName(_)));
    }

    #[test]
    fn validation_rejects_invalid_names_and_targets() {
        let mut list = RemoteAliases::default();
        let long = "a".repeat(ALIAS_NAME_MAX_LEN + 1);
        for name in [
            "",
            "host:path",
            "has space",
            "has\ttab",
            "has\nnewline",
            "--print",
            long.as_str(),
        ] {
            assert!(
                matches!(
                    list.add(alias(name, "prod-box:query-engine")).unwrap_err(),
                    AliasErr::InvalidName(_)
                ),
                "`{name}` must be rejected",
            );
        }
        list.add(alias("prod", "prod-box:query-engine")).unwrap();
        assert!(matches!(
            list.rename("prod", "host:path".to_owned()).unwrap_err(),
            AliasErr::InvalidName(_)
        ));

        assert!(matches!(
            RemoteAliases::default()
                .add(alias("prod", "prod-box"))
                .unwrap_err(),
            AliasErr::InvalidTarget(_)
        ));
        assert!(matches!(
            list.update(alias("prod", "prod-box")).unwrap_err(),
            AliasErr::InvalidTarget(_)
        ));

        let dir = tempdir().unwrap();
        let path = dir.path().join("remote.toml");
        std::fs::write(
            &path,
            r#"
            [[remote]]
            name = "bad\tname"
            target = "prod-box:query-engine"
            "#,
        )
        .unwrap();

        let err = RemoteAliases::load_from(&path).unwrap_err();

        assert!(matches!(err, AliasErr::InvalidName(_)));
    }

    #[test]
    fn forwards_round_trip_canonically_and_invalid_specs_name_the_alias() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("remote.toml");
        let mut entry = alias("dev", "dev-box:query-engine");
        entry.forward = vec!["03000".to_owned(), "8080:3000".to_owned()];
        let mut aliases = RemoteAliases::default();
        aliases.add(entry).unwrap();
        aliases.save_to(&path).unwrap();

        let loaded = RemoteAliases::load_from(&path).unwrap();
        assert_eq!(loaded.get("dev").unwrap().forward, ["3000", "8080:3000"]);

        std::fs::write(
            &path,
            r#"
            [[remote]]
            name = "dev"
            target = "dev-box:query-engine"
            forward = ["nope"]
            "#,
        )
        .unwrap();
        let error = RemoteAliases::load_from(&path).unwrap_err().to_string();
        assert!(error.contains("remote alias `dev`"), "{error}");
        assert!(error.contains("invalid port"), "{error}");
    }
}
