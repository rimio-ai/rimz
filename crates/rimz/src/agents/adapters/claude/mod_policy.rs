//! Claude's managed policies that keep RimZ's mod from loading, read from the
//! managed settings files Claude itself reads (`managed-settings.json`, then
//! each `managed-settings.d/*.json` in name order, a later file's value for a
//! key winning). A file that cannot be read or parsed sets nothing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::managed_pricing::{managed_settings_files, managed_settings_root};
use crate::agents::ModBlocker;

/// Each key that keeps a person's mod from loading, and whether it also turns
/// off settings hooks. The keys that do come first, so a policy that sets
/// several is reported by one that says the hooks are off too.
const BLOCKING_KEYS: [(&str, bool); 3] = [
    ("disableAllHooks", true),
    ("allowManagedHooksOnly", true),
    ("allowManagedModsOnly", false),
];

/// The first of [`BLOCKING_KEYS`] the managed settings set to `true`, with the
/// file whose value stands.
pub(super) fn blocker(login_env: &BTreeMap<String, String>) -> Option<ModBlocker> {
    blocker_in(&root(login_env))
}

fn blocker_in(root: &Path) -> Option<ModBlocker> {
    let mut values = BTreeMap::<&str, (bool, PathBuf)>::new();
    for path in managed_settings_files(root) {
        let Some(settings) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        else {
            continue;
        };
        for (key, _) in BLOCKING_KEYS {
            if let Some(value) = settings.get(key) {
                values.insert(key, (value == &Value::Bool(true), path.clone()));
            }
        }
    }
    BLOCKING_KEYS.into_iter().find_map(|(key, disables_hooks)| {
        let (true, file) = values.remove(key)? else {
            return None;
        };
        Some(ModBlocker {
            agent: "claude",
            key,
            file,
            disables_hooks,
        })
    })
}

/// Claude's managed settings directory. Test builds re-root it under the
/// machine root `RIMZ_TEST_GROK_POLICY_ROOT` names, as Grok's policy probe
/// does, so the suite never reads the host's own policy.
fn root(login_env: &BTreeMap<String, String>) -> PathBuf {
    let root = managed_settings_root();
    #[cfg(any(test, feature = "testkit"))]
    if let Some(machine) = login_env
        .get("RIMZ_TEST_GROK_POLICY_ROOT")
        .filter(|value| !value.is_empty())
    {
        return Path::new(machine).join(
            root.components()
                .filter(|part| matches!(part, std::path::Component::Normal(_)))
                .collect::<PathBuf>(),
        );
    }
    let _ = login_env;
    root
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, relative: &str, text: &str) -> PathBuf {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn each_blocking_key_names_itself_and_its_file() {
        for (key, disables_hooks) in BLOCKING_KEYS {
            let dir = tempfile::tempdir().unwrap();
            let file = write(
                dir.path(),
                "managed-settings.json",
                &format!(r#"{{"{key}": true, "modelPricing": {{}}}}"#),
            );
            let blocker = blocker_in(dir.path()).unwrap();
            assert_eq!((blocker.key, &blocker.file), (key, &file));
            assert_eq!(blocker.disables_hooks, disables_hooks);
            let message = blocker.to_string();
            assert!(message.contains(key) && message.contains(&file.display().to_string()));
            assert_eq!(
                message.contains("turns off RimZ's settings hooks"),
                disables_hooks,
                "{message}"
            );
        }
    }

    #[test]
    fn a_later_fragment_wins_and_unreadable_files_set_nothing() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "managed-settings.json",
            r#"{"allowManagedModsOnly": true}"#,
        );
        write(
            dir.path(),
            "managed-settings.d/20-lift.json",
            r#"{"allowManagedModsOnly": false}"#,
        );
        write(dir.path(), "managed-settings.d/30-broken.json", "{not json");
        write(
            dir.path(),
            "managed-settings.d/40-text.json",
            r#"{"disableAllHooks": "yes"}"#,
        );
        assert_eq!(blocker_in(dir.path()), None);

        let fragment = write(
            dir.path(),
            "managed-settings.d/50-hooks.json",
            r#"{"disableAllHooks": true}"#,
        );
        let blocker = blocker_in(dir.path()).unwrap();
        assert_eq!((blocker.key, blocker.file), ("disableAllHooks", fragment));
        assert_eq!(blocker_in(&dir.path().join("missing")), None);
    }

    #[test]
    fn a_hook_disabling_key_is_named_ahead_of_the_mods_only_key() {
        for hooks_key in ["allowManagedHooksOnly", "disableAllHooks"] {
            let one_file = tempfile::tempdir().unwrap();
            let file = write(
                one_file.path(),
                "managed-settings.json",
                &format!(r#"{{"allowManagedModsOnly": true, "{hooks_key}": true}}"#),
            );
            let blocker = blocker_in(one_file.path()).unwrap();
            assert_eq!((blocker.key, &blocker.file), (hooks_key, &file));
            assert!(blocker.disables_hooks);
            assert!(
                blocker
                    .to_string()
                    .contains("turns off RimZ's settings hooks")
            );

            let two_files = tempfile::tempdir().unwrap();
            write(
                two_files.path(),
                "managed-settings.json",
                r#"{"allowManagedModsOnly": true}"#,
            );
            let fragment = write(
                two_files.path(),
                "managed-settings.d/10-hooks.json",
                &format!(r#"{{"{hooks_key}": true}}"#),
            );
            let blocker = blocker_in(two_files.path()).unwrap();
            assert_eq!((blocker.key, blocker.file), (hooks_key, fragment));
            assert!(blocker.disables_hooks);
        }
    }
}
