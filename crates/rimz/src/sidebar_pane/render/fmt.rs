//! Time and text formatting helpers shared by the renderer.

use jiff::Timestamp;

use crate::agents::RateLimitWindow;
use crate::sidebar_pane::render::layout::clip;
#[cfg(test)]
use crate::theme::fmt::reset_secs;
pub(super) use crate::theme::fmt::{dollars_cap, dollars2, reset_countdown};

/// Seconds since `at`, clamped at zero — the shared input for [`age_short`] and
/// the staleness color ramp, so a row reads the frame clock once and styles and
/// labels its age from the same snapshot-derived number.
pub(super) fn age_secs(at: Timestamp, now: Timestamp) -> i64 {
    now.duration_since(at).as_secs().max(0)
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

pub(super) fn age_short(at: Timestamp, now: Timestamp) -> String {
    age_label(age_secs(at, now))
}

/// A row's last-activity age, floored to its highest whole unit: `{m}m` up to an
/// hour, whole hours `{h}h` from 1h on, capped at `>1d` from a day on — a coarse
/// "how long since this agent last did something". Ages under five minutes never
/// reach here; [`activity_short`] withholds them so the card stays quiet until a
/// real gap opens.
pub(super) fn activity_label(seconds: i64) -> String {
    if seconds < 60 * 60 {
        format!("{}m", seconds / 60)
    } else if seconds < 60 * 60 * 24 {
        format!("{}h", seconds / 3_600)
    } else {
        ">1d".to_owned()
    }
}

/// The last-activity age once it crosses five minutes (floored), or `None` while
/// it is still under — a recently-active agent shows nothing rather than a
/// fresh clock, so the age surfaces only once a real gap has opened and the card
/// stays quiet through normal turn churn.
pub(super) fn activity_short(at: Timestamp, now: Timestamp) -> Option<String> {
    let seconds = age_secs(at, now);
    (seconds >= 300).then(|| activity_label(seconds))
}

/// A subagent's elapsed work span in the age vocabulary, never seconds: `<1m`
/// under a minute, then the floored `{m}m` / `{h}h` / `>1d` of
/// [`activity_label`]. Every form is at most three cells, so the caller can
/// right-align it into a fixed slot and the clusters stack vertically.
pub(super) fn elapsed_label(seconds: i64) -> String {
    if seconds < 60 {
        "<1m".to_owned()
    } else {
        activity_label(seconds)
    }
}

/// A budget window's reset countdown, two units scaled to how much time is left:
/// `{d}d{hh:02}h` at a day or more (`30d10h`, `6d23h`, `1d02h`), `{h}h{mm:02}m`
/// under a day (`20h20m`, `5h00m`, `0h45m`). The provider panel right-aligns the
/// result in a six-cell slot, so five- and six-cell countdowns share one right
/// edge. A passed reset reads `0h00m`; the enrich layer's reset-to-max
/// projection rolls every displayed window forward the moment its reset passes,
/// so a rendered countdown is live.
/// A budget window's compact bar label. Provider-defined named quotas use their
/// explicit label, clipped on cell boundaries to the fixed three-cell slot;
/// temporal windows retain the existing rounded hour/day label.
pub(super) fn window_label(window: &RateLimitWindow) -> String {
    if let Some(scope) = &window.scope {
        return clip(&scope.label, 3);
    }
    let Some(mins) = window.duration_mins else {
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
/// decimals: the per-row cost, the cockpit's count-up headline total, the provider
/// dashboard, and the fleet store all share this one shape, so a price never
/// jitters between a cents and a whole-dollar form. Grouping keeps a large
/// accumulating pile (`$12,480.13`) legible without changing that shape.
/// Shorten a model's display name for the capability line. First drops a
/// trailing context-window parenthetical (`Opus 4.8 (1M context)` / `Opus 4.8
/// (1M)` → `Opus 4.8`) — the identity line's dedicated window token carries
/// that figure now, so the name never repeats it. Then, when the name is still
/// a bare vendor *slug* (all lowercase, hyphenated, no spaces — the
/// pre-enrichment fallback), prettifies it (`claude-opus-4-8` → `Opus 4.8`). A
/// friendly name keeps its words but trades hyphens for spaces (`GPT-5.5
/// Codex` → `GPT 5.5 Codex`), so every name on the line speaks the spaced
/// `Opus 4.8` form whatever the vendor's catalog punctuation.
pub(super) fn model_label(display: &str) -> String {
    let cleaned = strip_window_qualifier(display);
    if let Some(custom) = crate::agents::model_display::display_factory_custom_selector(&cleaned) {
        custom
    } else if looks_like_slug(&cleaned) {
        crate::agents::model_display::display_model(&cleaned)
    } else {
        cleaned.replace('-', " ")
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
/// display name (`Opus 4.8`, `GPT-5.5 Codex`), so the prettifier only fires on
/// the fallback path.
fn looks_like_slug(value: &str) -> bool {
    value.contains('-')
        && !value.contains(' ')
        && !value.contains('(')
        && !value.chars().any(|c| c.is_ascii_uppercase())
}

/// A token count as a whole-unit magnitude with no decimal — `523`, `76k`,
/// `1M`, `2B` — for the agent card and the live cockpit / provider lines, where a
/// tenths place is noise beside the precise `76.5k` the W/M store rows carry.
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

/// RSS in a compact form whose figure holds to four cells — `45k`, `512M`,
/// `1.1G`, `12G` — so the process row's fixed `M` slot never shifts: each unit
/// rolls to the next as its figure would reach four digits, and the GiB
/// decimal drops from 10 GiB on. Input is in kibibytes.
pub(super) fn fmt_rss(rss_kb: u64) -> String {
    let mib = rss_kb as f64 / 1_024.0;
    let gib = rss_kb as f64 / 1_048_576.0;
    if gib >= 9.95 {
        format!("{gib:.0}G")
    } else if mib >= 999.5 {
        format!("{gib:.1}G")
    } else if rss_kb >= 1_000 {
        format!("{mib:.0}M")
    } else {
        format!("{rss_kb}k")
    }
}

/// IO rate whose magnitude holds to four cells before the `/s` — `12B/s`,
/// `450k/s`, `8M/s`, `2G/s` — so the process row's fixed `⇅` slot never
/// shifts: each unit rolls to the next as its figure would reach four digits
/// (bytes/s, integer magnitude).
pub(super) fn fmt_io(bps: u64) -> String {
    let kib = bps as f64 / 1_024.0;
    let mib = bps as f64 / 1_048_576.0;
    let gib = bps as f64 / 1_073_741_824.0;
    if mib >= 999.5 {
        format!("{gib:.0}G/s")
    } else if kib >= 999.5 {
        format!("{mib:.0}M/s")
    } else if bps >= 1_000 {
        format!("{kib:.0}k/s")
    } else {
        format!("{bps}B/s")
    }
}
#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn time_and_window_labels_keep_their_compact_boundaries() {
        for (seconds, expected) in [
            (4 * 3_600 + 20 * 60, "4h20m"),
            (45 * 60, "0h45m"),
            (86_400, "1d00h"),
            (6 * 86_400 + 23 * 3_600, "6d23h"),
            (30 * 86_400 + 10 * 3_600, "30d10h"),
            (-10, "0h00m"),
        ] {
            assert_eq!(reset_secs(seconds), expected);
        }
        assert_eq!(reset_secs(45 * 60).chars().count(), 5);
        assert_eq!(reset_secs(5 * 86_400).chars().count(), 5);

        for (seconds, expected) in [
            (0, "<1m"),
            (45, "<1m"),
            (60, "1m"),
            (59 * 60, "59m"),
            (3 * 3_600 + 12 * 60, "3h"),
            (36 * 3_600, ">1d"),
        ] {
            assert_eq!(elapsed_label(seconds), expected);
            assert!(elapsed_label(seconds).chars().count() <= 3);
        }

        let window = |duration_mins| RateLimitWindow {
            duration_mins,
            ..Default::default()
        };
        assert_eq!(window_label(&window(Some(5 * 60))), "5h");
        assert_eq!(window_label(&window(Some(7 * 24 * 60))), "7d");
        assert_eq!(window_label(&window(Some(43_800))), "30d");
        assert_eq!(window_label(&window(None)), "");
        assert_eq!(
            window_label(&RateLimitWindow {
                scope: Some(crate::agents::RateLimitWindowScope {
                    id: "build_minutes".to_owned(),
                    label: "bld-minutes".to_owned(),
                }),
                ..Default::default()
            }),
            "bld"
        );
    }

    #[test]
    fn model_money_token_and_percent_labels_keep_display_shapes() {
        for (raw, expected) in [
            ("Opus 4.8 (1M context)", "Opus 4.8"),
            ("Opus 4.8 (1M)", "Opus 4.8"),
            ("Sonnet 4.6 (200K context)", "Sonnet 4.6"),
            ("GPT-5.5", "GPT 5.5"),
            ("Sonnet 3.5 (New)", "Sonnet 3.5 (New)"),
            ("claude-opus-4-8", "Opus 4.8"),
            ("claude-opus-4-8-20260101", "Opus 4.8"),
            ("gpt-5.5-codex", "GPT 5.5 Codex"),
            ("GPT-5.5 Codex", "GPT 5.5 Codex"),
        ] {
            assert_eq!(model_label(raw), expected);
        }

        for (usd, expected) in [
            (0.0, "$0.00"),
            (3.5, "$3.50"),
            (3.276, "$3.28"),
            (999.99, "$999.99"),
            (1_240.57, "$1,240.57"),
            (1_000_000.0, "$1,000,000.00"),
        ] {
            assert_eq!(dollars2(usd), expected);
        }
        assert_eq!(dollars_cap(50.0), "$50");
        assert_eq!(dollars_cap(1_250.25), "$1,250.25");

        for (count, short, int) in [
            (523, "523", "523"),
            (76_500, "76.5k", "76k"),
            (1_200_000, "1.2M", "1M"),
            (1_200_000_000, "1.2B", "1B"),
            (47_200_000, "47.2M", "47M"),
        ] {
            assert_eq!(tokens_short(count), short);
            assert_eq!(tokens_int(count), int);
        }
        assert_eq!(tokens_int(2_500_000_000), "2B");
        assert_eq!(window_short(1_050_000), "1m");
        assert_eq!(window_short(272_000), "272k");
        assert_eq!(window_short(523), "523");

        for (precise, whole, expected) in [
            (Some(78.23), 78, "78.2%"),
            (Some(9.9), 9, "9.9%"),
            (Some(99.96), 99, "100%"),
            (None, 38, "38%"),
            (None, 200, "100%"),
        ] {
            let label = pct_label(precise, whole);
            assert_eq!(label, expected);
            assert!(label.chars().count() <= 5);
        }
    }

    #[test]
    fn activity_labels_floor_and_sub_five_minute_ages_stay_quiet() {
        for (seconds, expected) in [
            (60, "1m"),
            (119, "1m"),
            (120, "2m"),
            (59 * 60, "59m"),
            (60 * 60, "1h"),
            (23 * 3_600, "23h"),
            (24 * 3_600, ">1d"),
            (100 * 3_600, ">1d"),
        ] {
            assert_eq!(activity_label(seconds), expected);
        }

        let now = Timestamp::now();
        assert_eq!(activity_short(now, now), None);
        assert_eq!(activity_short(now - Duration::from_secs(299), now), None);
        assert_eq!(
            activity_short(now - Duration::from_secs(300), now),
            Some("5m".to_owned())
        );
        assert_eq!(
            activity_short(now - Duration::from_secs(7 * 60), now),
            Some("7m".to_owned())
        );
    }

    #[test]
    fn process_resource_labels_pick_units_and_hold_fixed_slots() {
        assert_eq!(fmt_cpu(0), "0%");
        assert_eq!(fmt_cpu(400), "400%");

        for (rss_kb, expected) in [
            (45, "45k"),
            (999, "999k"),
            (1_000, "1M"),
            (234 * 1024, "234M"),
            (1_023 * 1_024, "1.0G"),
            (1_153_024, "1.1G"),
            (12 * 1_048_576, "12G"),
        ] {
            assert_eq!(fmt_rss(rss_kb), expected);
            assert!(fmt_rss(rss_kb).chars().count() <= 4);
        }

        for (bps, expected) in [
            (500, "500B/s"),
            (999, "999B/s"),
            (1_000, "1k/s"),
            (450 * 1_024, "450k/s"),
            (3 * 1_048_576, "3M/s"),
            (1_023 * 1_048_576, "1G/s"),
        ] {
            assert_eq!(fmt_io(bps), expected);
            assert!(fmt_io(bps).chars().count() <= 6);
        }
    }
}
