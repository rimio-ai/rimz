//! Forge pull-request ref and status parsing.
//!
//! RimZ keeps forge command execution outside this module. The pure helpers
//! here identify PR numbers, ref shapes, host families, and status JSON emitted
//! by forge CLIs.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use crate::store::snapshot::{WorktreePrCi, WorktreePrState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Forge {
    GitHubStyle,
    GitLab,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForgeCli {
    Gh,
    Tea,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrTarget {
    pub number: u64,
    pub forge: Option<Forge>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrHead {
    pub branch: String,
    pub owner: Option<String>,
    pub repo_full_name: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrCandidate {
    pub number: u64,
    pub state: WorktreePrState,
    pub ci: Option<WorktreePrCi>,
}

pub fn parse(raw: &str) -> Result<PrTarget, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("PR must be a number or a pull-request URL".to_owned());
    }
    if trimmed.bytes().all(|byte| byte.is_ascii_digit()) {
        return Ok(PrTarget {
            number: parse_number(trimmed)?,
            forge: None,
        });
    }

    let segments = trimmed.split('/').collect::<Vec<_>>();
    for (index, segment) in segments.iter().enumerate() {
        let marker = clean_segment(segment);
        let forge = match marker {
            "pull" | "pulls" => Forge::GitHubStyle,
            "merge_requests" => Forge::GitLab,
            _ => continue,
        };
        let Some(number) = segments
            .get(index + 1)
            .map(|segment| clean_segment(segment))
        else {
            return Err(format!("PR URL is missing a number after `{marker}`"));
        };
        return Ok(PrTarget {
            number: parse_number(number)?,
            forge: Some(forge),
        });
    }

    Err("PR must be a number or a GitHub, Gitea, Forgejo, or GitLab PR URL".to_owned())
}

pub fn forge_for_remote(remote_url: &str) -> Forge {
    if remote_host(remote_url)
        .to_ascii_lowercase()
        .contains("gitlab")
    {
        Forge::GitLab
    } else {
        Forge::GitHubStyle
    }
}

pub fn forge_cli_for_remote(remote_url: &str) -> Option<ForgeCli> {
    let host = remote_host(remote_url).to_ascii_lowercase();
    if host == "github.com" {
        Some(ForgeCli::Gh)
    } else if host.contains("gitea") || host.contains("forgejo") || host.contains("codeberg") {
        Some(ForgeCli::Tea)
    } else {
        None
    }
}

/// Extract the `owner/repo` slug from a git remote URL.
pub fn remote_repo_slug(remote_url: &str) -> Option<String> {
    let trimmed = remote_url.trim();
    let had_scheme = trimmed.contains("://");
    let without_scheme = trimmed
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    let after_userinfo = without_scheme
        .rsplit_once('@')
        .map(|(_, rest)| rest)
        .unwrap_or(without_scheme);
    let path = if had_scheme {
        let (authority, path) = after_userinfo.split_once('/')?;
        (!authority.is_empty()).then_some(path)?
    } else if let Some((host, path)) = after_userinfo.split_once(':') {
        (!host.is_empty()).then_some(path)?
    } else {
        let (host, path) = after_userinfo.split_once('/')?;
        (!host.is_empty()).then_some(path)?
    };
    let path = path.trim_matches('/');
    let slug = path.strip_suffix(".git").unwrap_or(path);
    let mut segments = slug.split('/');
    let owner = segments.next()?;
    let repo = segments.next()?;
    (!owner.is_empty() && !repo.is_empty() && segments.next().is_none()).then(|| slug.to_owned())
}

/// Find a pull-request head and the remote branches which point at it from one
/// `git ls-remote` response.
pub fn pr_head_branches(output: &str, pr_ref: &str) -> Option<(String, Vec<String>)> {
    let refs = output
        .lines()
        .filter_map(|line| line.split_once(char::is_whitespace))
        .map(|(sha, ref_name)| (ref_name.trim(), sha.trim()))
        .filter(|(ref_name, sha)| !ref_name.is_empty() && !sha.is_empty())
        .collect::<BTreeMap<_, _>>();
    let pr_sha = refs.get(pr_ref)?.to_string();
    let branches = refs
        .iter()
        .filter_map(|(ref_name, sha)| {
            (*sha == pr_sha)
                .then(|| ref_name.strip_prefix("refs/heads/"))
                .flatten()
                .map(ToOwned::to_owned)
        })
        .collect();
    Some((pr_sha, branches))
}

pub fn parse_gh_pr_view_json(raw: &str) -> Result<PrHead, String> {
    #[derive(Deserialize)]
    struct Pull {
        #[serde(rename = "headRefName")]
        head_ref_name: String,
        #[serde(rename = "headRepository")]
        head_repository: Option<Repository>,
        #[serde(rename = "headRepositoryOwner")]
        head_repository_owner: Option<Owner>,
    }

    #[derive(Deserialize)]
    struct Repository {
        name: String,
    }

    #[derive(Deserialize)]
    struct Owner {
        login: String,
    }

    let pull: Pull = serde_json::from_str(raw).map_err(|err| err.to_string())?;
    let branch = required_json_text(&pull.head_ref_name, "gh PR head branch")?;
    let owner = pull
        .head_repository_owner
        .and_then(|owner| nonempty(owner.login));
    let repo_full_name = owner
        .as_deref()
        .zip(
            pull.head_repository
                .and_then(|repo| nonempty(repo.name))
                .as_deref(),
        )
        .map(|(owner, repo)| format!("{owner}/{repo}"));
    Ok(PrHead {
        branch,
        owner,
        repo_full_name,
    })
}

pub fn parse_tea_pr_head_json(raw: &str) -> Result<PrHead, String> {
    let value: Value = serde_json::from_str(raw).map_err(|err| err.to_string())?;
    let branch = pr_head_branch(&value)
        .ok_or_else(|| "tea PR output has no usable head branch".to_owned())?;
    let head = value.get("head");
    let label = head.and_then(|head| {
        head.as_str().or_else(|| {
            head.as_object()
                .and_then(|object| object.get("label"))
                .and_then(Value::as_str)
        })
    });
    let label_owner = label
        .and_then(|label| label.trim().rsplit_once(':'))
        .and_then(|(owner, _)| nonempty(owner));
    let repo_full_name = head
        .and_then(|head| head.get("repo"))
        .and_then(|repo| repo.get("full_name"))
        .and_then(Value::as_str)
        .and_then(nonempty);
    let owner = label_owner.or_else(|| {
        repo_full_name
            .as_deref()
            .and_then(|repo| repo.split_once('/'))
            .and_then(|(owner, _)| nonempty(owner))
    });
    Ok(PrHead {
        branch,
        owner,
        repo_full_name,
    })
}

/// Build a sibling repository URL while preserving the origin's transport.
pub fn sibling_repo_url(origin_url: &str, repo_full_name: &str) -> Option<String> {
    let origin = origin_url.trim().trim_end_matches('/');
    let repo = repo_full_name.trim().trim_matches('/');
    if origin.is_empty() || repo.is_empty() || repo.split('/').any(|segment| segment.is_empty()) {
        return None;
    }
    let suffix = if origin.ends_with(".git") { ".git" } else { "" };

    if let Some((scheme, rest)) = origin.split_once("://") {
        let (authority, _) = rest.split_once('/')?;
        if scheme.is_empty() || authority.is_empty() {
            return None;
        }
        return Some(format!("{scheme}://{authority}/{repo}{suffix}"));
    }

    if let Some((authority, _)) = origin.split_once(':') {
        if authority.is_empty() || authority.contains('/') {
            return None;
        }
        return Some(format!("{authority}:{repo}{suffix}"));
    }

    let (authority, _) = origin.split_once('/')?;
    (!authority.is_empty()).then(|| format!("{authority}/{repo}{suffix}"))
}

fn required_json_text(raw: &str, label: &str) -> Result<String, String> {
    nonempty(raw).ok_or_else(|| format!("{label} is empty"))
}

fn nonempty(raw: impl AsRef<str>) -> Option<String> {
    let raw = raw.as_ref().trim();
    (!raw.is_empty()).then(|| raw.to_owned())
}

impl Forge {
    pub fn pr_refspec(self, number: u64) -> String {
        match self {
            Self::GitHubStyle => format!("refs/pull/{number}/head"),
            Self::GitLab => format!("refs/merge-requests/{number}/head"),
        }
    }
}

fn parse_number(raw: &str) -> Result<u64, String> {
    if raw.is_empty() {
        return Err("PR number cannot be empty".to_owned());
    }
    if !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("PR number `{raw}` must contain only digits"));
    }
    raw.parse::<u64>()
        .map_err(|_| format!("PR number `{raw}` is too large"))
}

