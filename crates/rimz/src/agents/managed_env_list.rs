//! One RimZ-owned path inside a `:`-joined path-list variable of a provider
//! settings `env` block (Claude's `CLAUDE_CODE_PLUGIN_DIRS`).
//!
//! Ownership is the path itself: a list entry equal to RimZ's path is RimZ's,
//! so no marker rides beside the value. Install appends the path when the list
//! lacks it, keeping every other entry and its order; uninstall removes that
//! entry alone, the key once its list is empty, and the `env` object once it
//! holds nothing else.

use serde_json::{Map, Value};

const ENV_KEY: &str = "env";
const SEPARATOR: char = ':';

/// RimZ's path in one list variable of the settings `env` block.
pub(super) struct ManagedEnvPath<'a> {
    pub key: &'static str,
    pub path: &'a str,
}

impl ManagedEnvPath<'_> {
    /// Whether the settings already list RimZ's path.
    pub(super) fn is_listed(&self, root: &Map<String, Value>) -> bool {
        self.listed_value(root)
            .is_some_and(|list| list.split(SEPARATOR).any(|entry| entry == self.path))
    }

    /// Add RimZ's path to the list, creating the `env` object or the key when
    /// absent. An `env` that is not an object, or a key whose value is not a
    /// string, is the user's own shape: the error names it and nothing changes.
    pub(super) fn upsert(&self, root: &mut Map<String, Value>) -> Result<(), String> {
        let env = root
            .entry(ENV_KEY.to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        let Value::Object(env) = env else {
            return Err(format!(
                "settings `{ENV_KEY}` is not an object; cannot list `{}` in `{ENV_KEY}.{}`",
                self.path, self.key
            ));
        };
        let list = match env.get(self.key) {
            None => String::new(),
            Some(Value::String(list)) => list.clone(),
            Some(_) => {
                return Err(format!(
                    "settings `{ENV_KEY}.{}` is not a string; cannot list `{}` in it",
                    self.key, self.path
                ));
            }
        };
        if list.split(SEPARATOR).any(|entry| entry == self.path) {
            return Ok(());
        }
        let list = if list.is_empty() {
            self.path.to_owned()
        } else {
            format!("{list}{SEPARATOR}{}", self.path)
        };
        env.insert(self.key.to_owned(), Value::String(list));
        Ok(())
    }

    /// Remove RimZ's path from the list. A shape [`Self::upsert`] refuses holds
    /// no RimZ entry and stays as it is.
    pub(super) fn strip(&self, root: &mut Map<String, Value>) {
        let Some(list) = self.listed_value(root) else {
            return;
        };
        let kept: Vec<&str> = list
            .split(SEPARATOR)
            .filter(|entry| *entry != self.path)
            .collect();
        if kept.len() == list.split(SEPARATOR).count() {
            return;
        }
        let kept = kept.join(&SEPARATOR.to_string());
        let Some(Value::Object(env)) = root.get_mut(ENV_KEY) else {
            return;
        };
        if kept.is_empty() {
            env.remove(self.key);
        } else {
            env.insert(self.key.to_owned(), Value::String(kept));
        }
        if env.is_empty() {
            root.remove(ENV_KEY);
        }
    }

    fn listed_value<'r>(&self, root: &'r Map<String, Value>) -> Option<&'r str> {
        root.get(ENV_KEY)?.as_object()?.get(self.key)?.as_str()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ENTRY: ManagedEnvPath<'static> = ManagedEnvPath {
        key: "CLAUDE_CODE_PLUGIN_DIRS",
        path: "/home/u/.rimz/mods/claude",
    };

    fn object(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn install_then_uninstall_restores_each_starting_shape() {
        for before in [
            json!({"model": "opus"}),
            json!({"env": {"FOO": "bar"}}),
            json!({"env": {"CLAUDE_CODE_PLUGIN_DIRS": "/a:~/b", "FOO": "bar"}}),
        ] {
            let mut root = object(before.clone());
            ENTRY.upsert(&mut root).unwrap();
            assert!(ENTRY.is_listed(&root), "{before}");
            let installed = root.clone();
            ENTRY.upsert(&mut root).unwrap();
            assert_eq!(root, installed, "reinstall is a no-op: {before}");
            ENTRY.strip(&mut root);
            assert_eq!(Value::Object(root), before);
        }
    }

    #[test]
    fn install_appends_after_the_users_folders_in_order() {
        let mut root = object(json!({"env": {"CLAUDE_CODE_PLUGIN_DIRS": "/a:~/b"}}));
        ENTRY.upsert(&mut root).unwrap();
        assert_eq!(
            root["env"]["CLAUDE_CODE_PLUGIN_DIRS"],
            "/a:~/b:/home/u/.rimz/mods/claude"
        );
        let mut root = object(json!({}));
        ENTRY.upsert(&mut root).unwrap();
        assert_eq!(
            Value::Object(root),
            json!({"env": {"CLAUDE_CODE_PLUGIN_DIRS": "/home/u/.rimz/mods/claude"}})
        );
    }

    #[test]
    fn uninstall_keeps_a_users_list_that_never_held_rimz() {
        let before = json!({"env": {"CLAUDE_CODE_PLUGIN_DIRS": "/home/u/.rimz/mods/claude-other"}});
        let mut root = object(before.clone());
        assert!(!ENTRY.is_listed(&root));
        ENTRY.strip(&mut root);
        assert_eq!(Value::Object(root), before);
    }

    #[test]
    fn foreign_shapes_refuse_install_and_survive_uninstall() {
        for before in [
            json!({"env": "FOO=bar"}),
            json!({"env": {"CLAUDE_CODE_PLUGIN_DIRS": ["/a"]}}),
        ] {
            let mut root = object(before.clone());
            assert!(ENTRY.upsert(&mut root).is_err(), "{before}");
            assert_eq!(Value::Object(root.clone()), before);
            ENTRY.strip(&mut root);
            assert_eq!(Value::Object(root), before);
        }
    }
}
