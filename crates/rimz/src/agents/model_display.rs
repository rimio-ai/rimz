//! One home for model-id parsing, display names, home-series lookup, and
//! matching keys across CLI, sidebar, and capacity surfaces.

/// A parsed model name split into its series, family words, version, and
/// optional context-window qualifier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedModel {
    series: Option<String>,
    family: Vec<String>,
    version: Option<String>,
    verbatim: Option<String>,
    window: Option<String>,
    has_date_suffix: bool,
}

/// A rendered model name split into its optional series prefix (omitted when
/// the adjacent agent's home series implies it or when there is no family) and
/// its main family-and-version text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedModel {
    pub series: Option<String>,
    pub name: String,
}

impl RenderedModel {
    /// Join the optional series prefix and main name with ` · `.
    pub fn full(&self) -> String {
        match self.series.as_deref() {
            Some(series) => format!("{series} · {}", self.name),
            None => self.name.clone(),
        }
    }
}

/// Canonical `(name, version)` pair used by quota and refusal matching.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ModelMatchKey {
    pub name: String,
    pub version: Option<String>,
}

impl ModelMatchKey {
    /// Whether this model spends a `model:<scope>` quota sub-cap: the scope's
    /// name matches as a leading run of whole words of this model's name, and
    /// either the scope carries no version or both versions match.
    pub(super) fn matches_scope(&self, scope: &Self) -> bool {
        let name_matches = self.name == scope.name
            || self
                .name
                .strip_prefix(scope.name.as_str())
                .is_some_and(|rest| rest.starts_with(' '));
        name_matches && (scope.version.is_none() || self.version == scope.version)
    }

    /// Whether a refusal recorded under `self` covers `other`: same family
    /// name, and either side is versionless (a bare family alias) or both
    /// carry the same version.
    pub(super) fn covers(&self, other: &Self) -> bool {
        self.name == other.name
            && (self.version.is_none() || other.version.is_none() || self.version == other.version)
    }

    /// Whether a clean turn on `self` clears a refusal recorded under
    /// `recorded`: same family name, and `recorded` has the same version or no
    /// version. A bare alias never clears a specific versioned refusal.
    pub(super) fn clears_refusal(&self, recorded: &Self) -> bool {
        self.name == recorded.name
            && (self.version == recorded.version || recorded.version.is_none())
    }
}

impl ParsedModel {
    /// Parse a model id or display label into its series, family, version, and
    /// optional context-window qualifier. Routing prefixes (`provider/.../`)
    /// and `#variant` tails are stripped for display and matching only.
    pub fn parse(raw: &str) -> Self {
        let trimmed = raw.trim();
        let (without_window, mut window) = strip_window_qualifier(trimmed);
        let without_effort = strip_effort_suffix(without_window);
        let without_variant = strip_variant_suffix(without_effort);
        let base = if window.is_none() {
            let (stripped, found) = strip_window_qualifier(without_variant);
            window = found;
            stripped
        } else {
            without_variant
        };
        let base = strip_factory_custom_selector(base).unwrap_or(base);
        let base = strip_routing_prefix(base);
        let (base, has_date_suffix) = strip_date_suffix(base);
        let base = base.trim();
        if base.is_empty() {
            return Self {
                series: None,
                family: Vec::new(),
                version: None,
                verbatim: None,
                window,
                has_date_suffix,
            };
        }
        // Any remaining parentheses or brackets (after valid window stripping)
        // signal a provider label or variant (`Orchestrator (Neon)`), kept as is.
        if base.contains(['(', ')', '[', ']']) {
            return Self {
                series: None,
                family: Vec::new(),
                version: None,
                verbatim: Some(base.to_owned()),
                window,
                has_date_suffix,
            };
        }

        let words: Vec<&str> = base
            .split(|ch: char| ch == '-' || ch.is_whitespace())
            .filter(|word| !word.is_empty())
            .collect();
        let Some(&first) = words.first() else {
            return Self {
                series: None,
                family: Vec::new(),
                version: None,
                verbatim: None,
                window,
                has_date_suffix,
            };
        };

        let lead = first
            .bytes()
            .take_while(|byte| byte.is_ascii_alphabetic())
            .count();
        let first_rest = &first[lead..];

        if words.len() == 1 {
            if lead >= 2 && is_numeric_version(first_rest) {
                return Self {
                    series: Some(title_word(&first[..lead])),
                    family: Vec::new(),
                    version: Some(first_rest.to_owned()),
                    verbatim: None,
                    window,
                    has_date_suffix,
                };
            }
            return Self {
                series: None,
                family: Vec::new(),
                version: None,
                verbatim: Some(title_word(first)),
                window,
                has_date_suffix,
            };
        }

        // A single-letter (or non-letter) leading token is not a series:
        // keep the name joined and show it whole (`o4-mini` -> `O4 Mini`).
        if lead <= 1 {
            return Self {
                series: None,
                family: words.into_iter().map(title_word).collect(),
                version: None,
                verbatim: None,
                window,
                has_date_suffix,
            };
        }

        let (series, mut version, mut version_open) = if first_rest.is_empty() {
            (title_word(first), None, false)
        } else if is_numeric_version(first_rest) {
            (
                title_word(&first[..lead]),
                Some(first_rest.to_owned()),
                true,
            )
        } else {
            (title_word(first), None, false)
        };

        let mut family = Vec::new();
        for &word in &words[1..] {
            if version.is_none() {
                if is_numeric_version(word) {
                    version = Some(word.to_owned());
                    version_open = true;
                } else if let Some(prefixed) = prefixed_version(word) {
                    version = Some(prefixed);
                    version_open = true;
                } else {
                    family.push(title_word(word));
                }
            } else if version_open && is_all_digits(word) {
                if let Some(current) = version.as_mut() {
                    current.push('.');
                    current.push_str(word);
                }
            } else {
                version_open = false;
                family.push(title_word(word));
            }
        }

        Self {
            series: Some(series),
            family,
            version,
            verbatim: None,
            window,
            has_date_suffix,
        }
    }