fn clean_segment(segment: &str) -> &str {
    segment.split(['?', '#']).next().unwrap_or(segment).trim()
}

pub(crate) fn remote_host(remote_url: &str) -> &str {
    let trimmed = remote_url.trim();
    let without_scheme = trimmed
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    let authority = without_scheme
        .rsplit_once('@')
        .map(|(_, rest)| rest)
        .unwrap_or(without_scheme);
    authority
        .split(['/', ':'])
        .next()
        .unwrap_or(authority)
        .trim()
}

pub fn parse_gh_pr_state_json(raw: &str) -> Result<Option<PrCandidate>, String> {
    #[derive(Deserialize)]
    struct Pull {
        number: u64,
        state: String,
    }

    let pulls: Vec<Pull> = serde_json::from_str(raw).map_err(|err| err.to_string())?;
    Ok(pulls
        .into_iter()
        .filter_map(|pull| {
            Some(PrCandidate {
                number: pull.number,
                state: parse_pr_state(&pull.state)?,
                ci: None,
            })
        })
        .fold(None, |current, next| Some(prefer_candidate(current, next))))
}

pub fn parse_gh_pr_list_links(raw: &str) -> Result<BTreeMap<String, PrCandidate>, String> {
    #[derive(Deserialize)]
    struct Pull {
        number: u64,
        state: String,
        #[serde(rename = "headRefName")]
        head_ref_name: String,
        #[serde(rename = "statusCheckRollup", default)]
        status_check_rollup: Option<Vec<Value>>,
    }

    let pulls: Vec<Pull> = serde_json::from_str(raw).map_err(|err| err.to_string())?;
    let mut links = BTreeMap::new();
    for pull in pulls {
        let branch = pull.head_ref_name.trim();
        let Some(state) = parse_pr_state(&pull.state) else {
            continue;
        };
        if branch.is_empty() {
            continue;
        }
        let candidate = PrCandidate {
            number: pull.number,
            state,
            ci: ci_from_gh_rollup(pull.status_check_rollup.as_deref().unwrap_or_default()),
        };
        links.insert(
            branch.to_owned(),
            prefer_candidate(links.get(branch).copied(), candidate),
        );
    }
    Ok(links)
}

