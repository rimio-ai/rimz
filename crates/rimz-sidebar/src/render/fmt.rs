//! Time and text formatting helpers shared by the renderer.

use jiff::Timestamp;

/// Seconds since `at`, clamped at zero — the shared input for [`age_short`] and
/// the staleness color ramp, so a row reads the wall clock once and styles and
/// labels its age from the same number.
pub(super) fn age_secs(at: Timestamp) -> i64 {
    Timestamp::now().duration_since(at).as_secs().max(0)
}

/// A coarse age as its single highest unit (`8s`, `12m`, `3h`, `2d`) — the pure
/// core of [`age_short`], so the styling caller can format from a seconds value
/// it already has.
pub(super) fn age_label(seconds: i64) -> String {
    if seconds <= 0 {
        "0s".to_owned()
    } else if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 60 * 60 {
        format!("{}m", seconds / 60)
    } else if seconds < 60 * 60 * 24 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

pub(super) fn age_short(at: Timestamp) -> String {
    age_label(age_secs(at))
}

/// A row's last-activity age for the work line: floors at `1m` (a sub-minute span
/// reads `1m`, never seconds), shows whole hours `{h}h` from 1h on, and caps at
/// `>1d` from a day on — a coarse "how long since this agent did something" that
/// never competes with the precise worked-time on the same line.
pub(super) fn activity_label(seconds: i64) -> String {
    if seconds < 60 * 60 {
        format!("{}m", (seconds / 60).max(1))
    } else if seconds < 60 * 60 * 24 {
        format!("{}h", seconds / 3_600)
    } else {
        ">1d".to_owned()
    }
}

pub(super) fn activity_short(at: Timestamp) -> String {
    activity_label(age_secs(at))
}

pub(super) fn time_remaining(deadline: Timestamp) -> String {
    let seconds = deadline.duration_since(Timestamp::now()).as_secs();
    if seconds <= 0 {
        "0s".to_owned()
    } else if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m", seconds / 60)
    }
}

pub(super) fn clip(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    value
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>()
        + "..."
}

/// The 5-hour budget window's reset countdown as a fixed `{h}h{mm:02}m`
/// (`4h20m`, `0h45m`, `5h00m`): always two units, so it column-aligns with the
/// 7-day [`reset_days_hours`] beside it. The window caps at 5h, so hours stay a
/// single digit. A passed reset reads `0h00m` (the stable-window selection drops
/// expired readings upstream, so a rendered window is live).
pub(super) fn reset_hours_minutes(deadline: Timestamp) -> String {
    reset_hm(deadline.duration_since(Timestamp::now()).as_secs())
}

fn reset_hm(seconds: i64) -> String {
    let seconds = seconds.max(0);
    format!("{}h{:02}m", seconds / 3_600, seconds % 3_600 / 60)
}

/// The 7-day budget window's reset countdown as a fixed `{d}d{hh:02}h` (`2d23h`,
/// `0d05h`, `6d23h`): always two units, so it column-aligns with the 5-hour
/// [`reset_hours_minutes`]. The window caps at 7d, so days stay a single digit.
pub(super) fn reset_days_hours(deadline: Timestamp) -> String {
    reset_dh(deadline.duration_since(Timestamp::now()).as_secs())
}

fn reset_dh(seconds: i64) -> String {
    let seconds = seconds.max(0);
    format!("{}d{:02}h", seconds / 86_400, seconds % 86_400 / 3_600)
}

/// Spend at full cent resolution — `$0.00`, `$3.50`, `$124.05`. Every spend in
/// the sidebar reads as money at two decimals: the per-row cost, the cockpit
/// fleet total, and the provider dashboard all share this one shape, so a price
/// never jitters between a cents and a whole-dollar form.
pub(super) fn dollars2(usd: f64) -> String {
    format!("${usd:.2}")
}

