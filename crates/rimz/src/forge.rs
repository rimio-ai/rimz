//! Pure forge PR and remote parsing; command execution stays in callers.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use crate::store::snapshot::{WorktreePrCi, WorktreePrState};

const GH_HEAD_FIELDS: &str = "headRefName,headRepository,headRepositoryOwner,isCrossRepository";
const GH_LIST_FIELDS: &str = "number,state,headRefName,statusCheckRollup";
const TEA_LIST_FIELDS: &str = "index,state,head";
const LIST_LIMIT: &str = "500";
const HEAD_FIELDS: &str = "head head_branch headRefName source_branch sourceBranch";
const HEAD_OBJECT_FIELDS: &str = "ref name branch label";
const NUMBER_FIELDS: &str = "number index id";

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
#[derive(Clone, Debug)]
pub(crate) struct RemoteRepo {
    raw: String,
    host: String,
    repo_slug: Option<String>,
    transport: RemoteTransport,
}

#[derive(Clone, Debug)]
enum RemoteTransport {
    Slash(String),
    Scp(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrTarget {
    pub number: u64,
    pub forge: Option<Forge>,
    pub host: Option<String>,
    pub repo: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrHead {
    pub branch: String,
    pub owner: Option<String>,
    pub repo_full_name: Option<String>,
    pub is_cross_repository: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrCandidate {
    pub number: u64,
    pub state: WorktreePrState,
    pub ci: Option<WorktreePrCi>,
    pub merge_sha: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeaPrDetail {
    pub state: Option<WorktreePrState>,
    pub merged_sha: Option<String>,
    pub head_sha: Option<String>,
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
            host: None,
            repo: None,
        });
    }

    let url = url::Url::parse(trimmed).ok();
    let segments = url
        .as_ref()
        .and_then(|url| url.path_segments().map(Iterator::collect::<Vec<_>>))
        .unwrap_or_else(|| trimmed.split('/').collect());
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
        let (host, repo) = match url.as_ref() {
            Some(url) => {
                let (host, repo) = pr_url_identity(url, &segments, index, forge)
                    .ok_or_else(|| "PR URL must name a host and repository".to_owned())?;
                (Some(host), Some(repo))
            }
            None => (None, None),
        };
        return Ok(PrTarget {
            number: parse_number(number)?,
            forge: Some(forge),
            host,
            repo,
        });
    }

    Err("PR must be a number or a GitHub, Gitea, Forgejo, or GitLab PR URL".to_owned())
}

fn pr_url_identity(
    url: &url::Url,
    segments: &[&str],
    marker_index: usize,
    forge: Forge,
) -> Option<(String, String)> {
    let host = url.host_str()?.to_ascii_lowercase();
    let mut repo_segments = segments[..marker_index].to_vec();
    if forge == Forge::GitLab && repo_segments.last() == Some(&"-") {
        repo_segments.pop();
    }
    let repo = repo_segments.last_mut()?;
    *repo = repo.strip_suffix(".git").unwrap_or(repo);
    if repo_segments.len() < 2 || repo_segments.iter().any(|segment| segment.is_empty()) {
        return None;
    }
    Some((host, repo_segments.join("/")))
}

impl RemoteRepo {
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if ["/", ".", "~"].iter().any(|prefix| raw.starts_with(prefix))
            || raw.contains('\\')
            || raw.as_bytes().get(1) == Some(&b':')
        {
            return None;
        }
        let (host, path, transport) = if let Some((scheme, rest)) = raw.split_once("://") {
            let url = url::Url::parse(raw).ok()?;
            let (authority, path) = rest.split_once('/')?;
            (!authority.is_empty()).then_some(())?;
            (
                url.host_str()?.to_ascii_lowercase(),
                path,
                RemoteTransport::Slash(format!("{scheme}://{authority}")),
            )
        } else {
            let (authority, path, transport) = if let Some((authority, path)) = raw.split_once(':')
            {
                (!authority.contains('/')).then_some(())?;
                (authority, path, RemoteTransport::Scp(authority.to_owned()))
            } else {
                let (authority, path) = raw.split_once('/')?;
                (
                    authority,
                    path,
                    RemoteTransport::Slash(authority.to_owned()),
                )
            };
            (remote_authority_host(authority)?, path, transport)
        };
        let path = normalized_repo_path(path)?;
        let slug = path.strip_suffix(".git").unwrap_or(path);
        let repo_slug = (slug.split('/').count() >= 2).then(|| slug.to_owned());
        Some(Self {
            raw: raw.to_owned(),
            host,
            repo_slug,
            transport,
        })
    }

    pub(crate) fn host(&self) -> &str {
        &self.host
    }

