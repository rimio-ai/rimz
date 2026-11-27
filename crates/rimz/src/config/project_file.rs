//! The project `.rimz/config.toml` as one declared shape, and the notices for keys no project
//! reader reads.

use std::collections::BTreeMap;
use std::path::Path;

use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use super::{CoreConfig, LspServerConfig, TeamsConfig};
use crate::ids::RoomLogins;
use crate::trust::{AgentConfig, HookConfig};

/// Every top-level key a project reader reads, each declared as deep as its reader names keys
/// and in its reader's own type, so a key this shape leaves unread is one RimZ drops. A subtree
/// another check reports on (profile rows, task rows, the refused machine tables) is opaque.
/// The trust hash keeps its own narrower read, `crate::trust::ProjectConfig`. The trusted
/// launch load parses this whole shape, so no field is stricter than the reader that owns its
/// key: a stricter one fails every launch over a key no launch reads (`table_or_absent`).
#[derive(Default, Deserialize)]
#[serde(default)]
pub(super) struct ProjectFile {
    pub(super) agents: ProjectAgents,
    pub(super) profiles: Option<toml::Value>,
    #[serde(deserialize_with = "table_or_absent")]
    pub(super) subagents: Option<ProjectSubagents>,
    #[serde(deserialize_with = "table_or_absent")]
    pub(super) lsp: Option<ProjectLsp>,
    tasks: Option<toml::Value>,
    hooks: Vec<HookConfig>,
    env: BTreeMap<String, String>,
    accounts: RoomLogins,
    #[serde(deserialize_with = "table_or_absent")]
    packs: Option<ProjectPacks>,
    tiers: Option<toml::Value>,
    models: Option<toml::Value>,
}

/// `agents` is either the `[agents]` table or the `[[agents]]` entries the trust hash reads.
/// The shape is chosen from the value's kind: a derived untagged enum buffers the subtree before
/// it tries a shape, which hides the subtree's unread keys from the check.
#[derive(Default)]
pub(super) enum ProjectAgents {
    #[default]
    Absent,
    Entries,
    Table(ProjectAgentsTable),
}

impl<'de> Deserialize<'de> for ProjectAgents {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Shape;

        impl<'de> Visitor<'de> for Shape {
            type Value = ProjectAgents;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an [agents] table or [[agents]] entries")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, entries: A) -> Result<Self::Value, A::Error> {
                Vec::<AgentConfig>::deserialize(serde::de::value::SeqAccessDeserializer::new(
                    entries,
                ))?;
                Ok(ProjectAgents::Entries)
            }

            fn visit_map<A: MapAccess<'de>>(self, table: A) -> Result<Self::Value, A::Error> {
                ProjectAgentsTable::deserialize(serde::de::value::MapAccessDeserializer::new(table))
                    .map(ProjectAgents::Table)
            }
        }

        deserializer.deserialize_any(Shape)
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub(super) struct ProjectAgentsTable {
    pub(super) teams: TeamsConfig,
    #[serde(rename = "env-reminder")]
    pub(super) env_reminder: Option<bool>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub(super) struct ProjectSubagents {
    pub(super) profiles: Option<toml::Value>,
}

/// `servers`, and the machine memory policy keys, which the launch load refuses by name.
#[derive(Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub(super) struct ProjectLsp {
    pub(super) servers: BTreeMap<String, LspServerConfig>,
    reserve_percent: Option<toml::Value>,
    reserve_min: Option<toml::Value>,
    idle_timeout: Option<toml::Value>,
    recycle_factor: Option<toml::Value>,
    recycle_min: Option<toml::Value>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ProjectPacks {
    expect: Option<toml::Value>,
}

/// A table field whose value of any other kind reads as absent, as the reader that looks the
/// table up drops it (`crate::packs::expectation_warnings` warns on a `packs` that is not a
/// table itself). The kind is read before the table is parsed, so every key inside it still
/// reaches the unread-key check, which a value buffered first would hide.
fn table_or_absent<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    struct TableOrAbsent<T>(std::marker::PhantomData<T>);

    impl<'de, T: Deserialize<'de>> Visitor<'de> for TableOrAbsent<T> {
        type Value = Option<T>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a table")
        }

        fn visit_map<A: MapAccess<'de>>(self, table: A) -> Result<Self::Value, A::Error> {
            T::deserialize(serde::de::value::MapAccessDeserializer::new(table)).map(Some)
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut entries: A) -> Result<Self::Value, A::Error> {
            while entries.next_element::<toml::Value>()?.is_some() {}
            Ok(None)
        }

        fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
            Ok(None)
        }
    }

    deserializer.deserialize_any(TableOrAbsent(std::marker::PhantomData))
}

/// A project config key that no project reader reads, as a room-entry warning and a doctor
/// finding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProjectKeyNotice {
    key: String,
    message: String,
    fix: &'static str,
}

impl ProjectKeyNotice {
    /// The key as a TOML dotted key naming it, its text escaped for one terminal line.
    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn fix(&self) -> &'static str {
        self.fix
    }
}

impl std::fmt::Display for ProjectKeyNotice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}; {}", self.message, self.fix)
    }
}

/// One step of a key's path through the file.
enum Segment {
    Key(String),
    Index(usize),
}

/// The keys of the project config at `project_root` that no project reader reads, in key order.
/// Empty where there is no file and where the trust read refuses the file, since that refusal is
/// its report. A known key whose value fails its typed parse silences only the keys inside that
/// value, which the trusted load reports. It never fails, so the room still opens.
pub fn project_key_notices(project_root: &Path) -> Vec<ProjectKeyNotice> {
    let Ok(Some(text)) = crate::trust::project_config_text(project_root) else {
        return Vec::new();
    };
    let Ok(doc) = toml::from_str::<toml::Value>(&text) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    unread(&mut Vec::new(), &doc, &mut paths);
    paths
        .into_iter()
        .filter(|path| !is_role_tier(path))
        .map(|path| notice(&doc, &path))
        .collect()
}