pub fn parse_tea_pr_list_json(raw: &str, branch: &str) -> Result<Option<PrCandidate>, String> {
    let value: Value = serde_json::from_str(raw).map_err(|err| err.to_string())?;
    let pulls = value
        .as_array()
        .ok_or_else(|| "tea PR list output must be a JSON array".to_owned())?;
    Ok(pulls
        .iter()
        .filter(|pull| pr_head_matches(pull, branch))
        .filter_map(|pull| {
            Some(PrCandidate {
                number: pr_number(pull)?,
                state: pr_state_from_value(pull)?,
                ci: None,
            })
        })
        .fold(None, |current, next| Some(prefer_candidate(current, next))))
}

pub fn parse_tea_pr_list_links(raw: &str) -> Result<BTreeMap<String, PrCandidate>, String> {
    let value: Value = serde_json::from_str(raw).map_err(|err| err.to_string())?;
    let pulls = value
        .as_array()
        .ok_or_else(|| "tea PR list output must be a JSON array".to_owned())?;
    let mut links = BTreeMap::new();
    for pull in pulls {
        let Some(branch) = pr_head_branch(pull) else {
            continue;
        };
        let Some(state) = pr_state_from_value(pull) else {
            continue;
        };
        let Some(number) = pr_number(pull) else {
            continue;
        };
        let candidate = PrCandidate {
            number,
            state,
            ci: None,
        };
        links.insert(
            branch.clone(),
            prefer_candidate(links.get(&branch).copied(), candidate),
        );
    }
    Ok(links)
}

pub fn parse_tea_pr_detail_json(raw: &str) -> Result<Option<WorktreePrState>, String> {
    let value: Value = serde_json::from_str(raw).map_err(|err| err.to_string())?;
    Ok(pr_state_from_value(&value))
}

