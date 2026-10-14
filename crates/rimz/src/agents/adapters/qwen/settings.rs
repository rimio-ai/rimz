//! Qwen settings layers and launch-time hook availability.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::agents::capabilities::LaunchBlocker;

pub(super) struct SettingsLayer {
    path: PathBuf,
    pub(super) value: Value,
    workspace: bool,
}

pub(super) fn layers(cwd: Option<&Path>, env: &BTreeMap<String, String>) -> Vec<SettingsLayer> {
    let home = env
        .get("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(crate::agents::transcript_fs::home_dir);
    let resolve = |path: PathBuf| {
        let path = path
            .strip_prefix("~")
            .map_or_else(|_| path.clone(), |relative| home.join(relative));
        match cwd {
            Some(cwd) if path.is_relative() => cwd.join(path),
            _ => path,
        }
    };
    let system = resolve(
        env.get("QWEN_CODE_SYSTEM_SETTINGS_PATH")
            .filter(|value| !value.is_empty())
            .map_or_else(
                || {
                    PathBuf::from(if cfg!(target_os = "macos") {
                        "/Library/Application Support/QwenCode/settings.json"
                    } else {
                        "/etc/qwen-code/settings.json"
                    })
                },
                PathBuf::from,
            ),
    );
    let defaults = env
        .get("QWEN_CODE_SYSTEM_DEFAULTS_PATH")
        .filter(|value| !value.is_empty())
        .map_or_else(
            || system.with_file_name("system-defaults.json"),
            |path| resolve(PathBuf::from(path)),
        );
    let mut paths = vec![(defaults, false)];
    if let Some(user) = super::install::qwen_home_from(
        env.get("QWEN_HOME").map(std::ffi::OsStr::new),
        Some(home.as_os_str()),
    ) {
        paths.push((resolve(user).join("settings.json"), false));
    }
    if let Some(cwd) = cwd
        && cwd != home.as_path()
    {
        paths.push((cwd.join(".qwen/settings.json"), true));
    }
    paths.push((system, false));
    paths
        .into_iter()
        .filter_map(|(path, workspace)| {
            let bytes = std::fs::read(&path).ok()?;
            let value = crate::agents::jsonc::from_slice(&bytes).ok()?;
            Some(SettingsLayer {
                path,
                value,
                workspace,
            })
        })
        .collect()
}

pub(super) fn hook_blocker(
    cwd: &Path,
    env: &BTreeMap<String, String>,
    argv: &[String],
) -> Option<LaunchBlocker> {
    let layers = layers(Some(cwd), env);
    for layer in layers.iter().rev() {
        if !layer.workspace
            && layer
                .value
                .pointer("/tools/executionSandbox")
                .is_some_and(Value::is_object)
        {
            let source = layer.path.display().to_string();
            return Some(refusal(
                "tools.executionSandbox",
                &source,
                &format!(
                    "this is operator policy: the operator must remove it from {source}, or run Qwen outside RimZ"
                ),
            ));
        }
    }
    if let Some(layer) = layers
        .iter()
        .rev()
        .find(|layer| layer.value.get("disableAllHooks").is_some())
        && layer.value.get("disableAllHooks").and_then(Value::as_bool) == Some(true)
    {
        let source = layer.path.display().to_string();
        return Some(refusal(
            "disableAllHooks",
            &source,
            &format!("remove disableAllHooks or set it false in {source}"),
        ));
    }
    let safe_mode = boolean_flag(argv, "--safe-mode", "--no-safe-mode");
    if safe_mode == Some(true) {
        return Some(refusal(
            "--safe-mode",
            "launch args",
            "remove --safe-mode from the profile args",
        ));
    }
    if boolean_flag(argv, "--bare", "--no-bare") == Some(true) {
        return Some(refusal(
            "--bare",
            "launch args",
            "remove --bare from the profile args",
        ));
    }
    for key in ["QWEN_CODE_SAFE_MODE", "QWEN_CODE_SIMPLE"] {
        if key == "QWEN_CODE_SAFE_MODE" && safe_mode == Some(false) {
            continue;
        }
        if env.get(key).is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        }) {
            return Some(refusal(key, "environment", &format!("unset {key}")));
        }
    }
    None
}

