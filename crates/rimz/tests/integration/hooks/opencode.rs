use super::*;

const SERVER: &str = include_str!("../../../src/agents/adapters/opencode/index.ts");
const CLIENT: &str = include_str!("../../../src/agents/adapters/opencode/tui.ts");
const STALE: &str = "// _rimz_managed\n// old source\n";

fn command(env: &Env) -> Command {
    let mut command = env.rimz();
    command
        .env("XDG_CONFIG_HOME", env.home_root.join("config"))
        .env("PATH", env.home_root.join("empty-bin"));
    command
}

fn install(env: &Env) -> Output {
    command(env)
        .args(["hooks", "install", "opencode"])
        .output()
        .unwrap()
}

fn doctor_hook_state(env: &Env) -> String {
    let binaries = fake_agent_bin_dir(&["opencode"]);
    let output = command(env)
        .env("PATH", binaries.path())
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    report["hooks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["kind"] == "opencode")
        .unwrap()["status"]["state"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn bundle_migrates_repairs_and_uninstalls_only_owned_files() {
    let env = Env::new();
    let bundle = env.home_root.join("config/opencode/plugins/rimz");
    let legacy = env.home_root.join("config/opencode/plugin/rimz.ts");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, STALE).unwrap();
    assert_eq!(doctor_hook_state(&env), "not_installed");
    let preview = command(&env)
        .args(["hooks", "install", "--dry-run", "opencode"])
        .output()
        .unwrap();
    let rendered = String::from_utf8_lossy(&preview.stdout);
    assert!(preview.status.success(), "{preview:?}");
    assert!(rendered.contains("tui.ts"), "bundle preview: {rendered}");
    assert!(
        rendered.contains("removes file"),
        "migration preview: {rendered}"
    );
    assert!(!bundle.exists());
    let result = install(&env);
    assert!(result.status.success(), "{result:?}");
    assert!(String::from_utf8_lossy(&result.stdout).contains("installed"));
    assert!(!legacy.exists());
    for (name, source) in [("index.ts", SERVER), ("tui.ts", CLIENT)] {
        assert_eq!(std::fs::read_to_string(bundle.join(name)).unwrap(), source);
    }
    assert_eq!(doctor_hook_state(&env), "installed");
    let before = std::fs::metadata(bundle.join("index.ts"))
        .unwrap()
        .modified()
        .unwrap();
    let result = install(&env);
    assert!(result.status.success());
    assert!(String::from_utf8_lossy(&result.stdout).contains("hooks up to date"));
    assert_eq!(
        std::fs::metadata(bundle.join("index.ts"))
            .unwrap()
            .modified()
            .unwrap(),
        before
    );
    for name in ["index.ts", "tui.ts"] {
        std::fs::write(bundle.join(name), STALE).unwrap();
        assert_eq!(doctor_hook_state(&env), "installed");
        let result = install(&env);
        assert!(result.status.success());
        assert!(String::from_utf8_lossy(&result.stdout).contains("refreshed"));
        std::fs::remove_file(bundle.join(name)).unwrap();
        assert_eq!(doctor_hook_state(&env), "not_installed");
        let result = command(&env).args(["hooks", "uninstall"]).output().unwrap();
        assert!(result.status.success(), "{result:?}");
        assert!(
            !bundle.exists(),
            "partial owned artifacts must be found without a binary"
        );
        assert!(install(&env).status.success());
    }
    std::fs::write(bundle.join("foreign.txt"), "mine").unwrap();
    let result = command(&env)
        .args(["hooks", "uninstall", "opencode"])
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    assert!(!bundle.join("index.ts").exists());
    assert!(!bundle.join("tui.ts").exists());
    assert_eq!(
        std::fs::read_to_string(bundle.join("foreign.txt")).unwrap(),
        "mine"
    );
    std::fs::remove_file(bundle.join("foreign.txt")).unwrap();
    assert!(install(&env).status.success());
    assert!(
        command(&env)
            .args(["hooks", "uninstall", "opencode"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(!bundle.exists());
}

#[test]
#[cfg(unix)]
fn bundle_preflights_all_targets_before_writing() {
    use std::os::unix::fs::symlink;

    let mut failures = Vec::new();
    for case in [
        "legacy",
        "index.ts",
        "tui.ts",
        "bundle-link",
        "target-link",
        "file-override",
    ] {
        let env = Env::new();
        let bundle = env.home_root.join("config/opencode/plugins/rimz");
        let legacy = env.home_root.join("config/opencode/plugin/rimz.ts");
        std::fs::create_dir_all(&bundle).unwrap();
        let outside = env.home_root.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        match case {
            "legacy" => {
                std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
                std::fs::write(&legacy, "// mine\n").unwrap();
            }
            "index.ts" | "tui.ts" => std::fs::write(bundle.join(case), "// mine\n").unwrap(),
            "bundle-link" => {
                std::fs::remove_dir(&bundle).unwrap();
                symlink(&outside, &bundle).unwrap();
            }
            "target-link" => {
                std::fs::write(outside.join("source.ts"), STALE).unwrap();
                symlink(outside.join("source.ts"), bundle.join("tui.ts")).unwrap();
            }
            "file-override" => std::fs::write(outside.join("file.ts"), STALE).unwrap(),
            _ => unreachable!(),
        }
        let mut cmd = command(&env);
        cmd.args(["hooks", "install", "opencode"]);
        if case == "file-override" {
            cmd.env("RIMZ_OPENCODE_PLUGIN", outside.join("file.ts"));
        }
        let result = cmd.output().unwrap();
        if result.status.success() || bundle.join("index.ts").exists() && case != "index.ts" {
            failures.push(format!("{case}: must refuse before any write: {result:?}"));
        }
        if case == "legacy" {
            assert_eq!(std::fs::read_to_string(&legacy).unwrap(), "// mine\n");
            assert!(String::from_utf8_lossy(&result.stderr).contains("move it aside"));
        }
        if matches!(case, "index.ts" | "tui.ts") {
            assert_eq!(
                std::fs::read_to_string(bundle.join(case)).unwrap(),
                "// mine\n"
            );
        }
        if case == "target-link" {
            assert_eq!(
                std::fs::read_to_string(outside.join("source.ts")).unwrap(),
                STALE
            );
            assert!(bundle.join("tui.ts").is_symlink());
        }
        if case == "file-override" {
            assert_eq!(
                std::fs::read_to_string(outside.join("file.ts")).unwrap(),
                STALE
            );
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn bundle_override_never_writes_or_removes_the_default_install() {
    let env = Env::new();
    let override_dir = env.home_root.join("custom/rimz");
    let legacy = env.home_root.join("config/opencode/plugin/rimz.ts");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, STALE).unwrap();
    let result = command(&env)
        .env("RIMZ_OPENCODE_PLUGIN", &override_dir)
        .args(["hooks", "install", "opencode"])
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    assert!(
        override_dir.join("index.ts").is_file(),
        "override must name a bundle directory"
    );
    assert!(override_dir.join("tui.ts").is_file());
    assert_eq!(std::fs::read_to_string(&legacy).unwrap(), STALE);
    assert!(!env.home_root.join("config/opencode/plugins").exists());
    let result = command(&env)
        .env("RIMZ_OPENCODE_PLUGIN", &override_dir)
        .args(["hooks", "uninstall", "opencode"])
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    assert!(!override_dir.exists());
    assert!(legacy.exists());
}