    /// Render the model name, hiding `home_series` when it matches the parsed
    /// series case-insensitively and a family is present.
    pub fn render(&self, home_series: Option<&str>) -> RenderedModel {
        let Some(series) = self.series.as_deref() else {
            return RenderedModel {
                series: None,
                name: self
                    .verbatim
                    .clone()
                    .unwrap_or_else(|| self.family.join(" ")),
            };
        };
        if self.family.is_empty() {
            let name = match self.version.as_deref() {
                Some(version) => format!("{series} {version}"),
                None => series.to_owned(),
            };
            return RenderedModel { series: None, name };
        }
        let mut name = self.family.join(" ");
        if let Some(version) = self.version.as_deref() {
            name.push(' ');
            name.push_str(version);
        }
        let shown_series = match home_series {
            Some(home) if home.eq_ignore_ascii_case(series) => None,
            _ => Some(series.to_owned()),
        };
        RenderedModel {
            series: shown_series,
            name,
        }
    }

    pub fn window(&self) -> Option<&str> {
        self.window.as_deref()
    }

    pub(super) fn series(&self) -> Option<&str> {
        self.series.as_deref()
    }

    pub(super) fn family(&self) -> &[String] {
        &self.family
    }

    pub(super) fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    pub(super) fn has_date_suffix(&self) -> bool {
        self.has_date_suffix
    }

    pub(super) fn match_key(&self) -> Option<ModelMatchKey> {
        let name = if !self.family.is_empty() {
            self.family.join(" ").to_ascii_lowercase()
        } else if let Some(series) = self.series.as_deref() {
            series.to_ascii_lowercase()
        } else {
            self.verbatim.as_deref()?.to_ascii_lowercase()
        };
        (!name.is_empty()).then(|| ModelMatchKey {
            name,
            version: self.version.as_deref().map(str::to_ascii_lowercase),
        })
    }

    /// Canonical refusal-record key: lowercased `series version family` (or
    /// the lowercased family/verbatim name when no series was parsed), so
    /// re-parsing any written key reproduces the exact same [`ModelMatchKey`]
    /// even when a family holds numeric or prefixed-version words.
    pub(super) fn refusal_key(&self) -> String {
        let mut parts = Vec::new();
        if let Some(series) = self.series.as_deref() {
            parts.push(series.to_ascii_lowercase());
            if let Some(version) = self.version.as_deref() {
                parts.push(version.to_ascii_lowercase());
            }
            for word in &self.family {
                parts.push(word.to_ascii_lowercase());
            }
        } else if !self.family.is_empty() {
            for word in &self.family {
                parts.push(word.to_ascii_lowercase());
            }
        } else if let Some(verbatim) = self.verbatim.as_deref() {
            parts.push(verbatim.to_ascii_lowercase());
        }
        parts.join(" ")
    }
}

