//! One home for model-id parsing, display names, home-series lookup, and
//! matching keys across CLI, sidebar, and capacity surfaces.

use std::borrow::Cow;

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

/// A rendered model name in reading order: the series word (absent when a
/// Claude line name leads under Claude's own provider, or when none was parsed),
/// the core (a Claude line name and the version; the first word when no series
/// was parsed; empty under a shown series with neither), then the child words
/// in id order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedModel {
    pub(crate) series: Option<String>,
    core: String,
    pub(crate) children: Vec<String>,
}

impl RenderedModel {
    /// Every shown word joined with single spaces.
    pub fn full(&self) -> String {
        let name = self.name(self.children.len());
        match self.series.as_deref() {
            Some(series) if name.is_empty() => series.to_owned(),
            Some(series) => format!("{series} {name}"),
            None => name,
        }
    }

    /// The core and the first `children` child words, without the series.
    pub(crate) fn name(&self, children: usize) -> String {
        std::iter::once(self.core.as_str())
            .chain(self.children.iter().take(children).map(String::as_str))
            .filter(|word| !word.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
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
    /// optional context-window qualifier. Routing prefixes (`provider/.../`),
    /// cloud vendor and region dot-prefixes (`us.anthropic.`), `#variant`
    /// tails, `-v1:0` revision tails, and trailing dates are stripped for
    /// display and matching only; `2p5` reads as `2.5`. A snapshot code (a 4-,
    /// 6- or 8-digit word) after the version extends the version, while an
    /// 8-digit date inside a version leaves it, so a refusal key written as
    /// `claude 4.5.20250929 sonnet v1:0` reads as its id does.
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
        let vendorless = strip_vendor_prefixes(base);
        let base = strip_revision_tail(&vendorless);
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

        let base = spell_point_versions(base);
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
            version: version.map(|version| without_date_components(&version)),
            verbatim: None,
            window,
            has_date_suffix,
        }
    }

