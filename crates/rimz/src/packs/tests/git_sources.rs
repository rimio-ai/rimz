use super::*;
use crate::packs::git as pack_git;
use std::process::Command;

pub(super) fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "user.name=Pack Test",
            "-c",
            "user.email=packs@example.test",
        ])
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn repository(root: &Path) -> (String, String) {
    fs::create_dir_all(root).unwrap();
    git(root, &["init", "-b", "main"]);
    write(&root.join("contents"), "first");
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "first"]);
    let first = git(root, &["rev-parse", "HEAD"]);
    git(root, &["tag", "-a", "v1", "-m", "version one"]);
    git(root, &["tag", "clash"]);
    write(&root.join("contents"), "second");
    git(root, &["commit", "-am", "second"]);
    git(root, &["branch", "clash"]);
    (first, git(root, &["rev-parse", "HEAD"]))
}

fn spec(url: String, reference: Option<&str>) -> GitSource {
    GitSource {
        source: url.trim_end_matches('/').to_owned(),
        url,
        pack: None,
        reference: reference.map(str::to_owned),
    }
}

#[test]
fn source_spellings_parse_identity_selection_and_refs() {
    let cases = [
        ("crew", "https://github.com/rimio-ai/rimz-crew", None, None),
        (
            "skills/git@release/v1",
            "https://github.com/rimio-ai/rimz-skills",
            Some("git"),
            Some("release/v1"),
        ),
        ("github:o/r", "https://github.com/o/r", None, None),
        (
            "github:o/r/one@v1",
            "https://github.com/o/r",
            Some("one"),
            Some("v1"),
        ),
        (
            "https://github.com/o/r.git/",
            "https://github.com/o/r",
            None,
            None,
        ),
        (
            "https://user@example.test/o/r.git@main",
            "https://user@example.test/o/r",
            None,
            Some("main"),
        ),
        (
            "file:///tmp/repo@release/v1",
            "file:///tmp/repo",
            None,
            Some("release/v1"),
        ),
        (
            "git@example.test:o/r.git@v1",
            "git@example.test:o/r",
            None,
            Some("v1"),
        ),
        ("example.test:o/r", "example.test:o/r", None, None),
    ];
    let actual: Vec<_> = cases
        .iter()
        .map(|(text, _, _, _)| match text.parse::<InstallSource>() {
            Ok(InstallSource::Git(source)) => Some((
                source.source,
                source.pack.map(|name| name.to_string()),
                source.reference,
            )),
            _ => None,
        })
        .collect();
    let expected: Vec<_> = cases
        .iter()
        .map(|(_, url, pack, reference)| {
            Some((
                url.to_string(),
                pack.map(str::to_owned),
                reference.map(str::to_owned),
            ))
        })
        .collect();
    assert_eq!(actual, expected);
    let identities: Vec<_> = [
        "crew",
        "github:rimio-ai/rimz-crew",
        "https://github.com/rimio-ai/rimz-crew.git",
    ]
    .map(|text| match text.parse::<InstallSource>() {
        Ok(InstallSource::Git(source)) => Some(source.source),
        _ => None,
    })
    .into_iter()
    .collect();
    assert_eq!(
        identities,
        vec![Some("https://github.com/rimio-ai/rimz-crew".to_owned()); 3]
    );
}

#[test]
fn malformed_sources_name_the_accepted_forms() {
    let cases = [
        "crew/",
        "github:o",
        "x@",
        "crew@",
        "github:o/r/one/two",
        "https://github.com/o/r/pack",
        "https://example.test/r.git/pack",
        "ext::echo bad",
    ];
    let results: Vec<_> = cases
        .iter()
        .map(|text| {
            let error = text
                .parse::<InstallSource>()
                .err()
                .map(|error| error.to_string());
            (
                text,
                error.as_ref().is_some_and(|error| {
                    error.contains("./") && error.contains("github:") && error.contains("crew")
                }),
                error,
            )
        })
        .collect();
    assert!(results.iter().all(|(_, valid, _)| *valid), "{results:?}");
}

#[test]
fn source_transport_keeps_the_given_url() {
    for text in [
        "file:///tmp/source.git",
        "https://example.test/source.git/",
        "git@example.test:source.git",
    ] {
        let InstallSource::Git(source) = text.parse().unwrap() else {
            panic!("expected Git source");
        };
        assert_eq!(source.url, text, "transport must not be normalized");
    }
}

#[test]
fn source_identity_strips_only_one_git_suffix() {
    let InstallSource::Git(source) = "github:o/double.git.git".parse().unwrap() else {
        panic!("expected Git source");
    };
    assert_eq!(source.source, "https://github.com/o/double.git");
}

#[test]
fn reinstall_spellings_preserve_the_normalized_identity() {
    for text in [
        "github:o/double.git.git",
        "https://github.com/o/double.git.git",
        "https://github.com/o/r.git/",
        "https://example.test/double.git.git/",
        "git@example.test:double.git.git",
    ] {
        let source = crate::packs::source::parse(text).unwrap();
        let spelling = source.spelling();
        let reparsed = crate::packs::source::parse(&spelling).unwrap();
        assert_eq!(reparsed.source, source.source, "{text} became {spelling}");
    }
}

#[test]
fn default_git_command_bound_allows_a_slow_fetch() {
    assert_eq!(
        pack_git::Client::default().timeout,
        std::time::Duration::from_secs(120)
    );
}

