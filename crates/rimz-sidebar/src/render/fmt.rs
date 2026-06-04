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

/// A row's last-activity age, floored to its highest whole unit: `{m}m` up to an
/// hour, whole hours `{h}h` from 1h on, capped at `>1d` from a day on — a coarse
/// "how long since this agent last did something". Sub-minute ages never reach
/// here; [`activity_short`] withholds them so the card stays quiet until a real
/// gap opens.
pub(super) fn activity_label(seconds: i64) -> String {
    if seconds < 60 * 60 {
        format!("{}m", seconds / 60)
    } else if seconds < 60 * 60 * 24 {
        format!("{}h", seconds / 3_600)
    } else {
        ">1d".to_owned()
    }
}

/// The last-activity age once it crosses a full minute (floored), or `None` while
/// it is still sub-minute — a just-active agent shows nothing rather than a
/// misleading `1m`, so the age surfaces only once a real gap has opened.
pub(super) fn activity_short(at: Timestamp) -> Option<String> {
    let seconds = age_secs(at);
    (seconds >= 60).then(|| activity_label(seconds))
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

/// A budget window's reset countdown, two units scaled to how much time is left:
/// `{d}d{hh:02}h` at a day or more (`30d10h`, `6d23h`, `1d02h`), `{h}h{mm:02}m`
/// under a day (`5h00m`, `0h45m`). Both forms hold to five cells under a 99-day
/// reset, so countdowns in one panel column-align. A passed reset reads `0h00m`
/// (the stable-window selection drops expired readings upstream, so a rendered
/// window is live).
pub(super) fn reset_countdown(deadline: Timestamp) -> String {
    reset_secs(deadline.duration_since(Timestamp::now()).as_secs())
}

fn reset_secs(seconds: i64) -> String {
    let seconds = seconds.max(0);
    if seconds >= 86_400 {
        format!("{}d{:02}h", seconds / 86_400, seconds % 86_400 / 3_600)
    } else {
        format!("{}h{:02}m", seconds / 3_600, seconds % 3_600 / 60)
    }
}

/// A budget window's bar label from its length in minutes: hours under a day
/// (`5h`), days at a day or more (`7d`, `30d`), each rounded to its nearest unit.
/// `None` (an unknown length) yields an empty label.
pub(super) fn window_label(duration_mins: Option<u32>) -> String {
    let Some(mins) = duration_mins else {
        return String::new();
    };
    if mins < 24 * 60 {
        format!("{}h", (mins + 30) / 60)
    } else {
        format!("{}d", (mins + 720) / 1_440)
    }
}

/// Spend at full cent resolution with thousands grouped — `$0.00`, `$3.50`,
/// `$124.05`, `$1,240.57`. Every spend in the sidebar reads as money at two
/// decimals: the per-row cost, the cockpit's count-up today total, the provider
/// dashboard, and the fleet ledger all share this one shape, so a price never
/// jitters between a cents and a whole-dollar form. Grouping keeps a large
/// accumulating pile (`$12,480.13`) legible without changing that shape.
pub(super) fn dollars2(usd: f64) -> String {
    // Work in integer cents so rounding matches `{:.2}` and grouping is exact.
    let cents = (usd.max(0.0) * 100.0).round() as u64;
    format!("${}.{:02}", group_thousands(cents / 100), cents % 100)
}

/// Insert `,` every three digits from the right — `1240` → `1,240`,
/// `47200000` → `47,200,000`. The shared grouping behind [`dollars2`].
fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    let len = digits.len();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Shorten a model's display name for the capability line. First drops a
/// trailing context-window parenthetical (`Opus 4.8 (1M context)` / `Opus 4.8
/// (1M)` → `Opus 4.8`) — the identity line's dedicated window token carries
/// that figure now, so the name never repeats it. Then, when the name is still
/// a bare vendor *slug* (all lowercase, hyphenated, no spaces — the
/// pre-enrichment fallback), prettifies it (`claude-opus-4-8` → `Opus 4.8`). A
/// friendly name passes through.
pub(super) fn model_label(display: &str) -> String {
    let cleaned = strip_window_qualifier(display);
    if looks_like_slug(&cleaned) {
        prettify_model_slug(&cleaned)
    } else {
        cleaned
    }
}

/// Drop a trailing `(…)` suffix when it reads as a context-window size — a
/// magnitude like `1M` or `200K`, optionally followed by ` context`. Any other
/// parenthetical (a genuine name qualifier) passes through untouched.
fn strip_window_qualifier(display: &str) -> String {
    let trimmed = display.trim_end();
    let stripped = trimmed
        .strip_suffix(')')
        .and_then(|head| head.rsplit_once(" ("))
        .and_then(|(name, qualifier)| {
            let magnitude = qualifier.strip_suffix(" context").unwrap_or(qualifier);
            let (digits, unit) = magnitude.split_at(magnitude.len().saturating_sub(1));
            let is_window = !digits.is_empty()
                && digits.chars().all(|c| c.is_ascii_digit())
                && matches!(unit, "k" | "K" | "m" | "M");
            is_window.then(|| name.trim_end().to_owned())
        });
    stripped.unwrap_or_else(|| trimmed.to_owned())
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

/// A token count as a whole-unit magnitude with no decimal — `523`, `76k`,
/// `1M`, `2B` — for the agent card and the live cockpit / provider lines, where a
/// tenths place is noise beside the precise `76.5k` the W/M ledger rows carry.
/// Truncates to the unit (`76_500` → `76k`), matching the live figures' coarser
/// read; sub-thousand counts stay exact.
pub(super) fn tokens_int(count: u64) -> String {
    if count >= 1_000_000_000 {
        format!("{}B", count / 1_000_000_000)
    } else if count >= 1_000_000 {
        format!("{}M", count / 1_000_000)
    } else if count >= 1_000 {
        format!("{}k", count / 1_000)
    } else {
        count.to_string()
    }
}

/// The model's context window as a compact lowercase magnitude — `200k`,
/// `272k`, `1m` — for the identity line's capability token. Lowercase `m`
/// keeps the window class quiet beside the model name; the count-of-tokens
/// figures elsewhere keep [`tokens_int`]'s uppercase `M`.
pub(super) fn window_short(window: u64) -> String {
    if window >= 1_000_000 {
        format!("{}m", window / 1_000_000)
    } else if window >= 1_000 {
        format!("{}k", window / 1_000)
    } else {
        window.to_string()
    }
}

/// A token count as a thin magnitude with no unit suffix — `523`, `76.5k`,
/// `1.2M`, `1.2B` — so callers compose it into a label (`{} tok`) or a split
/// line. The `B` tier keeps an all-time token pile compact as it crosses a
/// billion.
pub(super) fn tokens_short(count: u64) -> String {
    if count >= 1_000_000_000 {
        format!("{:.1}B", count as f64 / 1_000_000_000.0)
    } else if count >= 1_000_000 {
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

/// CPU utilisation as `11%` (integer; a u16 covers multi-core bursts).
pub(super) fn fmt_cpu(pct: u16) -> String {
    format!("{pct}%")
}

/// RSS in a compact form: `45k`, `234M`, `1.1G` (one decimal for ≥1 GiB,
/// integer otherwise). Input is in kibibytes.
pub(super) fn fmt_rss(rss_kb: u64) -> String {
    if rss_kb >= 1_048_576 {
        format!("{:.1}G", rss_kb as f64 / 1_048_576.0)
    } else if rss_kb >= 1_024 {
        format!("{}M", rss_kb / 1_024)
    } else {
        format!("{rss_kb}k")
    }
}

/// IO rate as `3M/s`, `450k/s`, `12B/s` (bytes/s, integer magnitude).
pub(super) fn fmt_io(bps: u64) -> String {
    if bps >= 1_048_576 {
        format!("{}M/s", bps / 1_048_576)
    } else if bps >= 1_024 {
        format!("{}k/s", bps / 1_024)
    } else {
        format!("{bps}B/s")
    }
}

/// A span as its two highest non-zero units — `3d12h`, `3h12m`, `2m30s`, `45s`.
/// Skipping zero units keeps it short, so a span with no minutes reads `3h3s`
/// rather than padding a `0m`; a non-positive span is empty (the caller only
/// paints a positive elapsed). Formats the subagent elapsed-work readout.
pub(super) fn compact_seconds(seconds: i64) -> String {
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
#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn reset_countdown_scales_units_to_time_left() {
        // Under a day: `{h}h{mm:02}m` — single-digit hours, padded minutes.
        assert_eq!(reset_secs(4 * 3_600 + 20 * 60), "4h20m");
        assert_eq!(reset_secs(45 * 60), "0h45m");
        // A day or more: `{d}d{hh:02}h` — padded hours.
        assert_eq!(reset_secs(86_400), "1d00h");
        assert_eq!(reset_secs(6 * 86_400 + 23 * 3_600), "6d23h");
        // A ~30-day window's reset stays compact: `30d10h`.
        assert_eq!(reset_secs(30 * 86_400 + 10 * 3_600), "30d10h");
        // Five cells under 10 days, so same-magnitude countdowns column-align.
        assert_eq!(reset_secs(45 * 60).chars().count(), 5);
        assert_eq!(reset_secs(5 * 86_400).chars().count(), 5);
        // A passed reset is the zero floor, never negative.
        assert_eq!(reset_secs(-10), "0h00m");
    }

    #[test]
    fn compact_seconds_takes_two_highest_nonzero_units() {
        assert_eq!(compact_seconds(3 * 86_400 + 12 * 3_600), "3d12h");
        assert_eq!(compact_seconds(3 * 3_600 + 12 * 60), "3h12m");
        assert_eq!(compact_seconds(2 * 60 + 30), "2m30s");
        assert_eq!(compact_seconds(45), "45s");
        // Zero units are skipped, not padded.
        assert_eq!(compact_seconds(3 * 3_600 + 3), "3h3s");
        // A non-positive span is empty; the caller only paints a positive one.
        assert_eq!(compact_seconds(0), "");
        assert_eq!(compact_seconds(-5), "");
    }

    #[test]
    fn window_label_reads_in_hours_or_days() {
        assert_eq!(window_label(Some(5 * 60)), "5h");
        assert_eq!(window_label(Some(7 * 24 * 60)), "7d");
        // Codex's ~30-day window (43800 min = 30d 10h) rounds to `30d`.
        assert_eq!(window_label(Some(43_800)), "30d");
        // An unknown length carries no label.
        assert_eq!(window_label(None), "");
    }

    #[test]
    fn model_label_drops_window_qualifier() {
        // The dedicated window token on the identity line carries the figure,
        // so the name sheds both qualifier forms entirely.
        assert_eq!(model_label("Opus 4.8 (1M context)"), "Opus 4.8");
        assert_eq!(model_label("Opus 4.8 (1M)"), "Opus 4.8");
        assert_eq!(model_label("Sonnet 4.6 (200K context)"), "Sonnet 4.6");
        assert_eq!(model_label("Opus 4.8"), "Opus 4.8");
        assert_eq!(model_label("GPT-5.5"), "GPT-5.5");
        // A non-window parenthetical is a real name qualifier — kept.
        assert_eq!(model_label("Sonnet 3.5 (New)"), "Sonnet 3.5 (New)");
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
        assert_eq!(model_label("Opus 4.8 Fast"), "Opus 4.8 Fast");
    }

    #[test]
    fn dollars2_is_always_two_decimals() {
        assert_eq!(dollars2(0.0), "$0.00");
        assert_eq!(dollars2(3.5), "$3.50");
        assert_eq!(dollars2(3.276), "$3.28");
        assert_eq!(dollars2(124.0), "$124.00");
    }

    #[test]
    fn dollars2_groups_thousands() {
        assert_eq!(dollars2(1_240.57), "$1,240.57");
        assert_eq!(dollars2(12_480.0), "$12,480.00");
        assert_eq!(dollars2(1_000_000.0), "$1,000,000.00");
        // Just under a grouping boundary stays ungrouped.
        assert_eq!(dollars2(999.99), "$999.99");
    }

    #[test]
    fn tokens_short_scales_by_magnitude() {
        assert_eq!(tokens_short(523), "523");
        assert_eq!(tokens_short(76_500), "76.5k");
        assert_eq!(tokens_short(1_200_000), "1.2M");
        assert_eq!(tokens_short(1_200_000_000), "1.2B");
        assert_eq!(tokens_short(47_200_000), "47.2M");
    }

    #[test]
    fn tokens_int_truncates_to_whole_units() {
        assert_eq!(tokens_int(523), "523");
        // 76.5k reads `76k` (truncated, not rounded) — the coarse live form.
        assert_eq!(tokens_int(76_500), "76k");
        assert_eq!(tokens_int(12_000), "12k");
        assert_eq!(tokens_int(999), "999");
        assert_eq!(tokens_int(1_900_000), "1M");
        assert_eq!(tokens_int(2_500_000_000), "2B");
    }

    #[test]
    fn window_short_reads_lowercase_magnitudes() {
        // The 1M class reads a quiet lowercase `m`, unlike `tokens_int`'s `M`.
        assert_eq!(window_short(1_000_000), "1m");
        assert_eq!(window_short(1_050_000), "1m");
        assert_eq!(window_short(272_000), "272k");
        assert_eq!(window_short(258_400), "258k");
        assert_eq!(window_short(200_000), "200k");
        assert_eq!(window_short(523), "523");
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
    fn activity_label_floors_to_its_highest_unit_and_caps_at_a_day() {
        // Whole minutes, floored — the sub-minute gating lives in `activity_short`.
        assert_eq!(activity_label(60), "1m");
        assert_eq!(activity_label(119), "1m");
        assert_eq!(activity_label(120), "2m");
        assert_eq!(activity_label(59 * 60), "59m");
        // Whole hours from 1h on, capped at `>1d` from a day on.
        assert_eq!(activity_label(60 * 60), "1h");
        assert_eq!(activity_label(23 * 3_600), "23h");
        assert_eq!(activity_label(24 * 3_600), ">1d");
        assert_eq!(activity_label(100 * 3_600), ">1d");
    }

    #[test]
    fn fmt_cpu_formats_integer() {
        assert_eq!(fmt_cpu(11), "11%");
        assert_eq!(fmt_cpu(0), "0%");
        assert_eq!(fmt_cpu(100), "100%");
        assert_eq!(fmt_cpu(400), "400%");
    }

    #[test]
    fn fmt_rss_picks_the_right_unit() {
        assert_eq!(fmt_rss(45), "45k");
        assert_eq!(fmt_rss(234 * 1024), "234M");
        // 1.1 GiB: 1024 + 102 = 1126 MiB = 1_153_024 KiB → 1.1G
        assert_eq!(fmt_rss(1_153_024), "1.1G");
        assert_eq!(fmt_rss(1_048_576), "1.0G");
    }

    #[test]
    fn fmt_io_picks_the_right_unit() {
        assert_eq!(fmt_io(500), "500B/s");
        assert_eq!(fmt_io(3 * 1_048_576), "3M/s");
        assert_eq!(fmt_io(450 * 1_024), "450k/s");
    }

    #[test]
    fn activity_short_withholds_sub_minute_ages() {
        let now = Timestamp::now();
        // A just-active agent shows nothing rather than a misleading `1m`.
        assert_eq!(activity_short(now), None);
        assert_eq!(activity_short(now - Duration::from_secs(59)), None);
        // Once a full minute has passed the floored age surfaces.
        assert_eq!(
            activity_short(now - Duration::from_secs(60)),
            Some("1m".to_owned())
        );
        assert_eq!(
            activity_short(now - Duration::from_secs(150)),
            Some("2m".to_owned())
        );
    }
}