/// The home series declared for `kind`, if the adapter implies one.
pub fn home_series(kind: &str) -> Option<&'static str> {
    super::registry::spec_by_kind(kind)?
        .launch
        .definitions
        .home_series
}

/// The adapter kind whose declared home series matches `series`
/// case-insensitively, if any.
pub(crate) fn home_series_owner(series: &str) -> Option<&'static str> {
    super::registry::BUILTINS.iter().find_map(|definition| {
        let spec = definition.spec();
        spec.launch
            .definitions
            .home_series
            .filter(|home| home.eq_ignore_ascii_case(series))
            .map(|_| spec.kind)
    })
}

/// Render a provider model id for people, hiding `kind`'s home series when
/// implied.
pub fn display_model_for_kind(id: &str, kind: Option<&str>) -> String {
    ParsedModel::parse(id)
        .render(kind.and_then(home_series))
        .full()
}

/// Render a provider model id for people, always keeping the series when a
/// family is present.
pub fn display_model(id: &str) -> String {
    ParsedModel::parse(id).render(None).full()
}

/// Choose the cleanest model string from provider observations: prefer the id
/// (`context_id`, then `launch_model`) unless it parses without a version while
/// `display_name` holds a digit (for example Cursor's `auto` beside
/// `GPT-5.6 Sol` or `GPT-4o Mini`), falling back to `display_name` when no id
/// is present.
pub fn model_label_source<'a>(
    display_name: Option<&'a str>,
    context_id: Option<&'a str>,
    launch_model: Option<&'a str>,
) -> Option<&'a str> {
    let display_name = display_name.filter(|value| !value.trim().is_empty());
    let id = context_id
        .filter(|value| !value.trim().is_empty())
        .or_else(|| launch_model.filter(|value| !value.trim().is_empty()));
    match (id, display_name) {
        (Some(id), Some(display))
            if ParsedModel::parse(id).version().is_none()
                && display.bytes().any(|byte| byte.is_ascii_digit()) =>
        {
            Some(display)
        }
        (Some(id), _) => Some(id),
        (None, display) => display,
    }
}

/// A model's display name and optional compact context-window qualifier.
pub fn model_label_parts(raw: &str) -> (String, Option<String>) {
    let parsed = ParsedModel::parse(raw);
    (
        parsed.render(None).full(),
        parsed.window().map(str::to_owned),
    )
}

/// Strip Factory's legacy custom selector wrapper
/// `custom:<display-name-slug>-<zero-based-index>`, returning the inner slug
/// unformatted. The terminal decimal is protocol bookkeeping, not part of the
/// model name.
pub(super) fn strip_factory_custom_selector(id: &str) -> Option<&str> {
    let selector = id.trim().strip_prefix("custom:")?;
    let (display_slug, index) = selector.rsplit_once('-')?;
    if display_slug.is_empty()
        || index.is_empty()
        || !index.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    Some(display_slug)
}

/// Context windows truncate to whole units, with lowercase `k` and uppercase `M`.
pub(crate) fn window_short(window: u64) -> String {
    if window >= 1_000_000 {
        format!("{}M", window / 1_000_000)
    } else if window >= 1_000 {
        format!("{}k", window / 1_000)
    } else {
        window.to_string()
    }
}

fn strip_window_qualifier(raw: &str) -> (&str, Option<String>) {
    let trimmed = raw.trim_end();
    let qualifier = trimmed
        .strip_suffix(']')
        .and_then(|head| head.rsplit_once('['))
        .or_else(|| {
            trimmed
                .strip_suffix(')')
                .and_then(|head| head.rsplit_once(" ("))
        });
    let Some((name, qualifier)) = qualifier else {
        return (trimmed, None);
    };
    let magnitude = qualifier.strip_suffix(" context").unwrap_or(qualifier);
    let (digits, multiplier) = match magnitude.as_bytes().last() {
        Some(b'k' | b'K') => (&magnitude[..magnitude.len() - 1], 1_000),
        Some(b'm' | b'M') => (&magnitude[..magnitude.len() - 1], 1_000_000),
        _ => return (trimmed, None),
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return (trimmed, None);
    }
    let window = digits
        .parse::<u64>()
        .ok()
        .and_then(|count| count.checked_mul(multiplier));
    match window {
        Some(window) => (name.trim_end(), Some(window_short(window))),
        None => (trimmed, None),
    }
}

