use std::io::Write;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Args;
use serde::Deserialize;
use url::Url;

use super::super::render;

const CONTENTS_API: &str = "https://api.github.com/repos/rimio-ai/rimz/contents/examples/teams";
const HTTP_TIMEOUT: Duration = Duration::from_secs(120);
const API_MAX_BYTES: u64 = 2 * 1024 * 1024;
const FILE_MAX_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Args)]
pub(super) struct InstallArgs {
    /// Team bundle to install. Omit to list available bundles.
    #[arg(
        value_name = "NAME",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::team_names)
    )]
    name: Option<String>,
    /// Release tag or branch to fetch.
    #[arg(long = "ref", value_name = "TAG|BRANCH")]
    reference: Option<String>,
    /// Replace files in an existing bundle directory.
    #[arg(long, requires = "name")]
    force: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
struct ContentsEntry {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    download_url: Option<String>,
}

#[derive(Debug, thiserror::Error)]
enum FetchError {
    #[error("cannot fetch {url}; check the network connection and retry: {source}")]
    Request {
        url: String,
        #[source]
        source: ureq::Error,
    },
    #[error("GitHub returned HTTP {status} for {url}")]
    Status { url: String, status: u16 },
    #[error("cannot read GitHub response from {url}: {source}")]
    Body {
        url: String,
        #[source]
        source: ureq::Error,
    },
    #[error("cannot parse GitHub contents response from {url}: {source}")]
    Json {
        url: String,
        #[source]
        source: serde_json::Error,
    },
}

pub(super) fn run(args: InstallArgs) -> Result<()> {
    let default_ref = format!("v{}", env!("CARGO_PKG_VERSION"));
    let reference = args.reference.as_deref().unwrap_or(&default_ref);
    let agent = http_agent();
    let Some(name) = args.name.as_deref() else {
        return list_bundles(&agent, reference, args.reference.is_none());
    };
    validate_bundle_name(name)?;
    let entries = fetch_entries(&agent, contents_url(Some(name), reference)?).map_err(|error| {
        if args.reference.is_none()
            && matches!(error, FetchError::Status { status: 404, .. })
        {
            anyhow::anyhow!(
                "team bundle `{name}` is unavailable at release `{reference}`; this may be a development build, retry with `rimz teams install {name} --ref main`"
            )
        } else {
            error.into()
        }
    })?;
    let files = download_bundle(&agent, entries)?;
    let dir = rimz::store::paths::agents_home().join("teams").join(name);
    write_bundle(&dir, &files, args.force)?;

    let mut out = render::out();
    writeln!(out, "installed {name} at {}", dir.display())?;
    writeln!(out, "launch with: rimz teams {name} -w <worktree>")?;
    Ok(())
}

fn list_bundles(agent: &ureq::Agent, reference: &str, default_ref: bool) -> Result<()> {
    let entries = fetch_entries(agent, contents_url(None, reference)?).map_err(|error| {
        if default_ref && matches!(error, FetchError::Status { status: 404, .. }) {
            anyhow::anyhow!(
                "team bundles are unavailable at release `{reference}`; this may be a development build, retry with `rimz teams install --ref main`"
            )
        } else {
            error.into()
        }
    })?;
    let mut bundles = entries
        .into_iter()
        .filter(|entry| entry.kind == "dir")
        .map(|entry| entry.name)
        .collect::<Vec<_>>();
    bundles.sort();
    if bundles.is_empty() {
        bail!("no team bundles found at `{reference}`");
    }
    let mut table = render::Table::new(["TEAM", "REF"]);
    for bundle in bundles {
        table.row([
            render::cell(bundle).fg(render::palette::accent()),
            render::cell(reference),
        ]);
    }
    table.render(&mut render::out())?;
    Ok(())
}

fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(HTTP_TIMEOUT))
        .build()
        .new_agent()
}

fn contents_url(name: Option<&str>, reference: &str) -> Result<Url> {
    let mut url = Url::parse(CONTENTS_API).expect("static GitHub contents URL is valid");
    if let Some(name) = name {
        url.path_segments_mut()
            .map_err(|()| anyhow::anyhow!("GitHub contents URL cannot accept a bundle name"))?
            .push(name);
    }
    url.query_pairs_mut().append_pair("ref", reference);
    Ok(url)
}

fn fetch_entries(
    agent: &ureq::Agent,
    url: Url,
) -> std::result::Result<Vec<ContentsEntry>, FetchError> {
    let text = fetch_text(agent, &url, API_MAX_BYTES)?;
    parse_entries(&text).map_err(|source| FetchError::Json {
        url: url.to_string(),
        source,
    })
}