/// Build the `tea pr list` argv for PR-state enrichment.
///
/// Both tea callers use the same bounded page size so older closed or merged
/// PRs do not fall off tea's default page while the sidebar is detecting a
/// branch transition.
pub fn tea_pr_list_args<'a>(state: &'a str, repo: Option<&'a str>) -> Vec<&'a str> {
    let mut args = vec![
        "pr",
        "list",
        "--state",
        state,
        "--output",
        "json",
        "--fields",
        "index,state,head",
        "--limit",
        "500",
    ];
    if let Some(repo) = repo {
        args.extend_from_slice(&["--repo", repo]);
    }
    args
}

/// Build the Gitea combined commit-status endpoint used for CI enrichment.
pub fn tea_commit_status_endpoint(repo_slug: &str, branch: &str) -> String {
    format!("repos/{repo_slug}/commits/{branch}/status")
}

/// Aggregate GitHub's check rollup into the worst actionable CI verdict.
pub fn ci_from_gh_rollup(rollup: &[Value]) -> Option<WorktreePrCi> {
    let mut verdict = None;
    for item in rollup {
        let Some(kind) = item.get("__typename").and_then(Value::as_str) else {
            continue;
        };
        let next = if kind.eq_ignore_ascii_case("CheckRun") {
            let status = item.get("status").and_then(Value::as_str);
            let conclusion = item.get("conclusion").and_then(Value::as_str);
            if conclusion.is_some_and(|value| {
                [
                    "FAILURE",
                    "TIMED_OUT",
                    "ACTION_REQUIRED",
                    "CANCELLED",
                    "STARTUP_FAILURE",
                ]
                .iter()
                .any(|failure| value.eq_ignore_ascii_case(failure))
            }) {
                Some(WorktreePrCi::Failing)
            } else if status.is_some_and(|value| !value.eq_ignore_ascii_case("COMPLETED"))
                || (status.is_some_and(|value| value.eq_ignore_ascii_case("COMPLETED"))
                    && conclusion.is_none())
            {
                Some(WorktreePrCi::Pending)
            } else if conclusion.is_some_and(|value| value.eq_ignore_ascii_case("SUCCESS")) {
                Some(WorktreePrCi::Passing)
            } else {
                None
            }
        } else if kind.eq_ignore_ascii_case("StatusContext") {
            match item.get("state").and_then(Value::as_str) {
                Some(value)
                    if ["FAILURE", "ERROR"]
                        .iter()
                        .any(|failure| value.eq_ignore_ascii_case(failure)) =>
                {
                    Some(WorktreePrCi::Failing)
                }
                Some(value)
                    if ["PENDING", "EXPECTED"]
                        .iter()
                        .any(|pending| value.eq_ignore_ascii_case(pending)) =>
                {
                    Some(WorktreePrCi::Pending)
                }
                Some(value) if value.eq_ignore_ascii_case("SUCCESS") => Some(WorktreePrCi::Passing),
                _ => None,
            }
        } else {
            None
        };
        verdict = worst_ci(verdict, next);
    }
    verdict
}

/// Parse Gitea's combined commit status into the sidebar CI vocabulary.
pub fn parse_tea_combined_status(raw: &str) -> Result<Option<WorktreePrCi>, String> {
    let value: Value = serde_json::from_str(raw).map_err(|err| err.to_string())?;
    let object = value
        .as_object()
        .ok_or_else(|| "tea combined commit status must be a JSON object".to_owned())?;
    let verdict = object
        .get("state")
        .and_then(Value::as_str)
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .and_then(|state| match state.as_str() {
            "success" => Some(WorktreePrCi::Passing),
            "pending" => Some(WorktreePrCi::Pending),
            "failure" | "error" | "warning" => Some(WorktreePrCi::Failing),
            _ => None,
        });
    Ok(verdict)
}

fn worst_ci(current: Option<WorktreePrCi>, next: Option<WorktreePrCi>) -> Option<WorktreePrCi> {
    match (current, next) {
        (Some(WorktreePrCi::Failing), _) | (_, Some(WorktreePrCi::Failing)) => {
            Some(WorktreePrCi::Failing)
        }
        (Some(WorktreePrCi::Pending), _) | (_, Some(WorktreePrCi::Pending)) => {
            Some(WorktreePrCi::Pending)
        }
        (Some(WorktreePrCi::Passing), _) | (_, Some(WorktreePrCi::Passing)) => {
            Some(WorktreePrCi::Passing)
        }
        (None, None) => None,
    }
}