    /// Render the model name as series, then a Claude line name, then the
    /// version, then the child words in id order, dropping fast, reasoning,
    /// packaging words, active-parameter tags, and snapshot codes (as words and
    /// as version components). With a series shown and neither a line name nor
    /// a version, the core stays empty and every later word is a child. The series
    /// hides only when it matches `home_series` case-insensitively and a Claude
    /// line name leads; every other series shows, or the version would lead.
    pub fn render(&self, home_series: Option<&str>) -> RenderedModel {
        let Some(series) = self.series.as_deref() else {
            if let Some(verbatim) = self.verbatim.as_deref() {
                return RenderedModel {
                    series: None,
                    core: verbatim.to_owned(),
                    children: Vec::new(),
                };
            }
            // A name with no series (`o4-mini`) keeps its first word as the core.
            let mut words = shown_words(&self.family).into_iter();
            return RenderedModel {
                series: None,
                core: words.next().unwrap_or_default(),
                children: words.collect(),
            };
        };
        // A label such as `Opus 4.8` parses its line name into the series slot.
        let (shown_series, line) = if is_claude_line(series) {
            (None, Some(series.to_owned()))
        } else {
            let line = series
                .eq_ignore_ascii_case("claude")
                .then(|| self.family.iter().find(|word| is_claude_line(word)))
                .flatten()
                .cloned();
            let hidden =
                line.is_some() && home_series.is_some_and(|home| home.eq_ignore_ascii_case(series));
            ((!hidden).then(|| series.to_owned()), line)
        };
        let mut children = shown_words(&self.family);
        if let Some(line) = line.as_deref() {
            children.retain(|word| word != line);
        }
        let version = self.version.as_deref().map(shown_version);
        let core = line
            .iter()
            .chain(version.iter())
            .filter(|word| !word.is_empty())
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(" ");
        RenderedModel {
            series: shown_series,
            core,
            children,
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

/// Render a provider model id for people, hiding `kind`'s home series only
/// where a Claude line name leads (`Opus 4.8` under Claude).
pub fn display_model_for_kind(id: &str, kind: Option<&str>) -> String {
    ParsedModel::parse(id)
        .render(kind.and_then(home_series))
        .full()
}

/// Render a provider model id for people, always keeping the series.
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

/// Strip cloud vendor and region dot-prefixes on the leading word (`us.`,
/// `us-gov.`, `anthropic.`, `meta.`, ...): every segment of letters and
/// hyphens before a dot. A segment whose rest would lead with a word of fewer
/// than two letters (`deepseek.r1-v1:0`) stays as the series.
fn strip_vendor_prefixes(raw: &str) -> Cow<'_, str> {
    let mut rest = raw;
    while let Some((segment, tail)) = rest.split_once('.')
        && segment.starts_with(|ch: char| ch.is_ascii_alphabetic())
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphabetic() || byte == b'-')
        && !tail.is_empty()
    {
        let leading_letters = tail
            .bytes()
            .take_while(|byte| byte.is_ascii_alphabetic())
            .count();
        if leading_letters < 2 {
            return Cow::Owned(format!("{segment}-{tail}"));
        }
        rest = tail;
    }
    Cow::Borrowed(rest)
}

/// Strip a Bedrock revision tail such as `-v1:0` or `-1:0`, after a hyphen
/// or a space.
fn strip_revision_tail(raw: &str) -> &str {
    let Some((head, tail)) = raw.rsplit_once(|ch: char| ch == '-' || ch.is_whitespace()) else {
        return raw;
    };
    let revision = tail.strip_prefix(['v', 'V']).unwrap_or(tail);
    match revision.split_once(':') {
        Some((major, minor))
            if !head.trim_end().is_empty() && is_all_digits(major) && is_all_digits(minor) =>
        {
            head.trim_end()
        }
        _ => raw,
    }
}

/// Read a `NpN` version spelling (`qwen2p5`) as `N.N`.
fn spell_point_versions(raw: &str) -> String {
    let bytes = raw.as_bytes();
    raw.char_indices()
        .map(|(index, ch)| {
            let between_digits = ch == 'p'
                && index > 0
                && bytes[index - 1].is_ascii_digit()
                && bytes.get(index + 1).is_some_and(u8::is_ascii_digit);
            if between_digits { '.' } else { ch }
        })
        .collect()
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

/// Drop a trailing `YYYYMMDD`, `YYYY-MM-DD` or `MM-YYYY` date stamp separated by
/// `-` or whitespace (`MM YYYY` too), returning the trimmed base and whether a
/// stamp was removed.
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
    if bytes.len() > 8 {
        let split = bytes.len() - 8;
        let sep = bytes[split];
        let tail = &bytes[split + 1..];
        let is_month_year = (sep == b'-' || sep.is_ascii_whitespace())
            && tail[0..2].iter().all(u8::is_ascii_digit)
            && matches!((tail[0] - b'0') * 10 + (tail[1] - b'0'), 1..=12)
            && (tail[2] == b'-' || tail[2].is_ascii_whitespace())
            && tail[3..7].iter().all(u8::is_ascii_digit);
        if is_month_year {
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

/// Claude's line names, which lead in place of the series. A new line name
/// joins this list.
const CLAUDE_LINES: [&str; 6] = ["Opus", "Sonnet", "Haiku", "Fable", "Mythos", "Instant"];

fn is_claude_line(word: &str) -> bool {
    CLAUDE_LINES
        .iter()
        .any(|line| line.eq_ignore_ascii_case(word))
}

/// The family words a name shows: every `fast`, `reasoning` and
/// `non-reasoning`, packaging words, active-parameter tags (`a3b`), and
/// snapshot codes drop.
fn shown_words(family: &[String]) -> Vec<String> {
    family
        .iter()
        .enumerate()
        .filter(|(index, word)| {
            let next = family.get(index + 1).map(String::as_str);
            !is_noise_word(word, next)
        })
        .map(|(_, word)| word.clone())
        .collect()
}

fn is_noise_word(word: &str, next: Option<&str>) -> bool {
    const NOISE: [&str; 12] = [
        "fast",
        "reasoning",
        "instruct",
        "it",
        "hf",
        "fp8",
        "fp4",
        "bf16",
        "maas",
        "latest",
        "preview",
        "customtools",
    ];
    let lower = word.to_ascii_lowercase();
    let active_parameters = lower
        .strip_prefix('a')
        .and_then(|rest| rest.strip_suffix('b'))
        .is_some_and(is_numeric_version);
    NOISE.contains(&lower.as_str())
        || (lower == "non" && next.is_some_and(|next| next.eq_ignore_ascii_case("reasoning")))
        || active_parameters
        || is_snapshot_code(word)
}

/// A version as shown: every snapshot-code component drops (`4.0613` shows
/// `4`, `2411` shows nothing).
fn shown_version(version: &str) -> String {
    version
        .split('.')
        .filter(|component| !is_snapshot_code(component))
        .collect::<Vec<_>>()
        .join(".")
}

/// Drop each 8-digit date after a version's first component (`4.5.20250929`
/// reads `4.5`), which only written keys and mid-id dates produce.
fn without_date_components(version: &str) -> String {
    version
        .split('.')
        .enumerate()
        .filter(|(index, component)| {
            *index == 0 || !(is_all_digits(component) && component.len() == 8)
        })
        .map(|(_, component)| component)
        .collect::<Vec<_>>()
        .join(".")
}

/// A 4-, 6- or 8-digit word such as `0324` or `20250929`.
fn is_snapshot_code(word: &str) -> bool {
    is_all_digits(word) && matches!(word.len(), 4 | 6 | 8)
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