/// Collects the unread keys of `value`, the value at `keys`, from a pass over a table holding it
/// alone. A table that fails that pass is split into its keys; any other value that fails it (an
/// array is taken whole) is dropped.
fn unread(keys: &mut Vec<String>, value: &toml::Value, paths: &mut Vec<Vec<Segment>>) {
    let mut found = Vec::new();
    let parsed = serde_ignored::deserialize::<_, _, ProjectFile>(holding(keys, value), |path| {
        found.push(key_path(&path));
    });
    if parsed.is_ok() {
        paths.append(&mut found);
        return;
    }
    let Some(table) = value.as_table() else {
        return;
    };
    for (key, inner) in table {
        keys.push(key.clone());
        unread(keys, inner, paths);
        keys.pop();
    }
}

/// A table holding `value` alone at `keys`.
fn holding(keys: &[String], value: &toml::Value) -> toml::Value {
    keys.iter().rev().fold(value.clone(), |inner, key| {
        toml::Value::Table(toml::Table::from_iter([(key.clone(), inner)]))
    })
}

fn key_path(path: &serde_ignored::Path<'_>) -> Vec<Segment> {
    use serde_ignored::Path;
    let (Path::Seq { parent, .. }
    | Path::Map { parent, .. }
    | Path::Some { parent }
    | Path::NewtypeStruct { parent }
    | Path::NewtypeVariant { parent }) = path
    else {
        return Vec::new();
    };
    let mut segments = key_path(parent);
    match path {
        Path::Seq { index, .. } => segments.push(Segment::Index(*index)),
        Path::Map { key, .. } => segments.push(Segment::Key(key.clone())),
        _ => {}
    }
    segments
}

/// A team role's `tier`, which the trusted launch load refuses.
fn is_role_tier(path: &[Segment]) -> bool {
    matches!(
        path,
        [
            Segment::Key(agents),
            Segment::Key(teams),
            Segment::Key(_),
            Segment::Key(roles),
            Segment::Index(_),
            Segment::Key(tier),
        ] if agents == "agents" && teams == "teams" && roles == "roles" && tier == "tier"
    )
}

fn notice(doc: &toml::Value, path: &[Segment]) -> ProjectKeyNotice {
    let key = rendered_key(path);
    let pointer = matches!(
        path,
        [Segment::Key(agents), Segment::Key(profiles)] if agents == "agents" && profiles == "profiles"
    );
    let (message, fix) = if pointer {
        (
            format!("project config ignores `{key}`, which is not a project table"),
            "declare project profiles as `[profiles.<name>]`",
        )
    } else if machine_reads(doc, path) {
        (
            format!("project config ignores machine-only key `{key}`"),
            "set it in ~/.rimz/config.toml",
        )
    } else {
        (
            format!("project config ignores unknown key `{key}`"),
            "remove or rename it",
        )
    };
    ProjectKeyNotice { key, message, fix }
}

/// `path` as a TOML dotted key naming the same key. A bare-key segment stays bare, and any other
/// is a basic string whose `"`, `\` and line-breaking characters (`crate::packs::breaks_line`)
/// are TOML escapes, so repository text cannot write to the terminal and each segment boundary
/// shows. An array index shows as its number.
fn rendered_key(path: &[Segment]) -> String {
    let segments: Vec<String> = path
        .iter()
        .map(|segment| match segment {
            Segment::Index(index) => index.to_string(),
            Segment::Key(key)
                if !key.is_empty()
                    && key.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
                    }) =>
            {
                key.clone()
            }
            Segment::Key(key) => quoted_key(key),
        })
        .collect();
    segments.join(".")
}

fn quoted_key(key: &str) -> String {
    let mut quoted = String::from('"');
    for character in key.chars() {
        if matches!(character, '"' | '\\') {
            quoted.push('\\');
            quoted.push(character);
        } else if crate::packs::breaks_line(character) {
            // Every line-breaking character sits below U+10000, so four digits hold it.
            quoted.push_str(&format!("\\u{:04X}", u32::from(character)));
        } else {
            quoted.push(character);
        }
    }
    quoted.push('"');
    quoted
}

/// Whether the per-machine config reads the key at `path`, judged by its own schema over a table
/// that holds that key alone. The schema skips a key it does not read before parsing its value,
/// so a parse error on the value still means the key is read. A key under an array never
/// qualifies: no machine table holds one there.
fn machine_reads(doc: &toml::Value, path: &[Segment]) -> bool {
    let Some(keys) = path
        .iter()
        .map(|segment| match segment {
            Segment::Key(key) => Some(key.clone()),
            Segment::Index(_) => None,
        })
        .collect::<Option<Vec<String>>>()
    else {
        return false;
    };
    let Some(value) = keys
        .iter()
        .try_fold(doc, |value, key| value.get(key.as_str()))
    else {
        return false;
    };
    let mut read = true;
    let _ = serde_ignored::deserialize::<_, _, CoreConfig>(holding(&keys, value), |ignored| {
        let ignored = key_path(&ignored);
        if ignored.len() <= keys.len()
            && ignored
                .iter()
                .zip(&keys)
                .all(|(ignored, key)| matches!(ignored, Segment::Key(ignored) if ignored == key))
        {
            read = false;
        }
    });
    read
}

#[cfg(test)]
mod tests;