    pub(crate) fn repo_slug(&self) -> Option<&str> {
        self.repo_slug.as_deref()
    }

    pub(crate) fn forge(&self) -> Forge {
        if self.host.contains("gitlab") {
            Forge::GitLab
        } else {
            Forge::GitHubStyle
        }
    }

    pub(crate) fn forge_cli(&self) -> Option<ForgeCli> {
        if self.host == "github.com" {
            return Some(ForgeCli::Gh);
        }
        ["gitea", "forgejo", "codeberg"]
            .iter()
            .any(|name| self.host.contains(name))
            .then_some(ForgeCli::Tea)
    }

    pub(crate) fn repo_key(&self, cli: ForgeCli) -> String {
        let repo = self.repo_slug.as_deref().unwrap_or(&self.raw);
        format!("{}:{}:{repo}", cli.key(), self.host())
    }

    pub(crate) fn pr_web_url(&self, number: u64) -> Option<String> {
        let repo = self.repo_slug()?;
        let path = match (self.forge(), self.forge_cli()) {
            (Forge::GitLab, _) => format!("-/merge_requests/{number}"),
            (_, Some(ForgeCli::Tea)) => format!("pulls/{number}"),
            (Forge::GitHubStyle, _) => format!("pull/{number}"),
        };
        Some(format!("https://{}/{repo}/{path}", self.host()))
    }

    pub(crate) fn matches_target(&self, target: &PrTarget) -> bool {
        let Some((host, repo)) = target.host.as_deref().zip(target.repo.as_deref()) else {
            return true;
        };
        host.eq_ignore_ascii_case(&self.host)
            && self
                .repo_slug()
                .is_some_and(|origin| repo.eq_ignore_ascii_case(origin))
    }

    pub(crate) fn sibling_url(&self, repo_full_name: &str) -> Option<String> {
        let repo = normalized_repo_path(repo_full_name.trim())?;
        let raw = self.raw.trim_end_matches('/');
        let suffix = if raw.ends_with(".git") { ".git" } else { "" };
        Some(match &self.transport {
            RemoteTransport::Slash(base) => format!("{base}/{repo}{suffix}"),
            RemoteTransport::Scp(authority) => format!("{authority}:{repo}{suffix}"),
        })
    }
}

fn normalized_repo_path(path: &str) -> Option<&str> {
    let path = path.trim_matches('/');
    (!path.is_empty() && path.split('/').all(|segment| !segment.is_empty())).then_some(path)
}

fn remote_authority_host(authority: &str) -> Option<String> {
    let host = authority.rsplit('@').next().unwrap_or_default();
    (!matches!(host, "" | "." | "..") && !host.chars().any(char::is_whitespace))
        .then(|| host.to_ascii_lowercase())
}
pub fn forge_for_remote(remote_url: &str) -> Forge {
    RemoteRepo::parse(remote_url).map_or(Forge::GitHubStyle, |remote| remote.forge())
}
pub fn forge_cli_for_remote(remote_url: &str) -> Option<ForgeCli> {
    RemoteRepo::parse(remote_url).and_then(|remote| remote.forge_cli())
}
/// Extract the `owner/repo` slug from a git remote URL.
pub fn remote_repo_slug(remote_url: &str) -> Option<String> {
    RemoteRepo::parse(remote_url).and_then(|remote| remote.repo_slug)
}

/// Return whether URL-derived PR identity names the origin repository.
pub fn pr_url_matches_origin(target: &PrTarget, origin_url: &str) -> bool {
    target.host.is_none()
        || RemoteRepo::parse(origin_url).is_some_and(|remote| remote.matches_target(target))
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
        #[serde(rename = "isCrossRepository", default)]
        is_cross_repository: Option<bool>,
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
        is_cross_repository: pull.is_cross_repository,
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
        is_cross_repository: None,
    })
}

