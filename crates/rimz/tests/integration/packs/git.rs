use super::*;
use crate::common::git::{git, git_stdout};

struct Fixture {
    env: Env,
    config: PathBuf,
    crew: PathBuf,
    commit: String,
}

#[test]
fn the_empty_pack_catalog_teaches_the_official_install() {
    Env::new()
        .rimz()
        .args(["packs"])
        .assert()
        .success()
        .stdout(contains("rimz packs install crew"));
}

impl Fixture {
    fn new() -> Self {
        let env = Env::new();
        let sources = env.home_root.join("sources");
        let crew = sources.join("rimz-crew");
        let skills = sources.join("rimz-skills");
        write(
            crew.join("rimz-packs.toml"),
            "[[pack]]\nname = 'crew'\ndescription = 'Crew'\npath = '.'\nrequires = ['skills/essential']\n",
        );
        write(
            crew.join("agents/claude.md"),
            "---\ndescription: Base\n---\nBase.",
        );
        write(
            crew.join("agents/worker.md"),
            "---\ndescription: Worker\nagent: claude\nmode: auto\ntools: [Read, Write]\nallowed-tools: [Bash(git *)]\n---\nWork.",
        );
        write(crew.join("traits/craft.md"), "Craft.");
        write(
            skills.join("rimz-packs.toml"),
            "[[pack]]\nname = 'essential'\ndescription = 'Essential'\npath = '.'\n",
        );
        write(skills.join("skills/review/SKILL.md"), "# Review");
        for repo in [&crew, &skills] {
            git(repo, &["init", "-b", "main"]);
            git(repo, &["config", "user.name", "Pack Test"]);
            git(repo, &["config", "user.email", "packs@example.test"]);
            git(repo, &["config", "commit.gpgsign", "false"]);
            git(repo, &["add", "."]);
            git(repo, &["commit", "-m", "fixture"]);
            git(repo, &["tag", &format!("v{}", env!("CARGO_PKG_VERSION"))]);
        }
        let config = env.home_root.join("gitconfig");
        write(
            &config,
            &format!(
                "[url {:?}]\n insteadOf = https://github.com/rimio-ai/\n",
                url::Url::from_directory_path(&sources).unwrap().as_str()
            ),
        );
        let commit = git_stdout(&crew, &["rev-parse", "HEAD"]);
        Self {
            env,
            config,
            crew,
            commit,
        }
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        self.env
            .rimz()
            .env("GIT_CONFIG_GLOBAL", &self.config)
            .args(args)
            .output()
            .unwrap()
    }

    fn record(&self) -> Value {
        serde_json::from_slice(
            &fs::read(self.env.agents_home().join("packs/installed.json")).unwrap(),
        )
        .unwrap()
    }
}