fn refusal(setting: &str, source: &str, fix: &str) -> LaunchBlocker {
    LaunchBlocker(format!(
        "Qwen hooks are disabled by {setting} in {source}; RimZ tracks Qwen only through its hooks, so the card would never show a turn, a permission prompt, or a question; {fix}"
    ))
}

fn boolean_flag(argv: &[String], flag: &str, negated: &str) -> Option<bool> {
    let mut value = None;
    for (index, arg) in argv
        .iter()
        .take_while(|arg| arg.as_str() != "--")
        .enumerate()
    {
        if arg == negated {
            value = Some(false);
        } else if arg == flag {
            value = Some(argv.get(index + 1).is_none_or(|next| next != "false"));
        } else if let Some(raw) = arg
            .strip_prefix(flag)
            .and_then(|suffix| suffix.strip_prefix('='))
        {
            value = Some(raw != "false");
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use serde_json::{Value, json};

    use crate::agents::capabilities::LaunchCapability;

    struct Fixture {
        dir: tempfile::TempDir,
        env: BTreeMap<String, String>,
        cwd: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let env = [
                ("HOME", "home"),
                ("QWEN_HOME", "user"),
                ("QWEN_CODE_SYSTEM_SETTINGS_PATH", "system/settings.json"),
            ]
            .into_iter()
            .map(|(key, path)| (key.to_owned(), dir.path().join(path).display().to_string()))
            .collect();
            let cwd = dir.path().join("workspace");
            Self { dir, env, cwd }
        }

        fn write(&self, relative: &str, value: Value) -> PathBuf {
            let path = self.dir.path().join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            path
        }

        fn refusal(&self, args: &[&str]) -> Option<String> {
            super::super::QwenAdapter
                .launch_blocker(
                    &self.cwd,
                    &self.env,
                    &args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
                )
                .map(|error| error.to_string())
        }

        fn assert_file_refusal(&self, path: &Path, key: &str, fix: &str) {
            let message = self
                .refusal(&[])
                .expect("hooks-disabled launch must refuse");
            assert!(message.contains(key), "{message}");
            assert!(message.contains(&path.display().to_string()), "{message}");
            assert!(message.contains(fix), "{message}");
            assert!(
                message.contains("turn, a permission prompt, or a question"),
                "{message}"
            );
        }
    }

    #[test]
    fn user_execution_sandbox_refuses() {
        let fixture = Fixture::new();
        let path = fixture.write(
            "user/settings.json",
            json!({"tools":{"executionSandbox":{}}}),
        );
        fixture.assert_file_refusal(&path, "tools.executionSandbox", "operator");
    }

    #[test]
    fn system_execution_sandbox_refuses() {
        let fixture = Fixture::new();
        let path = fixture.write(
            "system/settings.json",
            json!({"tools":{"executionSandbox":{}}}),
        );
        fixture.assert_file_refusal(&path, "tools.executionSandbox", "outside RimZ");
    }

    #[test]
    fn execution_sandbox_refusal_names_the_winning_layer() {
        let fixture = Fixture::new();
        fixture.write(
            "user/settings.json",
            json!({"tools":{"executionSandbox":{}}}),
        );
        let path = fixture.write(
            "system/settings.json",
            json!({"tools":{"executionSandbox":{}}}),
        );
        fixture.assert_file_refusal(&path, "tools.executionSandbox", "operator");
    }

    #[test]
    fn defaults_execution_sandbox_refuses() {
        let fixture = Fixture::new();
        let path = fixture.write(
            "system/system-defaults.json",
            json!({"tools":{"executionSandbox":{}}}),
        );
        fixture.assert_file_refusal(&path, "tools.executionSandbox", "operator");
    }

    #[test]
    fn workspace_execution_sandbox_does_not_refuse() {
        let fixture = Fixture::new();
        fixture.write(
            "workspace/.qwen/settings.json",
            json!({"tools":{"executionSandbox":{}}}),
        );
        assert!(fixture.refusal(&[]).is_none());
    }

    #[test]
    fn workspace_disable_all_hooks_refuses() {
        let fixture = Fixture::new();
        let path = fixture.write(
            "workspace/.qwen/settings.json",
            json!({"disableAllHooks":true}),
        );
        fixture.assert_file_refusal(&path, "disableAllHooks", "false");
    }

    #[test]
    fn workspace_false_overrides_user_true() {
        let fixture = Fixture::new();
        fixture.write("user/settings.json", json!({"disableAllHooks":true}));
        fixture.write(
            "workspace/.qwen/settings.json",
            json!({"disableAllHooks":false}),
        );
        assert!(fixture.refusal(&[]).is_none());
    }

    #[test]
    fn system_true_overrides_workspace_false() {
        let fixture = Fixture::new();
        fixture.write(
            "workspace/.qwen/settings.json",
            json!({"disableAllHooks":false}),
        );
        let path = fixture.write("system/settings.json", json!({"disableAllHooks":true}));
        fixture.assert_file_refusal(&path, "disableAllHooks", "false");
    }

    #[test]
    fn safe_mode_flag_refuses() {
        let fixture = Fixture::new();
        let message = fixture
            .refusal(&["qwen", "--safe-mode"])
            .expect("safe mode must refuse");
        assert!(message.contains("--safe-mode"));
        assert!(message.contains("launch args"));
        assert!(message.contains("remove"));
    }

    #[test]
    fn bare_flag_refuses() {
        let fixture = Fixture::new();
        let message = fixture
            .refusal(&["qwen", "--bare"])
            .expect("bare mode must refuse");
        assert!(message.contains("--bare"));
        assert!(message.contains("launch args"));
    }

    #[test]
    fn safe_mode_environment_refuses() {
        let mut fixture = Fixture::new();
        fixture
            .env
            .insert("QWEN_CODE_SAFE_MODE".to_owned(), " yes ".to_owned());
        let message = fixture
            .refusal(&[])
            .expect("safe-mode environment must refuse");
        assert!(message.contains("QWEN_CODE_SAFE_MODE"));
        assert!(message.contains("environment"));
        assert!(message.contains("unset"));
    }

    #[test]
    fn simple_environment_refuses_even_with_false_bare_flag() {
        let mut fixture = Fixture::new();
        fixture
            .env
            .insert("QWEN_CODE_SIMPLE".to_owned(), "1".to_owned());
        for args in [&[][..], &["--bare=false"][..]] {
            let message = fixture
                .refusal(args)
                .expect("simple-mode environment must refuse");
            assert!(message.contains("QWEN_CODE_SIMPLE"));
            assert!(message.contains("unset"));
        }
    }

    #[test]
    fn no_safe_mode_suppresses_environment() {
        let mut fixture = Fixture::new();
        fixture
            .env
            .insert("QWEN_CODE_SAFE_MODE".to_owned(), "1".to_owned());
        for args in [
            ["--no-safe-mode"].as_slice(),
            ["--safe-mode=false"].as_slice(),
        ] {
            assert!(fixture.refusal(args).is_none());
        }
    }

    #[test]
    fn clean_config_passes() {
        assert!(Fixture::new().refusal(&[]).is_none());
    }

    #[test]
    fn home_directory_is_not_a_workspace_layer() {
        let mut fixture = Fixture::new();
        fixture.cwd = fixture.dir.path().join("home");
        fixture.write("home/.qwen/settings.json", json!({"disableAllHooks":true}));
        assert!(fixture.refusal(&[]).is_none());
    }

    #[test]
    fn malformed_settings_abstain() {
        let fixture = Fixture::new();
        let path = fixture.write("user/settings.json", json!({}));
        std::fs::write(path, "{broken").unwrap();
        assert!(fixture.refusal(&[]).is_none());
    }

    #[test]
    fn explicit_system_defaults_override_refuses() {
        let mut fixture = Fixture::new();
        let path = fixture.write("defaults.json", json!({"disableAllHooks":true}));
        fixture.env.insert(
            "QWEN_CODE_SYSTEM_DEFAULTS_PATH".to_owned(),
            path.display().to_string(),
        );
        fixture.assert_file_refusal(&path, "disableAllHooks", "false");
    }
}