fn pr_state_from_value(value: &Value) -> Option<WorktreePrState> {
    if value
        .get("merged")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || value
            .get("merged_at")
            .is_some_and(|merged| !merged.is_null())
    {
        return Some(WorktreePrState::Merged);
    }
    ["state", "status"].iter().find_map(|field| {
        value
            .get(field)
            .and_then(Value::as_str)
            .and_then(parse_pr_state)
    })
}

fn parse_pr_state(raw: &str) -> Option<WorktreePrState> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "open" => Some(WorktreePrState::Open),
        "closed" => Some(WorktreePrState::Closed),
        "merged" => Some(WorktreePrState::Merged),
        _ => None,
    }
}

fn prefer_candidate(current: Option<PrCandidate>, next: PrCandidate) -> PrCandidate {
    let Some(current) = current else {
        return next;
    };
    if pr_state_rank(next.state) > pr_state_rank(current.state) {
        next
    } else {
        current
    }
}

fn pr_state_rank(state: WorktreePrState) -> u8 {
    // Single precedence source: merged beats open beats closed.
    match state {
        WorktreePrState::Closed => 0,
        WorktreePrState::Open => 1,
        WorktreePrState::Merged => 2,
    }
}

fn pr_number(value: &Value) -> Option<u64> {
    ["number", "index", "id"].iter().find_map(|field| {
        let value = value.get(field)?;
        value
            .as_u64()
            .or_else(|| value.as_str().and_then(|raw| raw.parse().ok()))
    })
}

fn pr_head_matches(value: &Value, branch: &str) -> bool {
    pr_head_branch(value).is_some_and(|head| ref_name_matches(&head, branch))
}

fn pr_head_branch(value: &Value) -> Option<String> {
    [
        "head",
        "head_branch",
        "headRefName",
        "source_branch",
        "sourceBranch",
    ]
    .iter()
    .find_map(|field| head_branch_from_value(value.get(field)))
}

fn head_branch_from_value(value: Option<&Value>) -> Option<String> {
    let value = value?;
    if let Some(raw) = value.as_str() {
        return ref_name_branch(raw);
    }
    if let Some(object) = value.as_object() {
        return ["ref", "name", "branch", "label"].iter().find_map(|field| {
            object
                .get(*field)
                .and_then(Value::as_str)
                .and_then(ref_name_branch)
        });
    }
    None
}

fn ref_name_branch(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    Some(
        raw.rsplit_once(':')
            .map(|(_, name)| name)
            .unwrap_or(raw)
            .to_owned(),
    )
}