fn strip_effort_suffix(raw: &str) -> &str {
    let trimmed = raw.trim_end();
    let Some((head, tail)) = trimmed.rsplit_once('@') else {
        return trimmed;
    };
    let head = head.trim_end();
    if head.is_empty()
        || tail.is_empty()
        || tail
            .chars()
            .any(|ch| ch.is_whitespace() || matches!(ch, '[' | ']' | '(' | ')'))
    {
        return trimmed;
    }
    head
}

/// Strip a provider routing prefix through the last `/` (such as `moonshot/` or
/// `openrouter/anthropic/`) for display and matching only.
fn strip_routing_prefix(raw: &str) -> &str {
    match raw.rsplit_once('/') {
        Some((prefix, rest))
            if !prefix.is_empty()
                && !rest.trim().is_empty()
                && !prefix.chars().any(char::is_whitespace) =>
        {
            rest.trim()
        }
        _ => raw,
    }
}

/// Strip a `#variant` tail (such as `#high` or `#fast`) for display and
/// matching only.
fn strip_variant_suffix(raw: &str) -> &str {
    match raw.split_once('#') {
        Some((head, tail)) if !head.trim().is_empty() && !tail.chars().any(char::is_whitespace) => {
            head.trim_end()
        }
        _ => raw,
    }
}

/// Drop a trailing `YYYYMMDD` or `YYYY-MM-DD` date stamp separated by `-` or
/// whitespace, returning the trimmed base and whether a stamp was removed.
fn strip_date_suffix(raw: &str) -> (&str, bool) {
    let trimmed = raw.trim_end();
    let bytes = trimmed.as_bytes();
    if bytes.len() > 11 {
        let split = bytes.len() - 11;
        let sep = bytes[split];
        let tail = &bytes[split + 1..];
        let is_iso_date = (sep == b'-' || sep.is_ascii_whitespace())
            && tail[0..4].iter().all(u8::is_ascii_digit)
            && tail[4] == b'-'
            && tail[5..7].iter().all(u8::is_ascii_digit)
            && tail[7] == b'-'
            && tail[8..10].iter().all(u8::is_ascii_digit);
        if is_iso_date {
            let base = trimmed[..split].trim_end();
            if !base.is_empty() {
                return (base, true);
            }
        }
    }
    if bytes.len() > 9 {
        let split = bytes.len() - 9;
        let sep = bytes[split];
        let tail = &bytes[split + 1..];
        if (sep == b'-' || sep.is_ascii_whitespace()) && tail.iter().all(u8::is_ascii_digit) {
            let base = trimmed[..split].trim_end();
            if !base.is_empty() {
                return (base, true);
            }
        }
    }
    (trimmed, false)
}

fn is_all_digits(word: &str) -> bool {
    !word.is_empty() && word.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_numeric_version(word: &str) -> bool {
    !word.is_empty() && word.split('.').all(is_all_digits)
}

fn prefixed_version(word: &str) -> Option<String> {
    let first = *word.as_bytes().first()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    let rest = &word[1..];
    is_numeric_version(rest).then(|| format!("{}{rest}", (first as char).to_ascii_uppercase()))
}

fn canonical_series_word(word: &str) -> Option<&'static str> {
    super::registry::BUILTINS.iter().find_map(|definition| {
        definition
            .spec()
            .launch
            .definitions
            .home_series
            .filter(|home| home.eq_ignore_ascii_case(word))
    })
}

/// Title-case one segment: declared series names keep their canonical casing,
/// `DeepSeek` keeps its camel case, numeric tokens pass through, and every
/// other word capitalizes its first character.
fn title_word(word: &str) -> String {
    if let Some(series) = canonical_series_word(word) {
        return series.to_owned();
    }
    if word.eq_ignore_ascii_case("deepseek") {
        return "DeepSeek".to_owned();
    }
    if word
        .bytes()
        .all(|byte| byte.is_ascii_digit() || byte == b'.')
    {
        return word.to_owned();
    }
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests;
