use std::collections::BTreeMap;
use std::fmt;

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Serialize};

/// Agent-launch preferences. Layout entries name registry-backed agent kinds or
/// `term`; the parser lives in [`crate::tab_layout`].
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct AgentsConfig {
    pub layouts: LayoutsConfig,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct LayoutsConfig(pub BTreeMap<String, LayoutEntry>);

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub enum LayoutEntry {
    Shape(String),
    Detailed {
        shape: String,
        flags: BTreeMap<String, String>,
    },
}

impl<'de> Deserialize<'de> for LayoutEntry {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(LayoutEntryVisitor)
    }
}

struct LayoutEntryVisitor;

impl<'de> Visitor<'de> for LayoutEntryVisitor {
    type Value = LayoutEntry;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a layout shape string or a table with `shape`")
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(LayoutEntry::Shape(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(LayoutEntry::Shape(value))
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut shape = None;
        let mut flags = None;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "shape" => {
                    if shape.is_some() {
                        return Err(de::Error::duplicate_field("shape"));
                    }
                    shape = Some(map.next_value()?);
                }
                "flags" => {
                    if flags.is_some() {
                        return Err(de::Error::duplicate_field("flags"));
                    }
                    flags = Some(map.next_value()?);
                }
                _ => {
                    let _ = map.next_value::<de::IgnoredAny>()?;
                }
            }
        }
        let shape = shape.ok_or_else(|| de::Error::missing_field("shape"))?;
        Ok(LayoutEntry::Detailed {
            shape,
            flags: flags.unwrap_or_default(),
        })
    }
}

impl LayoutEntry {
    pub fn shape(&self) -> &str {
        match self {
            Self::Shape(shape) => shape,
            Self::Detailed { shape, .. } => shape,
        }
    }

    pub fn flags(&self) -> Option<&BTreeMap<String, String>> {
        match self {
            Self::Shape(_) => None,
            Self::Detailed { flags, .. } => Some(flags),
        }
    }
}