fn ref_name_matches(raw: &str, branch: &str) -> bool {
    let raw = raw.trim();
    raw == branch || raw.rsplit_once(':').is_some_and(|(_, name)| name == branch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_number_without_forge() {
        assert_eq!(
            parse(" 42 ").expect("parse PR number"),
            PrTarget {
                number: 42,
                forge: None
            }
        );
    }

    #[test]
    fn parses_github_style_urls() {
        assert_eq!(
            parse("https://github.com/org/repo/pull/123").expect("github URL"),
            PrTarget {
                number: 123,
                forge: Some(Forge::GitHubStyle)
            }
        );
        assert_eq!(
            parse("https://gitea.example.test/org/repo/pulls/7").expect("gitea URL"),
            PrTarget {
                number: 7,
                forge: Some(Forge::GitHubStyle)
            }
        );
    }

    #[test]
    fn parses_gitlab_urls() {
        assert_eq!(
            parse("https://gitlab.com/org/repo/-/merge_requests/9").expect("gitlab URL"),
            PrTarget {
                number: 9,
                forge: Some(Forge::GitLab)
            }
        );
    }

    #[test]
    fn maps_remote_hosts_to_forge() {
        for remote in [
            "https://github.com/org/repo.git",
            "git@github.com:org/repo.git",
            "https://gitea.example.test/org/repo.git",
            "git@gitea.example.test:org/repo.git",
        ] {
            assert_eq!(forge_for_remote(remote), Forge::GitHubStyle, "{remote}");
        }
        for remote in [
            "https://gitlab.com/org/repo.git",
            "git@gitlab.com:org/repo.git",
            "ssh://git@gitlab.example.test/org/repo.git",
            "ssh://git@gitlab.example.test:2222/org/repo.git",
        ] {
            assert_eq!(forge_for_remote(remote), Forge::GitLab, "{remote}");
        }
    }

    #[test]
    fn maps_remote_hosts_to_forge_cli() {
        for remote in [
            "https://github.com/org/repo.git",
            "git@github.com:org/repo.git",
        ] {
            assert_eq!(forge_cli_for_remote(remote), Some(ForgeCli::Gh), "{remote}");
        }
        for remote in [
            "https://gitea.example.test/org/repo.git",
            "git@forgejo.example.test:org/repo.git",
            "https://codeberg.org/org/repo.git",
        ] {
            assert_eq!(
                forge_cli_for_remote(remote),
                Some(ForgeCli::Tea),
                "{remote}"
            );
        }
        for remote in [
            "https://gitlab.com/org/repo.git",
            "https://example.test/org/repo.git",
        ] {
            assert_eq!(forge_cli_for_remote(remote), None, "{remote}");
        }
    }

    #[test]
    fn extracts_remote_repo_slug() {
        for (remote, slug) in [
            ("git@gitea-ssh.example.test:owner/repo.git", "owner/repo"),
            ("https://gitea.example.test/owner/repo.git", "owner/repo"),
            ("ssh://git@host:2222/owner/repo.git", "owner/repo"),
            ("git@host:owner/repo", "owner/repo"),
            ("https://host/owner/repo/", "owner/repo"),
        ] {
            assert_eq!(remote_repo_slug(remote), Some(slug.to_owned()), "{remote}");
        }
    }

    #[test]
    fn rejects_remote_repo_slug_without_owner_repo_path() {
        for remote in [
            "",
            "git@gitea-ssh.example.test",
            "not-a-remote",
            "/tmp/repo",
            "https://host/repo.git",
            "https:///owner/repo.git",
            "git@host:owner/team/repo.git",
        ] {
            assert_eq!(remote_repo_slug(remote), None, "{remote}");
        }
    }

    #[test]
    fn resolves_pr_head_branches_from_ls_remote() {
        let raw = "\
aaa\trefs/heads/main
bbb\trefs/heads/feature
bbb\trefs/pull/7/head
bbb\trefs/heads/same-tip
";
        assert_eq!(
            pr_head_branches(raw, "refs/pull/7/head"),
            Some((
                "bbb".to_owned(),
                vec!["feature".to_owned(), "same-tip".to_owned()]
            ))
        );
        assert_eq!(pr_head_branches(raw, "refs/pull/8/head"), None);
        assert_eq!(
            pr_head_branches("bbb\trefs/pull/7/head\n", "refs/pull/7/head"),
            Some(("bbb".to_owned(), Vec::new()))
        );
    }

    #[test]
    fn parses_gh_pr_heads() {
        assert_eq!(
            parse_gh_pr_view_json(
                r#"{
                    "headRefName":"feature",
                    "headRepository":{"name":"repo"},
                    "headRepositoryOwner":{"login":"org"}
                }"#
            )
            .unwrap(),
            PrHead {
                branch: "feature".to_owned(),
                owner: Some("org".to_owned()),
                repo_full_name: Some("org/repo".to_owned()),
            }
        );
        assert_eq!(
            parse_gh_pr_view_json(
                r#"{
                    "headRefName":"fork-work",
                    "headRepository":{"name":"fork"},
                    "headRepositoryOwner":{"login":"alice"}
                }"#
            )
            .unwrap()
            .repo_full_name
            .as_deref(),
            Some("alice/fork")
        );
    }

    #[test]
    fn parses_tea_pr_heads() {
        assert_eq!(
            parse_tea_pr_head_json(
                r#"{
                    "head":{"label":"alice:feature","repo":{"full_name":"alice/fork"}},
                    "base":{"repo":{"full_name":"org/repo"}}
                }"#
            )
            .unwrap(),
            PrHead {
                branch: "feature".to_owned(),
                owner: Some("alice".to_owned()),
                repo_full_name: Some("alice/fork".to_owned()),
            }
        );
        assert_eq!(
            parse_tea_pr_head_json(r#"{"head":{"ref":"feature","repo":{"full_name":"org/repo"}}}"#)
                .unwrap(),
            PrHead {
                branch: "feature".to_owned(),
                owner: Some("org".to_owned()),
                repo_full_name: Some("org/repo".to_owned()),
            }
        );
    }

    #[test]
    fn builds_sibling_repo_urls() {
        for (origin, expected) in [
            (
                "https://github.com/org/repo.git",
                "https://github.com/alice/fork.git",
            ),
            (
                "ssh://git@host:2222/org/repo.git",
                "ssh://git@host:2222/alice/fork.git",
            ),
            ("git@host:org/repo.git", "git@host:alice/fork.git"),
            ("host/org/repo", "host/alice/fork"),
        ] {
            assert_eq!(
                sibling_repo_url(origin, "alice/fork").as_deref(),
                Some(expected),
                "{origin}"
            );
        }
        assert_eq!(sibling_repo_url("/tmp/origin.git", "alice/fork"), None);
    }

    #[test]
    fn parses_gh_pr_state_json_with_priority() {
        assert_eq!(
            parse_gh_pr_state_json(
                r#"[{"number":1,"state":"CLOSED"},{"number":2,"state":"OPEN"}]"#
            )
            .unwrap(),
            Some(PrCandidate {
                number: 2,
                state: WorktreePrState::Open,
                ci: None,
            })
        );
        assert_eq!(
            parse_gh_pr_state_json(
                r#"[{"number":1,"state":"OPEN"},{"number":2,"state":"MERGED"}]"#
            )
            .unwrap(),
            Some(PrCandidate {
                number: 2,
                state: WorktreePrState::Merged,
                ci: None,
            })
        );
        assert_eq!(parse_gh_pr_state_json("[]").unwrap(), None);
        assert!(parse_gh_pr_state_json("{").is_err());
    }

    #[test]
    fn parses_gh_pr_list_links_by_head_branch_with_priority() {
        let links = parse_gh_pr_list_links(
            r#"[
                {"number":1,"state":"CLOSED","headRefName":"feature"},
                {"number":2,"state":"OPEN","headRefName":"feature"},
                {"number":3,"state":"OPEN","headRefName":"other"}
            ]"#,
        )
        .unwrap();

        assert_eq!(
            links.get("feature"),
            Some(&PrCandidate {
                number: 2,
                state: WorktreePrState::Open,
                ci: None,
            })
        );
        assert_eq!(
            links.get("other"),
            Some(&PrCandidate {
                number: 3,
                state: WorktreePrState::Open,
                ci: None,
            })
        );
        assert!(parse_gh_pr_list_links("{").is_err());
    }

    #[test]
    fn classifies_gh_check_rollups_by_worst_verdict() {
        let parse = |raw: &str| serde_json::from_str::<Vec<Value>>(raw).unwrap();
        assert_eq!(
            ci_from_gh_rollup(&parse(
                r#"[{"__typename":"CheckRun","status":"COMPLETED","conclusion":"SUCCESS"}]"#
            )),
            Some(WorktreePrCi::Passing)
        );
        assert_eq!(
            ci_from_gh_rollup(&parse(
                r#"[{"__typename":"StatusContext","state":"EXPECTED"}]"#
            )),
            Some(WorktreePrCi::Pending)
        );
        assert_eq!(
            ci_from_gh_rollup(&parse(
                r#"[{"__typename":"CheckRun","status":"COMPLETED","conclusion":null}]"#
            )),
            Some(WorktreePrCi::Pending)
        );
        assert_eq!(
            ci_from_gh_rollup(&parse(
                r#"[
                    {"__typename":"StatusContext","state":"SUCCESS"},
                    {"__typename":"CheckRun","status":"completed","conclusion":"timed_out"}
                ]"#
            )),
            Some(WorktreePrCi::Failing)
        );
        assert_eq!(
            ci_from_gh_rollup(&parse(
                r#"[
                    {"__typename":"CheckRun","status":"COMPLETED","conclusion":"NEUTRAL"},
                    {"__typename":"CheckRun","status":"COMPLETED","conclusion":"SKIPPED"}
                ]"#
            )),
            None
        );
    }

    #[test]
    fn gh_pr_links_include_rollup_ci() {
        let links = parse_gh_pr_list_links(
            r#"[{
                "number":2,
                "state":"OPEN",
                "headRefName":"feature",
                "statusCheckRollup":[
                    {"__typename":"StatusContext","state":"SUCCESS"},
                    {"__typename":"CheckRun","status":"IN_PROGRESS","conclusion":null}
                ]
            }]"#,
        )
        .unwrap();

        assert_eq!(links["feature"].ci, Some(WorktreePrCi::Pending));
    }

    #[test]
    fn gh_pr_links_accept_null_rollup() {
        let links = parse_gh_pr_list_links(
            r#"[{
                "number":2,
                "state":"OPEN",
                "headRefName":"feature",
                "statusCheckRollup":null
            }]"#,
        )
        .unwrap();

        assert_eq!(links["feature"].ci, None);
    }

    #[test]
    fn parses_tea_pr_list_and_detail_json() {
        let list = r#"[
            {"index": 7, "head": {"label": "me:feature"}, "state": "closed"},
            {"index": 8, "head": "other", "state": "open"}
        ]"#;
        assert_eq!(
            parse_tea_pr_list_json(list, "feature").unwrap(),
            Some(PrCandidate {
                number: 7,
                state: WorktreePrState::Closed,
                ci: None,
            })
        );
        assert_eq!(
            parse_tea_pr_detail_json(r#"{"state":"closed","merged_at":"2026-06-01T00:00:00Z"}"#)
                .unwrap(),
            Some(WorktreePrState::Merged)
        );
        assert_eq!(parse_tea_pr_list_json("[]", "feature").unwrap(), None);
        assert!(parse_tea_pr_list_json("{}", "feature").is_err());
    }

    #[test]
    fn parses_tea_pr_list_links_by_head_branch() {
        let links = parse_tea_pr_list_links(
            r#"[
                {"index": 7, "head": {"label": "me:feature"}, "state": "closed"},
                {"index": 8, "head": {"branch": "feature"}, "state": "open"},
                {"index": 9, "source_branch": "other", "state": "open"}
            ]"#,
        )
        .unwrap();

        assert_eq!(
            links.get("feature"),
            Some(&PrCandidate {
                number: 8,
                state: WorktreePrState::Open,
                ci: None,
            })
        );
        assert_eq!(
            links.get("other"),
            Some(&PrCandidate {
                number: 9,
                state: WorktreePrState::Open,
                ci: None,
            })
        );
        assert!(parse_tea_pr_list_links("{}").is_err());
    }

    #[test]
    fn tea_pr_list_args_thread_limit_state_and_repo() {
        let args = tea_pr_list_args("all", Some("org/repo"));
        assert!(args.windows(2).any(|window| window == ["--state", "all"]));
        assert!(args.windows(2).any(|window| window == ["--limit", "500"]));
        assert!(
            args.windows(2)
                .any(|window| window == ["--repo", "org/repo"])
        );

        let bare = tea_pr_list_args("open", None);
        assert!(bare.windows(2).any(|window| window == ["--limit", "500"]));
        assert!(!bare.contains(&"--repo"));
    }

    #[test]
    fn parses_tea_combined_commit_status() {
        for (state, expected) in [
            ("success", WorktreePrCi::Passing),
            ("pending", WorktreePrCi::Pending),
            ("failure", WorktreePrCi::Failing),
            ("error", WorktreePrCi::Failing),
            ("warning", WorktreePrCi::Failing),
        ] {
            let raw = format!(r#"{{"state":"{state}"}}"#);
            assert_eq!(parse_tea_combined_status(&raw).unwrap(), Some(expected));
        }

        for raw in [
            r#"{"state":""}"#,
            r#"{"state":"unknown"}"#,
            r#"{}"#,
            r#"{"message":"not found"}"#,
        ] {
            assert_eq!(parse_tea_combined_status(raw).unwrap(), None);
        }
        assert!(parse_tea_combined_status("[]").is_err());
    }

    #[test]
    fn tea_commit_status_endpoint_carries_repo_and_branch() {
        assert_eq!(
            tea_commit_status_endpoint("org/repo", "feature/topic"),
            "repos/org/repo/commits/feature/topic/status"
        );
    }

    #[test]
    fn renders_forge_refspecs() {
        assert_eq!(
            Forge::GitHubStyle.pr_refspec(5),
            "refs/pull/5/head".to_owned()
        );
        assert_eq!(
            Forge::GitLab.pr_refspec(5),
            "refs/merge-requests/5/head".to_owned()
        );
    }

    #[test]
    fn rejects_unusable_input() {
        assert!(parse("not-a-number").is_err());
        assert!(parse("https://github.com/org/repo/pull/nope").is_err());
        assert!(parse("https://example.test/org/repo/issues/1").is_err());
    }
}