#[test]
fn git_resolves_tags_before_branches_full_commits_and_the_default_branch() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let (first, second) = repository(&repo);
    let client = pack_git::Client::default();
    let url = url::Url::from_directory_path(&repo).unwrap().to_string();
    let cases = [
        (Some("v1"), "v1", first.as_str()),
        (Some("main"), "main", second.as_str()),
        (Some("clash"), "clash", first.as_str()),
        (Some(first.as_str()), first.as_str(), first.as_str()),
        (None, "main", second.as_str()),
    ];
    let actual: Vec<_> = cases
        .iter()
        .map(|(reference, _, _)| {
            client
                .resolve(temp.path(), &spec(url.clone(), *reference))
                .map(|revision| (revision.reference, revision.commit))
                .map_err(|error| error.to_string())
        })
        .collect();
    let expected: Vec<_> = cases
        .iter()
        .map(|(_, reference, commit)| Ok((reference.to_string(), commit.to_string())))
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn git_checkout_is_exact_and_does_not_inherit_the_callers_repository() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let (first, second) = repository(&repo);
    let checkout = temp.path().join("checkout");
    let source = spec(
        url::Url::from_directory_path(&repo).unwrap().to_string(),
        None,
    );
    let mut client = pack_git::Client::default();
    for key in git(&repo, &["rev-parse", "--local-env-vars"]).lines() {
        client
            .env
            .insert(key.to_owned(), "must-be-removed".to_owned());
    }
    let result = client.checkout(
        &checkout,
        &source,
        &pack_git::Revision {
            reference: "v1".to_owned(),
            commit: first.clone(),
        },
    );
    assert!(
        result.is_ok()
            && fs::read_to_string(checkout.join("contents"))
                .ok()
                .as_deref()
                == Some("first"),
        "checkout must be isolated and contain the requested commit: {result:?}"
    );
    assert_eq!(git(&checkout, &["rev-parse", "HEAD"]), first);
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]), second);
    assert!(checkout.join(".git").is_dir());
}

#[test]
fn missing_official_tag_names_the_source_tag_and_default_branch_command() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    repository(&repo);
    let config = temp.path().join("gitconfig");
    write(
        &config,
        &format!(
            "[url {:?}]\n insteadOf = https://github.com/rimio-ai/rimz-crew\n",
            url::Url::from_directory_path(&repo).unwrap().as_str()
        ),
    );
    let mut client = pack_git::Client::default();
    client.env.insert(
        "GIT_CONFIG_GLOBAL".to_owned(),
        config.to_str().unwrap().to_owned(),
    );
    let result = client.resolve(
        temp.path(),
        &spec("https://github.com/rimio-ai/rimz-crew".to_owned(), None),
    );
    let error = result
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
    assert!(
        error.contains(&format!("v{}", env!("CARGO_PKG_VERSION")))
            && error.contains("https://github.com/rimio-ai/rimz-crew")
            && error.contains("rimz packs install crew@main"),
        "{error}"
    );
    git(&repo, &["branch", "-m", "topic;echo-injected"]);
    let error = client
        .resolve(
            temp.path(),
            &spec("https://github.com/rimio-ai/rimz-crew".to_owned(), None),
        )
        .unwrap_err()
        .to_string();
    let command = error
        .split_once("use `")
        .and_then(|(_, rest)| rest.split_once('`'))
        .map(|(command, _)| command)
        .unwrap_or_default();
    let output = Command::new("sh")
        .args([
            "-c",
            &format!("rimz() {{ printf '%s\\n' \"$@\"; }}; {command}"),
        ])
        .output()
        .unwrap();
    assert_eq!(
        output.stdout, b"packs\ninstall\ncrew@topic;echo-injected\n",
        "the suggested command must preserve the branch as one argument: {output:?}"
    );
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn git_network_is_noninteractive_and_bounded() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let ssh = temp.path().join("ssh");
    write(&ssh, "#!/bin/sh\nexec sleep 10\n");
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
    let mut client = pack_git::Client {
        timeout: std::time::Duration::from_millis(50),
        ..Default::default()
    };
    client.env.insert(
        "GIT_SSH_COMMAND".to_owned(),
        ssh.to_str().unwrap().to_owned(),
    );
    let result = client.resolve(
        temp.path(),
        &spec("ssh://example.test/repo".to_owned(), None),
    );
    let error = result
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
    assert!(
        error.contains("timed out") && error.contains("50ms"),
        "{error}"
    );
}

#[test]
fn git_children_force_c_locale_disable_prompts_and_receive_no_stdin() {
    let temp = tempfile::tempdir().unwrap();
    let mut client = pack_git::Client::default();
    client
        .env
        .insert("GIT_TERMINAL_PROMPT".to_owned(), "1".to_owned());
    client
        .env
        .insert("LC_ALL".to_owned(), "not-a-locale".to_owned());
    let result = client.run(temp.path(), &["-c", "alias.probe=!printf '%s\\n' \"$GIT_TERMINAL_PROMPT\" \"$LC_ALL\"; if read value; then printf input; else printf eof; fi", "probe"], "fixture", false);
    assert_eq!(
        result.map_err(|error| error.to_string()),
        Ok("0\nC\neof".to_owned())
    );
}

#[test]
fn reinstall_spellings_are_short_and_leave_the_ref_separate() {
    let cases = [
        ("https://github.com/rimio-ai/rimz-crew", None, "crew"),
        (
            "https://github.com/rimio-ai/rimz-skills",
            Some("one"),
            "skills/one",
        ),
        ("https://github.com/o/r", None, "github:o/r"),
        (
            "https://user@example.test/o/r",
            None,
            "https://user@example.test/o/r",
        ),
        ("git@example.test:o/r", None, "git@example.test:o/r"),
    ];
    let actual: Vec<_> = cases
        .iter()
        .map(|(url, pack, _)| {
            GitSource {
                source: url.to_string(),
                url: url.to_string(),
                pack: pack.map(name),
                reference: Some("v1".to_owned()),
            }
            .spelling()
        })
        .collect();
    assert_eq!(actual, cases.map(|(_, _, expected)| expected.to_owned()));
}