/// Shorten a model's display name for the capability line. First drops the
/// `context` qualifier from an extended-window suffix (`Opus 4.8 (1M context)`
/// → `Opus 4.8 (1M)`). Then, when the name is still a bare vendor *slug* (all
/// lowercase, hyphenated, no spaces — the pre-enrichment fallback), prettifies
/// it (`claude-opus-4-8` → `Opus 4.8`). A friendly name passes through.
pub(super) fn model_label(display: &str) -> String {
    let cleaned = display.replace(" context)", ")");
    if looks_like_slug(&cleaned) {
        prettify_model_slug(&cleaned)
    } else {
        cleaned
    }
}

/// A name still reads as a raw model slug when it is hyphenated, carries no
/// space, no parenthetical, and no uppercase letter — exactly the shape of a
/// catalog id (`claude-opus-4-8`, `gpt-5.5-codex`) and never of a friendly
/// display name (`Opus 4.8`, `GPT-5.5`), so the prettifier only fires on the
/// fallback path.
fn looks_like_slug(value: &str) -> bool {
    value.contains('-')
        && !value.contains(' ')
        && !value.contains('(')
        && !value.chars().any(|c| c.is_ascii_uppercase())
}

/// Prettify a raw model slug into a display name: drop a leading vendor token
/// so the family name leads, join split version digits with a dot (`4-8` →
/// `4.8`), and title-case the words (acronyms like `gpt` upper-cased), so
/// `claude-opus-4-8` reads `Opus 4.8` and `gpt-5.5-codex` reads `GPT 5.5 Codex`.
fn prettify_model_slug(slug: &str) -> String {
    let segments: Vec<&str> = slug.split('-').filter(|seg| !seg.is_empty()).collect();
    // A leading vendor prefix is redundant with the brand emblem and product
    // header, so the family name leads; a single-segment product keeps its name.
    let start = usize::from(segments.len() > 1 && matches!(segments[0], "claude" | "anthropic"));
    let mut words: Vec<String> = Vec::new();
    for segment in &segments[start..] {
        let is_int = segment.chars().all(|c| c.is_ascii_digit());
        let prev_is_version = words
            .last()
            .is_some_and(|prev| prev.chars().all(|c| c.is_ascii_digit() || c == '.'));
        if is_int && prev_is_version {
            // A split `major-minor`: glue onto the running version (`4` then `8`).
            let version = words.last_mut().expect("prev_is_version implies a word");
            version.push('.');
            version.push_str(segment);
        } else {
            words.push(title_word(segment));
        }
    }
    words.join(" ")
}

/// Title-case one slug segment: known acronyms upper-case, a version-like
/// segment (digits and dots) passes through, every other word capitalizes its
/// first letter.
fn title_word(word: &str) -> String {
    match word {
        "gpt" => "GPT".to_owned(),
        "codex" => "Codex".to_owned(),
        _ if word.chars().all(|c| c.is_ascii_digit() || c == '.') => word.to_owned(),
        _ => {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        }
    }
}

/// A token count as a thin magnitude with no unit suffix — `523`, `76.5k`,
/// `1.2M` — so callers compose it into a label (`{} tok`) or a split line.
pub(super) fn tokens_short(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}k", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}

/// The context bar's right-hand value, sized to fit the bar's 5-cell value
/// column. Prefers a one-decimal precise fraction (`78.2%`) when the caller can
/// derive it from the current-message token composition; otherwise the agent's
/// integer `used_percentage` (`38%`). Clamps to `100%` so it never spills past
/// five cells (`100.0%` would).
pub(super) fn pct_label(precise: Option<f64>, whole: u8) -> String {
    match precise {
        Some(fraction) if fraction >= 99.95 => "100%".to_owned(),
        Some(fraction) => format!("{:.1}%", fraction.clamp(0.0, 100.0)),
        None => format!("{}%", whole.min(100)),
    }
}

