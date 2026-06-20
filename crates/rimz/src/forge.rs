//! Forge pull-request ref parsing.
//!
//! Rimz talks to forges through Git refs only. This module identifies the PR
//! number and the forge ref shape without depending on a forge CLI or API.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Forge {
    GitHubStyle,
    GitLab,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrTarget {
    pub number: u64,
    pub forge: Option<Forge>,
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

fn remote_host(remote_url: &str) -> &str {
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