#[test]
fn git_install_prints_the_review_then_refuses_without_a_terminal_or_yes() {
    let fixture = Fixture::new();
    let output = fixture.run(&["packs", "install", "crew"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    for text in [
        "crew",
        "essential",
        "worker",
        "mode: auto",
        "isolation:",
        "tools:",
        "allowed-tools:",
        "required by crew",
    ] {
        assert!(stdout.contains(text), "missing {text:?}: {stdout}");
    }
    assert!(stderr.contains("--yes"), "{stderr}");
    assert!(
        !fixture
            .env
            .agents_home()
            .join("packs/installed.json")
            .exists()
    );
}

#[test]
fn yes_installs_matching_official_dependencies_and_lists_full_version_identity() {
    let fixture = Fixture::new();
    let output = fixture.run(&["packs", "install", "crew", "--yes"]);
    assert!(output.status.success(), "{output:?}");
    let record = fixture.record();
    assert_eq!(record["sources"].as_array().unwrap().len(), 2, "{record}");
    for source in record["sources"].as_array().unwrap() {
        assert_eq!(source["ref"], format!("v{}", env!("CARGO_PKG_VERSION")));
        assert_eq!(source["pinned"], false);
        assert_eq!(source["linked"], false);
        assert!(
            fixture
                .env
                .agents_home()
                .join("packs")
                .join(source["entry"].as_str().unwrap())
                .is_symlink()
        );
    }
    let listing = fixture.run(&["packs", "list", "--json"]);
    assert!(listing.status.success(), "{listing:?}");
    let listing: Value = serde_json::from_slice(&listing.stdout).unwrap();
    let crew = listing["packs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|pack| pack["name"] == "crew")
        .unwrap();
    assert_eq!(crew["ref"], format!("v{}", env!("CARGO_PKG_VERSION")));
    assert_eq!(crew["pinned"], false);
    assert_eq!(crew["commit"], fixture.commit);
    let showing = fixture.run(&["packs", "show", "crew", "--json"]);
    assert!(showing.status.success(), "{showing:?}");
    let showing: Value = serde_json::from_slice(&showing.stdout).unwrap();
    for field in ["ref", "pinned", "commit"] {
        assert_eq!(showing[field], crew[field], "{showing}");
    }
    let human = fixture.run(&["packs", "show", "crew"]);
    let human = String::from_utf8_lossy(&human.stdout);
    assert!(
        human.contains(&format!("crew@v{}", env!("CARGO_PKG_VERSION")))
            && human.contains(&fixture.commit[..8]),
        "{human}"
    );
}

#[test]
fn pinned_official_dependencies_use_the_same_ref_and_missing_tags_name_the_escape() {
    let fixture = Fixture::new();
    let version = format!("v{}", env!("CARGO_PKG_VERSION"));
    let output = fixture.run(&["packs", "install", &format!("crew@{version}"), "--yes"]);
    assert!(output.status.success(), "{output:?}");
    let record = fixture.record();
    assert!(
        record["sources"]
            .as_array()
            .unwrap()
            .iter()
            .all(|source| source["pinned"] == true && source["ref"] == version),
        "{record}"
    );
    let fixture = Fixture::new();
    git(&fixture.crew, &["tag", "-d", &version]);
    let output = fixture.run(&["packs", "install", "crew", "--yes"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        stderr.contains(&version)
            && stderr.contains("https://github.com/rimio-ai/rimz-crew")
            && stderr.contains("rimz packs install crew@main"),
        "{stderr}"
    );
}

#[test]
fn official_commit_pins_leave_dependencies_on_their_own_following_rule() {
    let fixture = Fixture::new();
    let output = fixture.run(&[
        "packs",
        "install",
        &format!("crew@{}", fixture.commit),
        "--yes",
    ]);
    assert!(
        output.status.success(),
        "commit pins must install requirements: {output:?}"
    );
    let record = fixture.record();
    assert_eq!(record["sources"][0]["ref"], fixture.commit);
    assert_eq!(record["sources"][0]["pinned"], true);
    assert_eq!(
        record["sources"][1]["ref"],
        format!("v{}", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(record["sources"][1]["pinned"], false);
}

#[test]
fn official_branch_pins_share_the_branch_and_pin_with_dependencies() {
    let fixture = Fixture::new();
    let version = format!("v{}", env!("CARGO_PKG_VERSION"));
    let skills = fixture.crew.parent().unwrap().join("rimz-skills");
    for repo in [&fixture.crew, &skills] {
        git(repo, &["tag", "-d", &version]);
    }
    let output = fixture.run(&["packs", "install", "crew@main", "--yes"]);
    assert!(output.status.success(), "{output:?}");
    for source in fixture.record()["sources"].as_array().unwrap() {
        assert_eq!(source["ref"], "main");
        assert_eq!(source["pinned"], true);
    }
}

#[test]
fn an_unpinned_installed_parent_shares_its_older_named_ref() {
    let fixture = Fixture::new();
    let skills = fixture.crew.parent().unwrap().join("rimz-skills");
    write(
        fixture.crew.join("rimz-packs.toml"),
        "[[pack]]\nname = 'crew'\ndescription = 'Crew'\npath = '.'\n[[pack]]\nname = 'extra'\ndescription = 'Extra'\npath = 'extra'\nrequires = ['skills/essential']\n",
    );
    write(fixture.crew.join("extra/traits/extra.md"), "Extra.");
    git(&fixture.crew, &["add", "."]);
    git(&fixture.crew, &["commit", "-m", "older release"]);
    for repo in [&fixture.crew, &skills] {
        git(repo, &["tag", "older"]);
    }
    let output = fixture.run(&["packs", "install", "crew/crew@older", "--yes"]);
    assert!(output.status.success(), "{output:?}");
    let mut record = fixture.record();
    record["sources"][0]["pinned"] = false.into();
    write(
        fixture.env.agents_home().join("packs/installed.json"),
        &serde_json::to_string(&record).unwrap(),
    );
    let root = fixture.env.home_root.join("requester");
    write(
        root.join("rimz-packs.toml"),
        "[[pack]]\nname = 'requester'\ndescription = 'Requester'\npath = '.'\nrequires = ['crew/extra']\n",
    );
    crate::common::git::init_repo(&root);
    git(&root, &["add", "."]);
    git(&root, &["commit", "-m", "request"]);
    let output = fixture.run(&[
        "packs",
        "install",
        url::Url::from_directory_path(&root).unwrap().as_str(),
        "--yes",
    ]);
    assert!(output.status.success(), "{output:?}");
    let record = fixture.record();
    let required = record["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["source"] == "https://github.com/rimio-ai/rimz-skills")
        .unwrap();
    assert_eq!(required["ref"], "older");
    assert_eq!(required["pinned"], false);
}

#[test]
fn given_git_transport_wins_over_a_neighboring_repository_and_is_recorded() {
    let fixture = Fixture::new();
    let root = fixture.env.home_root.join("transport");
    for (directory, name) in [("source", "neighbor"), ("source.git", "requested")] {
        let repo = root.join(directory);
        write(
            repo.join("rimz-packs.toml"),
            &format!("[[pack]]\nname = '{name}'\ndescription = 'Transport'\npath = '.'\n"),
        );
        crate::common::git::init_repo(&repo);
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "pack"]);
    }
    let repo = root.join("source.git");
    let url = url::Url::from_directory_path(&repo).unwrap().to_string();
    let output = fixture.run(&["packs", "install", &url, "--yes"]);
    assert!(output.status.success(), "{output:?}");
    let record = fixture.record();
    assert_eq!(record["sources"][0]["packs"][0]["name"], "requested");
    assert_eq!(record["sources"][0]["url"], url);
    assert_eq!(
        record["sources"][0]["source"],
        url.trim_end_matches('/').strip_suffix(".git").unwrap()
    );
    let listing = fixture.run(&["packs", "list", "--json"]);
    assert!(listing.status.success(), "{listing:?}");
    let listing: Value = serde_json::from_slice(&listing.stdout).unwrap();
    assert_eq!(listing["packs"][0]["url"], url);
    let showing = fixture.run(&["packs", "show", "requested", "--json"]);
    assert!(showing.status.success(), "{showing:?}");
    let showing: Value = serde_json::from_slice(&showing.stdout).unwrap();
    assert_eq!(showing["url"], url);
    let alternate = url.trim_end_matches('/');
    let output = fixture.run(&["packs", "install", alternate, "--yes"]);
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("requested:"),
        "transport changes must appear in the review: {output:?}"
    );
    let record = fixture.record();
    assert_eq!(record["sources"].as_array().unwrap().len(), 1);
    assert_eq!(record["sources"][0]["url"], alternate);
}

#[test]
fn git_install_does_not_fetch_into_the_callers_git_dir() {
    let fixture = Fixture::new();
    crate::common::git::init_repo(&fixture.env.project_root);
    let before = git_stdout(&fixture.env.project_root, &["rev-parse", "HEAD"]);
    let output = fixture
        .env
        .rimz()
        .env("GIT_CONFIG_GLOBAL", &fixture.config)
        .env("GIT_DIR", fixture.env.project_root.join(".git"))
        .env("GIT_WORK_TREE", &fixture.env.project_root)
        .env(
            "GIT_INDEX_FILE",
            fixture.env.project_root.join("wrong-index"),
        )
        .env("GIT_CONFIG_COUNT", "not-a-number")
        .args(["packs", "install", "crew", "--yes"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        git_stdout(&fixture.env.project_root, &["rev-parse", "HEAD"]),
        before
    );
    assert!(!fixture.env.project_root.join(".git/FETCH_HEAD").exists());
    assert!(!fixture.env.project_root.join("wrong-index").exists());
    assert_eq!(fixture.record()["sources"][0]["commit"], fixture.commit);
}

fn answer(fixture: &Fixture, answer: &[u8]) -> (u32, String) {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::io::{Read as _, Write as _};
    use std::time::{Duration, Instant};
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 40,
            cols: 180,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut command = CommandBuilder::new(fixture.env.rimz_bin());
    fixture.env.pin_pty_command(&mut command);
    command.env("GIT_CONFIG_GLOBAL", &fixture.config);
    command.args(["packs", "install", "crew"]);
    command.cwd(&fixture.env.project_root);
    let mut child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        while let Ok(read) = reader.read(&mut buffer) {
            if read == 0 || tx.send(buffer[..read].to_vec()).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut bytes = Vec::new();
    while !String::from_utf8_lossy(&bytes).contains("Install these packs?") {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(chunk) => bytes.extend(chunk),
            Err(_) => {
                let _ = child.kill();
                assert!(
                    String::from_utf8_lossy(&bytes).contains("Install these packs?"),
                    "no install question: {}",
                    String::from_utf8_lossy(&bytes)
                );
            }
        }
    }
    pair.master
        .take_writer()
        .unwrap()
        .write_all(answer)
        .unwrap();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            assert!(
                Instant::now() < deadline,
                "install did not finish: {}",
                String::from_utf8_lossy(&bytes)
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    while let Ok(chunk) = rx.recv_timeout(Duration::from_secs(5)) {
        bytes.extend(chunk);
    }
    (
        status.exit_code(),
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

#[test]
fn interactive_decline_is_success_without_an_install_and_yes_commits() {
    let fixture = Fixture::new();
    let (code, output) = answer(&fixture, b"n\n");
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("nothing changed"), "{output}");
    assert!(
        !fixture
            .env
            .agents_home()
            .join("packs/installed.json")
            .exists()
    );
    let (code, output) = answer(&fixture, b"y\n");
    assert_eq!(code, 0, "{output}");
    assert_eq!(fixture.record()["sources"].as_array().unwrap().len(), 2);
}

#[test]
fn an_unchanged_git_install_still_summarizes_but_needs_no_confirmation() {
    let fixture = Fixture::new();
    let installed = fixture.run(&["packs", "install", "crew", "--yes"]);
    assert!(installed.status.success(), "{installed:?}");
    let output = fixture.run(&["packs", "install", "crew"]);
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("up to date") && stdout.contains("No changed profiles"),
        "{stdout}"
    );
}

#[test]
fn the_review_names_exclusions_overrides_conflicts_and_failed_items() {
    let fixture = Fixture::new();
    let installed = fixture.run(&["packs", "install", "crew", "--yes"]);
    assert!(installed.status.success(), "{installed:?}");
    packs::exclude(
        &fixture.env.agents_home(),
        &"crew".parse().unwrap(),
        &ExclusionTarget::Type(ItemType::Traits),
    )
    .unwrap();
    write(
        fixture.env.agents_home().join("agents/worker.md"),
        "---\ndescription: Home\nagent: claude\n---\nHome.",
    );
    write(
        fixture.crew.join("agents/broken.md"),
        "---\ndescription: Broken\nagent: absent\n---\nBroken.",
    );
    write(
        fixture.crew.join("agents/clash.md"),
        "---\ndescription: Clash\nagent: claude\n---\nClash.",
    );
    write(
        fixture.crew.join("extra/agents/clash.md"),
        "---\ndescription: Clash\nagent: claude\n---\nClash.",
    );
    let mut manifest = fs::read_to_string(fixture.crew.join("rimz-packs.toml")).unwrap();
    manifest.push_str("[[pack]]\nname = 'extra'\ndescription = 'Extra'\npath = 'extra'\n");
    write(fixture.crew.join("rimz-packs.toml"), &manifest);
    git(&fixture.crew, &["add", "."]);
    git(&fixture.crew, &["commit", "-m", "review fates"]);
    git(
        &fixture.crew,
        &["tag", "-f", &format!("v{}", env!("CARGO_PKG_VERSION"))],
    );
    let before = fixture.record();
    let output = fixture.run(&["packs", "install", "crew"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    for fate in [
        "excluded (type)",
        "overridden by",
        "conflicts with",
        "failed:",
    ] {
        assert!(stdout.contains(fate), "missing {fate}: {stdout}");
    }
    assert_eq!(fixture.record(), before);
}