/// Build a sibling repository URL while preserving the origin's transport.
pub fn sibling_repo_url(origin_url: &str, repo_full_name: &str) -> Option<String> {
    RemoteRepo::parse(origin_url)?.sibling_url(repo_full_name)
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

impl ForgeCli {
    pub(crate) fn program(self) -> &'static str {
        match self {
            Self::Gh => "gh",
            Self::Tea => "tea",
        }
    }

    pub(crate) fn key(self) -> &'static str {
        self.program()
    }

    pub(crate) fn pr_head_args(
        self,
        number: u64,
        repo: Option<&str>,
    ) -> Result<Vec<String>, String> {
        let number = number.to_string();
        Ok(match self {
            Self::Gh => ["pr", "view", &number, "--json", GH_HEAD_FIELDS]
                .map(str::to_owned)
                .into(),
            Self::Tea => {
                let repo = repo
                    .ok_or_else(|| "could not derive the origin repository for tea".to_owned())?;
                ["pr", &number, "--output", "json", "--repo", repo]
                    .map(str::to_owned)
                    .into()
            }
        })
    }

    pub(crate) fn decode_pr_head(self, raw: &str) -> Result<PrHead, String> {
        match self {
            Self::Gh => parse_gh_pr_view_json(raw),
            Self::Tea => parse_tea_pr_head_json(raw),
        }
    }

    pub(crate) fn open_pr_list_args(self, repo: Option<&str>) -> Vec<&str> {
        match self {
            Self::Gh => {
                let mut args = vec!["pr", "list", "--state", "open"];
                args.extend(["--json", GH_LIST_FIELDS, "--limit", LIST_LIMIT]);
                args
            }
            Self::Tea => tea_pr_list_args("open", repo),
        }
    }

    pub(crate) fn decode_open_prs(
        self,
        raw: &str,
    ) -> Result<BTreeMap<String, PrCandidate>, String> {
        match self {
            Self::Gh => parse_gh_pr_list_links(raw),
            Self::Tea => parse_tea_pr_list_links(raw),
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

#[derive(Deserialize)]
struct GhPrState {
    number: u64,
    state: String,
    #[serde(rename = "mergeCommit", default)]
    merge_commit: Option<GhMergeCommit>,
    #[serde(rename = "statusCheckRollup", default)]
    status_check_rollup: Option<Vec<Value>>,
}

#[derive(Deserialize)]
struct GhMergeCommit {
    oid: String,
}

pub fn parse_gh_pr_state_json(raw: &str) -> Result<Option<PrCandidate>, String> {
    let pulls: Vec<GhPrState> = serde_json::from_str(raw).map_err(|err| err.to_string())?;
    Ok(pulls
        .into_iter()
        .filter_map(gh_pr_candidate)
        .fold(None, |current, next| Some(prefer_candidate(current, next))))
}

pub fn parse_gh_pr_detail_json(raw: &str) -> Result<Option<PrCandidate>, String> {
    let pull: GhPrState = serde_json::from_str(raw).map_err(|err| err.to_string())?;
    Ok(gh_pr_candidate(pull))
}

fn gh_pr_candidate(pull: GhPrState) -> Option<PrCandidate> {
    let state = parse_pr_state(&pull.state)?;
    let ci = match state {
        WorktreePrState::Open | WorktreePrState::Merged => {
            ci_from_gh_rollup(pull.status_check_rollup.as_deref().unwrap_or_default())
        }
        WorktreePrState::Closed => None,
    };
    let merge_sha = if state == WorktreePrState::Merged {
        pull.merge_commit.map(|commit| commit.oid)
    } else {
        None
    }
    .filter(|sha| !sha.trim().is_empty());
    Some(PrCandidate {
        number: pull.number,
        state,
        ci,
        merge_sha,
    })
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
            merge_sha: None,
        };
        links.insert(
            branch.to_owned(),
            prefer_candidate(links.get(branch).cloned(), candidate),
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
        .filter_map(tea_pr_candidate)
        .filter(|(head, _)| head == branch)
        .map(|(_, candidate)| candidate)
        .fold(None, |current, next| Some(prefer_candidate(current, next))))
}

pub fn parse_tea_pr_list_links(raw: &str) -> Result<BTreeMap<String, PrCandidate>, String> {
    let value: Value = serde_json::from_str(raw).map_err(|err| err.to_string())?;
    let pulls = value
        .as_array()
        .ok_or_else(|| "tea PR list output must be a JSON array".to_owned())?;
    let mut links = BTreeMap::new();
    for pull in pulls {
        let Some((branch, candidate)) = tea_pr_candidate(pull) else {
            continue;
        };
        links.insert(
            branch.clone(),
            prefer_candidate(links.get(&branch).cloned(), candidate),
        );
    }
    Ok(links)
}

/// Parse the Gitea payload from `tea api repos/<slug>/pulls/<number>`.
pub fn parse_tea_pr_detail_json(raw: &str) -> Result<TeaPrDetail, String> {
    let value: Value = serde_json::from_str(raw).map_err(|err| err.to_string())?;
    let text = |value: Option<&Value>| {
        value
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    Ok(TeaPrDetail {
        state: pr_state_from_value(&value),
        merged_sha: text(value.get("merge_commit_sha")),
        head_sha: text(value.get("head").and_then(|head| head.get("sha"))),
    })
}

/// Build bounded `tea pr list` argv for open-set and transition probes.
pub fn tea_pr_list_args<'a>(state: &'a str, repo: Option<&'a str>) -> Vec<&'a str> {
    let mut args = vec!["pr", "list", "--state", state];
    args.extend([
        "--output",
        "json",
        "--fields",
        TEA_LIST_FIELDS,
        "--limit",
        LIST_LIMIT,
    ]);
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
            ci_from_gh_check_run(
                item.get("status").and_then(Value::as_str),
                item.get("conclusion").and_then(Value::as_str),
            )
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

/// Aggregate GitHub's commit check runs into the worst actionable CI verdict.
pub fn parse_gh_check_runs(raw: &str) -> Result<Option<WorktreePrCi>, String> {
    #[derive(Deserialize)]
    struct Page {
        #[serde(default)]
        check_runs: Vec<CheckRun>,
    }

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Response {
        Page(Page),
        Pages(Vec<Page>),
    }

    #[derive(Deserialize)]
    struct CheckRun {
        status: Option<String>,
        conclusion: Option<String>,
    }

    let pages = match serde_json::from_str(raw).map_err(|err| err.to_string())? {
        Response::Page(page) => vec![page],
        Response::Pages(pages) => pages,
    };
    Ok(pages
        .into_iter()
        .flat_map(|page| page.check_runs)
        .fold(None, |verdict, run| {
            worst_ci(
                verdict,
                ci_from_gh_check_run(run.status.as_deref(), run.conclusion.as_deref()),
            )
        }))
}

/// Parse GitHub's combined commit status into the sidebar CI vocabulary.
pub fn parse_gh_combined_status(raw: &str) -> Result<Option<WorktreePrCi>, String> {
    #[derive(Deserialize)]
    struct Response {
        state: Option<String>,
        #[serde(default)]
        statuses: Vec<Value>,
    }

    let response: Response = serde_json::from_str(raw).map_err(|err| err.to_string())?;
    if response.statuses.is_empty() {
        return Ok(None);
    }
    Ok(response.state.as_deref().and_then(|state| {
        if state.eq_ignore_ascii_case("success") {
            Some(WorktreePrCi::Passing)
        } else if state.eq_ignore_ascii_case("pending") {
            Some(WorktreePrCi::Pending)
        } else if ["failure", "error"]
            .iter()
            .any(|failure| state.eq_ignore_ascii_case(failure))
        {
            Some(WorktreePrCi::Failing)
        } else {
            None
        }
    }))
}

fn ci_from_gh_check_run(status: Option<&str>, conclusion: Option<&str>) -> Option<WorktreePrCi> {
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
    } else if conclusion.is_some_and(|value| {
        ["SUCCESS", "NEUTRAL", "SKIPPED"]
            .iter()
            .any(|passing| value.eq_ignore_ascii_case(passing))
    }) {
        Some(WorktreePrCi::Passing)
    } else {
        None
    }
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

pub(crate) fn worst_ci(
    current: Option<WorktreePrCi>,
    next: Option<WorktreePrCi>,
) -> Option<WorktreePrCi> {
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
    if value.get("merged").and_then(Value::as_bool) == Some(true)
        || value
            .get("merged_at")
            .is_some_and(|merged| !merged.is_null())
    {
        return Some(WorktreePrState::Merged);
    }
    "state status".split_ascii_whitespace().find_map(|field| {
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
    match current {
        Some(current) if pr_state_rank(current.state) >= pr_state_rank(next.state) => current,
        _ => next,
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
    NUMBER_FIELDS.split_ascii_whitespace().find_map(|field| {
        value
            .get(field)
            .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
    })
}

fn tea_pr_candidate(value: &Value) -> Option<(String, PrCandidate)> {
    let candidate = PrCandidate {
        number: pr_number(value)?,
        state: pr_state_from_value(value)?,
        ci: None,
        merge_sha: None,
    };
    Some((pr_head_branch(value)?, candidate))
}

fn pr_head_branch(value: &Value) -> Option<String> {
    HEAD_FIELDS
        .split_ascii_whitespace()
        .find_map(|field| head_branch_from_value(value.get(field)))
}

fn head_branch_from_value(value: Option<&Value>) -> Option<String> {
    let value = value?;
    let raw = value.as_str().or_else(|| {
        HEAD_OBJECT_FIELDS
            .split_ascii_whitespace()
            .find_map(|field| value.get(field)?.as_str())
    })?;
    ref_name_branch(raw)
}

fn ref_name_branch(raw: &str) -> Option<String> {
    let raw = raw.trim();
    (!raw.is_empty()).then(|| raw.rsplit(':').next().unwrap_or(raw).to_owned())
}

#[cfg(test)]
mod tests;