fn fetch_text(
    agent: &ureq::Agent,
    url: &Url,
    limit: u64,
) -> std::result::Result<String, FetchError> {
    let mut response = agent
        .get(url.as_str())
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "rimz-teams")
        .call()
        .map_err(|source| FetchError::Request {
            url: url.to_string(),
            source,
        })?;
    if !response.status().is_success() {
        return Err(FetchError::Status {
            url: url.to_string(),
            status: response.status().as_u16(),
        });
    }
    response
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_string()
        .map_err(|source| FetchError::Body {
            url: url.to_string(),
            source,
        })
}

fn parse_entries(json: &str) -> std::result::Result<Vec<ContentsEntry>, serde_json::Error> {
    serde_json::from_str(json)
}

fn download_bundle(
    agent: &ureq::Agent,
    entries: Vec<ContentsEntry>,
) -> Result<Vec<(String, String)>> {
    let mut files = Vec::new();
    for entry in entries {
        if entry.kind != "file" {
            bail!(
                "team bundle contains unsupported {} entry `{}`",
                entry.kind,
                entry.name
            );
        }
        validate_file_name(&entry.name)?;
        let raw = entry
            .download_url
            .context("GitHub omitted a team bundle file's download URL")?;
        let url =
            Url::parse(&raw).context("GitHub returned an invalid team bundle download URL")?;
        if url.scheme() != "https" {
            bail!("GitHub returned a non-HTTPS team bundle download URL");
        }
        let contents = fetch_text(agent, &url, FILE_MAX_BYTES)?;
        files.push((entry.name, contents));
    }
    if files.is_empty() {
        bail!("team bundle contains no files");
    }
    Ok(files)
}

fn validate_bundle_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        bail!("invalid team bundle name `{name}`");
    }
    Ok(())
}

fn validate_file_name(name: &str) -> Result<()> {
    let path = Path::new(name);
    if name.is_empty()
        || name == "."
        || name == ".."
        || path.components().count() != 1
        || path.file_name().and_then(|name| name.to_str()) != Some(name)
    {
        bail!("team bundle contains invalid file name `{name}`");
    }
    Ok(())
}

fn write_bundle(dir: &Path, files: &[(String, String)], force: bool) -> Result<()> {
    for (name, _) in files {
        validate_file_name(name)?;
    }
    match std::fs::symlink_metadata(dir) {
        Ok(_) if !force => {
            bail!(
                "team bundle directory {} already exists; pass --force to replace its files",
                dir.display()
            )
        }
        Ok(metadata) if !metadata.is_dir() => {
            bail!(
                "team bundle destination {} is not a directory",
                dir.display()
            )
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("creating team bundle directory {}", dir.display()))?;
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("checking team bundle directory {}", dir.display()));
        }
    }
    for (name, contents) in files {
        rimz::store::atomic::write_bytes_atomically(&dir.join(name), contents.as_bytes())
            .with_context(|| format!("writing team bundle file {name}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(contents: &str) -> Vec<(String, String)> {
        vec![
            ("team.toml".to_owned(), contents.to_owned()),
            ("planner.md".to_owned(), "plan".to_owned()),
        ]
    }

    #[test]
    fn contents_api_json_is_structured() {
        let parsed = parse_entries(
            r#"[
                {"name":"forge","type":"dir","download_url":null},
                {"name":"team.toml","type":"file","download_url":"https://raw.githubusercontent.com/rimio-ai/rimz/main/examples/teams/forge/team.toml"}
            ]"#,
        )
        .unwrap();
        assert_eq!(parsed[0].name, "forge");
        assert_eq!(parsed[0].kind, "dir");
        assert_eq!(
            parsed[1].download_url.as_deref(),
            Some(
                "https://raw.githubusercontent.com/rimio-ai/rimz/main/examples/teams/forge/team.toml"
            )
        );
    }

    #[test]
    fn write_bundle_is_fresh_refusing_and_force_replacing() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("forge");
        write_bundle(&dir, &files("one"), false).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("team.toml")).unwrap(),
            "one"
        );

        let error = write_bundle(&dir, &files("two"), false).unwrap_err();
        assert!(error.to_string().contains("--force"));
        assert_eq!(
            std::fs::read_to_string(dir.join("team.toml")).unwrap(),
            "one"
        );

        write_bundle(&dir, &files("two"), true).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("team.toml")).unwrap(),
            "two"
        );
    }

    #[test]
    fn bundle_and_file_names_reject_path_traversal() {
        assert!(validate_bundle_name("../forge").is_err());
        assert!(validate_file_name("../team.toml").is_err());
        assert!(validate_file_name("nested/team.toml").is_err());

        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("forge");
        assert!(
            write_bundle(
                &dir,
                &[("../team.toml".to_owned(), "invalid".to_owned())],
                false,
            )
            .is_err()
        );
        assert!(!dir.exists());
    }
}