/// A span as its two highest non-zero units — `3d12h`, `3h12m`, `2m30s`, `45s`.
/// Skipping zero units keeps it short, so a span with no minutes reads `3h3s`
/// rather than padding a `0m`; a non-positive span is empty (callers special-case
/// it). The shared core behind the worked-time spans.
fn compact_seconds(seconds: i64) -> String {
    if seconds <= 0 {
        return String::new();
    }
    [
        (seconds / 86_400, 'd'),
        (seconds % 86_400 / 3_600, 'h'),
        (seconds % 3_600 / 60, 'm'),
        (seconds % 60, 's'),
    ]
    .into_iter()
    .filter(|(value, _)| *value > 0)
    .take(2)
    .map(|(value, unit)| format!("{value}{unit}"))
    .collect()
}

/// A worked-time span (`12m`, `1h12m`, `3d4h`) from a millisecond duration — the
/// session's `total_duration_ms`. A zero span reads `0s` rather than the empty
/// core, which would misread as missing data.
pub(super) fn duration_worked(ms: u64) -> String {
    let seconds = (ms / 1_000) as i64;
    if seconds <= 0 {
        return "0s".to_owned();
    }
    compact_seconds(seconds)
}

/// Like [`duration_worked`] but never finer than minutes — for the cockpit
/// fleet clock, where a ticking seconds field on an aggregate is noise. Floors
/// to whole minutes (a span under a minute reads `0m`) and takes the two
/// highest non-zero units from `d`/`h`/`m`.
pub(super) fn duration_worked_coarse(ms: u64) -> String {
    let minutes = (ms / 60_000) as i64;
    if minutes <= 0 {
        return "0m".to_owned();
    }
    [
        (minutes / 1_440, 'd'),
        (minutes % 1_440 / 60, 'h'),
        (minutes % 60, 'm'),
    ]
    .into_iter()
    .filter(|(value, _)| *value > 0)
    .take(2)
    .map(|(value, unit)| format!("{value}{unit}"))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_seconds_takes_two_highest_nonzero_units() {
        assert_eq!(compact_seconds(3 * 86_400 + 12 * 3_600), "3d12h");
        assert_eq!(compact_seconds(3 * 3_600 + 12 * 60), "3h12m");
        assert_eq!(compact_seconds(2 * 60 + 30), "2m30s");
        assert_eq!(compact_seconds(45), "45s");
        // Zero minutes between non-zero hours and seconds collapses out.
        assert_eq!(compact_seconds(3 * 3_600 + 3), "3h3s");
        // A non-positive span is the empty core (callers special-case it).
        assert_eq!(compact_seconds(0), "");
    }

    #[test]
    fn reset_labels_are_fixed_two_unit_and_aligned() {
        // 5h window: always `{h}h{mm:02}m` — single-digit hours, padded minutes.
        assert_eq!(reset_hm(4 * 3_600 + 20 * 60), "4h20m");
        assert_eq!(reset_hm(45 * 60), "0h45m");
        assert_eq!(reset_hm(5 * 3_600), "5h00m");
        // 7d window: always `{d}d{hh:02}h` — single-digit days, padded hours.
        assert_eq!(reset_dh(2 * 86_400 + 23 * 3_600), "2d23h");
        assert_eq!(reset_dh(5 * 3_600), "0d05h");
        assert_eq!(reset_dh(6 * 86_400 + 23 * 3_600), "6d23h");
        // Both stay five cells so the two countdowns column-align.
        assert_eq!(reset_hm(45 * 60).chars().count(), 5);
        assert_eq!(reset_dh(5 * 3_600).chars().count(), 5);
        // A passed reset is the zero floor, never negative.
        assert_eq!(reset_hm(-10), "0h00m");
        assert_eq!(reset_dh(-10), "0d00h");
    }

    #[test]
    fn model_label_drops_context_qualifier() {
        assert_eq!(model_label("Opus 4.8 (1M context)"), "Opus 4.8 (1M)");
        assert_eq!(model_label("Opus 4.8"), "Opus 4.8");
        assert_eq!(model_label("GPT-5.5"), "GPT-5.5");
    }

    #[test]
    fn model_label_prettifies_a_bare_slug() {
        // A pre-enrichment slug has no friendly display name to prefer, so the
        // fallback cleans it: vendor prefix dropped, split version glued, words
        // title-cased.
        assert_eq!(model_label("claude-opus-4-8"), "Opus 4.8");
        assert_eq!(model_label("gpt-5.5-codex"), "GPT 5.5 Codex");
        // A friendly name (space or uppercase) is never mistaken for a slug.
        assert_eq!(model_label("GPT-5.5"), "GPT-5.5");
        assert_eq!(model_label("Opus 4.8 (1M)"), "Opus 4.8 (1M)");
    }

    #[test]
    fn dollars2_is_always_two_decimals() {
        assert_eq!(dollars2(0.0), "$0.00");
        assert_eq!(dollars2(3.5), "$3.50");
        assert_eq!(dollars2(3.276), "$3.28");
        assert_eq!(dollars2(124.0), "$124.00");
    }

    #[test]
    fn duration_worked_coarse_floors_to_minutes() {
        assert_eq!(duration_worked_coarse(13 * 60_000 + 3_000), "13m"); // 13m3s → 13m
        assert_eq!(duration_worked_coarse(60 * 60_000 + 12 * 60_000), "1h12m");
        assert_eq!(
            duration_worked_coarse(3 * 86_400_000 + 4 * 3_600_000),
            "3d4h"
        );
        assert_eq!(duration_worked_coarse(30_000), "0m"); // sub-minute floors to 0m
        assert_eq!(duration_worked_coarse(0), "0m");
    }

    #[test]
    fn tokens_short_scales_by_magnitude() {
        assert_eq!(tokens_short(523), "523");
        assert_eq!(tokens_short(76_500), "76.5k");
        assert_eq!(tokens_short(1_200_000), "1.2M");
    }

    #[test]
    fn pct_label_prefers_precise_decimal_then_clamps() {
        assert_eq!(pct_label(Some(78.23), 78), "78.2%");
        assert_eq!(pct_label(Some(9.9), 9), "9.9%");
        // A precise value within rounding of full reads `100%`, never `100.0%`.
        assert_eq!(pct_label(Some(99.96), 99), "100%");
        assert_eq!(pct_label(Some(100.0), 100), "100%");
        // No breakdown: the integer gauge value, also clamped.
        assert_eq!(pct_label(None, 38), "38%");
        assert_eq!(pct_label(None, 200), "100%");
        // Every rendering fits the 5-cell value column.
        for s in [
            pct_label(Some(78.23), 78),
            pct_label(Some(100.0), 100),
            pct_label(None, 38),
        ] {
            assert!(s.chars().count() <= 5, "{s:?} exceeds 5 cells");
        }
    }

    #[test]
    fn duration_worked_spans_two_units_and_floors_zero() {
        assert_eq!(duration_worked(720_000), "12m"); // 12 minutes
        assert_eq!(duration_worked(4_320_000), "1h12m"); // 1h12m
        assert_eq!(duration_worked(0), "0s");
        assert_eq!(duration_worked(500), "0s"); // sub-second floors to 0s, not "now"
    }

    #[test]
    fn activity_label_floors_at_one_minute_and_caps_at_a_day() {
        // Sub-minute and the first minute both floor to `1m` — never seconds.
        assert_eq!(activity_label(0), "1m");
        assert_eq!(activity_label(59), "1m");
        assert_eq!(activity_label(90), "1m");
        assert_eq!(activity_label(120), "2m");
        assert_eq!(activity_label(59 * 60), "59m");
        // Whole hours from 1h on, capped at `>1d` from a day on.
        assert_eq!(activity_label(60 * 60), "1h");
        assert_eq!(activity_label(23 * 3_600), "23h");
        assert_eq!(activity_label(24 * 3_600), ">1d");
        assert_eq!(activity_label(100 * 3_600), ">1d");
    }
}
