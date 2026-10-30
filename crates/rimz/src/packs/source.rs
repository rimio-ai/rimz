use super::*;

const OFFICIAL: [(&str, &str); 2] = [
    ("crew", "https://github.com/rimio-ai/rimz-crew"),
    ("skills", "https://github.com/rimio-ai/rimz-skills"),
];

pub(super) fn parse(value: &str) -> Result<GitSource> {
    let invalid = || PackErr::NotPath {
        argument: value.to_owned(),
    };
    if value.is_empty()
        || value
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return Err(invalid());
    }
    let after_host = if let Some(scheme) = value.find("://") {
        let authority = scheme + 3;
        value[authority..]
            .find('/')
            .map_or(value.len(), |slash| authority + slash)
    } else {
        separator(value).map_or(0, |colon| colon + 1)
    };
    let (location, reference) = match value[after_host..].find('@') {
        Some(at) => {
            let at = after_host + at;
            let reference = &value[at + 1..];
            if reference.is_empty() {
                return Err(invalid());
            }
            (&value[..at], Some(reference.to_owned()))
        }
        None => (value, None),
    };
    let mut parts = location.split('/');
    let first = parts.next().unwrap_or_default();
    let (url, pack) = if let Some((_, url)) = OFFICIAL.iter().find(|(name, _)| *name == first) {
        let pack = parts.next();
        if parts.next().is_some() {
            return Err(invalid());
        }
        (url.to_string(), pack)
    } else if let Some(github) = location.strip_prefix("github:") {
        let parts: Vec<_> = github.split('/').collect();
        if !(2..=3).contains(&parts.len()) || !component(parts[0]) || !component(parts[1]) {
            return Err(invalid());
        }
        (
            format!("https://github.com/{}/{}", parts[0], parts[1]),
            parts.get(2).copied(),
        )
    } else if location.contains("://") {
        (location.to_owned(), None)
    } else if let Some(colon) = separator(location) {
        let host = &location[..colon];
        let path = &location[colon + 1..];
        if host.is_empty()
            || host.starts_with('-')
            || host.contains('/')
            || path.is_empty()
            || path.starts_with(':')
            || path.trim_end_matches('/').contains(".git/")
        {
            return Err(invalid());
        }
        return Ok(GitSource {
            source: identity(location).to_owned(),
            url: location.to_owned(),
            pack: None,
            reference,
        });
    } else {
        return Err(invalid());
    };
    let mut normalized = url::Url::parse(&url).map_err(|_| invalid())?;
    if normalized.path().contains(".git/") && !normalized.path().ends_with(".git/") {
        return Err(invalid());
    }
    let path = identity(normalized.path()).to_owned();
    normalized.set_path(&path);
    if normalized.host_str() == Some("github.com") {
        let parts: Vec<_> = normalized
            .path()
            .trim_start_matches('/')
            .split('/')
            .collect();
        if parts.len() != 2 || parts.iter().any(|part| !component(part)) {
            return Err(invalid());
        }
    }
    Ok(GitSource {
        source: normalized.to_string().trim_end_matches('/').to_owned(),
        url,
        pack: pack.map(str::parse).transpose().map_err(|_| invalid())?,
        reference,
    })
}

fn identity(value: &str) -> &str {
    let value = value.trim_end_matches('/');
    value.strip_suffix(".git").unwrap_or(value)
}

fn separator(value: &str) -> Option<usize> {
    value
        .find("]:")
        .map(|end| end + 1)
        .or_else(|| value.find(':'))
}

fn component(value: &str) -> bool {
    !value.is_empty()
        && !matches!(value, "." | "..")
        && value
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'_' | b'-' | b'.'))
}

pub(super) fn requirement(value: &str, parent: &str) -> Option<GitSource> {
    if let Ok(pack) = value.parse() {
        return Some(GitSource {
            source: parent.to_owned(),
            url: parent.to_owned(),
            pack: Some(pack),
            reference: None,
        });
    }
    if !["crew/", "skills/", "github:"]
        .iter()
        .any(|prefix| value.starts_with(prefix))
    {
        return None;
    }
    let source = parse(value).ok()?;
    (source.pack.is_some() && source.reference.is_none()).then_some(source)
}

impl GitSource {
    pub(super) fn official(&self) -> Option<&'static str> {
        OFFICIAL
            .iter()
            .find_map(|(name, url)| (self.source == *url).then_some(*name))
    }

    pub fn spelling(&self) -> String {
        let mut spelling = self.official().map(str::to_owned).unwrap_or_else(|| {
            url::Url::parse(&self.url)
                .ok()
                .filter(|url| {
                    url.scheme() == "https"
                        && url.host_str() == Some("github.com")
                        && url.username().is_empty()
                        && url.password().is_none()
                        && url.port().is_none()
                        && url.query().is_none()
                        && url.fragment().is_none()
                })
                .map_or_else(
                    || self.url.clone(),
                    |url| format!("github:{}", url.path().trim_matches('/')),
                )
        });
        if (self.official().is_some() || spelling.starts_with("github:"))
            && let Some(pack) = &self.pack
        {
            spelling.push('/');
            spelling.push_str(pack.as_str());
        }
        spelling
    }
}
