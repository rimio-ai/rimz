//! Forge pull-request ref and status parsing.
//!
//! Rimz keeps forge command execution outside this module. The pure helpers
//! here identify PR numbers, ref shapes, host families, and status JSON emitted
//! by forge CLIs.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use crate::ledger::snapshot::WorktreePrState;

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
pub struct TeaPrCandidate {
    pub number: u64,
    pub state: WorktreePrState,
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

pub fn parse_gh_pr_state_json(raw: &str) -> Result<Option<WorktreePrState>, String> {
    #[derive(Deserialize)]
    struct Pull {
        state: String,
    }

    let pulls: Vec<Pull> = serde_json::from_str(raw).map_err(|err| err.to_string())?;
    Ok(pulls
        .into_iter()
        .filter_map(|pull| parse_pr_state(&pull.state))
        .fold(None, prefer_pr_state))
}

pub fn parse_gh_pr_list_states(raw: &str) -> Result<BTreeMap<String, WorktreePrState>, String> {
    #[derive(Deserialize)]
    struct Pull {
        state: String,
        #[serde(rename = "headRefName")]
        head_ref_name: String,
    }

    let pulls: Vec<Pull> = serde_json::from_str(raw).map_err(|err| err.to_string())?;
    let mut states = BTreeMap::new();
    for pull in pulls {
        let branch = pull.head_ref_name.trim();
        let Some(state) = parse_pr_state(&pull.state) else {
            continue;
        };
        if branch.is_empty() {
            continue;
        }
        if let Some(preferred) = prefer_pr_state(states.get(branch).copied(), state) {
            states.insert(branch.to_owned(), preferred);
        }
    }
    Ok(states)
}

pub fn parse_tea_pr_list_json(raw: &str, branch: &str) -> Result<Option<TeaPrCandidate>, String> {
    let value: Value = serde_json::from_str(raw).map_err(|err| err.to_string())?;
    let pulls = value
        .as_array()
        .ok_or_else(|| "tea PR list output must be a JSON array".to_owned())?;
    Ok(pulls
        .iter()
        .filter(|pull| pr_head_matches(pull, branch))
        .filter_map(|pull| {
            Some(TeaPrCandidate {
                number: pr_number(pull)?,
                state: pr_state_from_value(pull)?,
            })
        })
        .fold(None, prefer_tea_candidate))
}

pub fn parse_tea_pr_list_states(raw: &str) -> Result<BTreeMap<String, WorktreePrState>, String> {
    let value: Value = serde_json::from_str(raw).map_err(|err| err.to_string())?;
    let pulls = value
        .as_array()
        .ok_or_else(|| "tea PR list output must be a JSON array".to_owned())?;
    let mut states = BTreeMap::new();
    for pull in pulls {
        let Some(branch) = pr_head_branch(pull) else {
            continue;
        };
        let Some(state) = pr_state_from_value(pull) else {
            continue;
        };
        if let Some(preferred) = prefer_pr_state(states.get(&branch).copied(), state) {
            states.insert(branch, preferred);
        }
    }
    Ok(states)
}

pub fn parse_tea_pr_detail_json(raw: &str) -> Result<Option<WorktreePrState>, String> {
    let value: Value = serde_json::from_str(raw).map_err(|err| err.to_string())?;
    Ok(pr_state_from_value(&value))
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

fn prefer_pr_state(
    current: Option<WorktreePrState>,
    next: WorktreePrState,
) -> Option<WorktreePrState> {
    match (current, next) {
        (Some(WorktreePrState::Merged), _) | (_, WorktreePrState::Merged) => {
            Some(WorktreePrState::Merged)
        }
        (Some(WorktreePrState::Open), _) | (_, WorktreePrState::Open) => {
            Some(WorktreePrState::Open)
        }
        (Some(WorktreePrState::Closed), _) | (None, WorktreePrState::Closed) => {
            Some(WorktreePrState::Closed)
        }
    }
}

fn prefer_tea_candidate(
    current: Option<TeaPrCandidate>,
    next: TeaPrCandidate,
) -> Option<TeaPrCandidate> {
    let Some(current) = current else {
        return Some(next);
    };
    if pr_state_rank(next.state) > pr_state_rank(current.state) {
        Some(next)
    } else {
        Some(current)
    }
}

fn pr_state_rank(state: WorktreePrState) -> u8 {
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
            ("git@gitea-ssh.***REMOVED***:rimio/rimz.git", "rimio/rimz"),
            ("https://gitea.***REMOVED***/rimio/rimz.git", "rimio/rimz"),
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
            "git@gitea-ssh.***REMOVED***",
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
    fn parses_gh_pr_state_json_with_priority() {
        assert_eq!(
            parse_gh_pr_state_json(
                r#"[{"number":1,"state":"CLOSED"},{"number":2,"state":"OPEN"}]"#
            )
            .unwrap(),
            Some(WorktreePrState::Open)
        );
        assert_eq!(
            parse_gh_pr_state_json(
                r#"[{"number":1,"state":"OPEN"},{"number":2,"state":"MERGED"}]"#
            )
            .unwrap(),
            Some(WorktreePrState::Merged)
        );
        assert_eq!(parse_gh_pr_state_json("[]").unwrap(), None);
        assert!(parse_gh_pr_state_json("{").is_err());
    }

    #[test]
    fn parses_gh_pr_list_states_by_head_branch_with_priority() {
        let states = parse_gh_pr_list_states(
            r#"[
                {"number":1,"state":"CLOSED","headRefName":"feature"},
                {"number":2,"state":"OPEN","headRefName":"feature"},
                {"number":3,"state":"OPEN","headRefName":"other"}
            ]"#,
        )
        .unwrap();

        assert_eq!(states.get("feature"), Some(&WorktreePrState::Open));
        assert_eq!(states.get("other"), Some(&WorktreePrState::Open));
        assert!(parse_gh_pr_list_states("{").is_err());
    }

    #[test]
    fn parses_tea_pr_list_and_detail_json() {
        let list = r#"[
            {"index": 7, "head": {"label": "me:feature"}, "state": "closed"},
            {"index": 8, "head": "other", "state": "open"}
        ]"#;
        assert_eq!(
            parse_tea_pr_list_json(list, "feature").unwrap(),
            Some(TeaPrCandidate {
                number: 7,
                state: WorktreePrState::Closed,
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
    fn parses_tea_pr_list_states_by_head_branch() {
        let states = parse_tea_pr_list_states(
            r#"[
                {"index": 7, "head": {"label": "me:feature"}, "state": "closed"},
                {"index": 8, "head": {"branch": "feature"}, "state": "open"},
                {"index": 9, "source_branch": "other", "state": "open"}
            ]"#,
        )
        .unwrap();

        assert_eq!(states.get("feature"), Some(&WorktreePrState::Open));
        assert_eq!(states.get("other"), Some(&WorktreePrState::Open));
        assert!(parse_tea_pr_list_states("{}").is_err());
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
