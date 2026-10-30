//! Layer 2 — palette resolution: the scheme's raw tones (Layer 1) derived
//! into the depth-resolved semantic slots the renderer paints, plus the
//! heat/calm ramps and the derived expense, fast-mode, and expired-cache
//! tones. Component tokens (Layer 3) and the Theme facade read these slots;
//! this is the one place depth quantization and slot overrides are applied.

use crate::agents::context::PaceReading;
use crate::config::{
    AnimationColor, BudgetBarConfig, BudgetBurnRateConfig, ColorDepth, PaletteRole, ThemeColor,
    ThemeConfig, xterm_rgb,
};

use super::raw::RawPalette;
use super::{BrandColor, Identity, Tone, oklab};

/// Stops on the context **health** ramp, ordered calm → alarm:
/// `[good, warn, caution, alarm]` — green → gold → orange → rose-red. Prepending
/// the scheme's green to the warm trio widens the visible range so a filling
/// context reads as a health sweep at a glance, while every stop stays
/// scheme-tunable through its existing slot. [`Theme::heat_tone`] interpolates
/// across these in OKLab.
const HEAT_RAMP_STOPS: usize = 4;

/// Where the warm tail (`warn`) sits on the full ramp: the second of four stops,
/// i.e. one third of the way along. Scales whose "low" should read warm rather
/// than healthy-green — idle age, where fifteen minutes is stale, not optimal —
/// map their amount into `[HEAT_RAMP_WARM_START, 1.0]` via
/// [`Theme::warm_heat_tone`], reproducing the legacy warn → caution → alarm
/// sweep.
pub(crate) const HEAT_RAMP_WARM_START: f32 = 1.0 / (HEAT_RAMP_STOPS as f32 - 1.0);

/// The fresh-input "expense" tone is the reddest marker in the sidebar: it sits
/// past the ramp's `alarm` stop, so the input read always reads redder than the
/// context bar's scaled-to-red cache-read run — even at a near-full window, where
/// that run reaches `alarm`. Take `alarm` (`heat_ramp[3]`, the ramp's reddest
/// stop) directly, enrich its chroma toward the gamut edge, and deepen its
/// lightness: a deep, hot red that holds alarm's hue but burns hotter than the
/// lighter rose. The deepen step also carries the separation at indexed depth —
/// without it a tone only a touch off the rose collapses into alarm's xterm cell.
/// Levers: `CHROMA` enriches where a scheme leaves gamut room; `DEEPEN` makes it
/// read hotter and lands its own indexed cell. Tuned against a rendered frame.
const INPUT_EXPENSE_CHROMA: f32 = 1.30;
const INPUT_EXPENSE_DEEPEN: f32 = -0.09;

/// Pull `alarm` halfway to `muted`: a dull, greyed red for an expired prompt
/// cache, not the full bar's live red.
const CACHE_EXPIRED_GREY: f32 = 0.5;

/// Step the fast-mode amber away from the background to separate it from `warn`.
/// `LIGHTNESS_STEP` is the first step; the shared readability floor then checks
/// the emitted color, truecolor or indexed cell, against the background.
const FAST_MODE_LIGHTNESS_STEP: f32 = 0.04;
const READABLE_CONTRAST_STEP: f32 = 0.01;
const MIN_READABLE_CONTRAST: f32 = 3.0;

/// The active palette, one named slot per semantic tone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub(crate) depth: ColorDepth,
    raw: RawPalette,
    pub(crate) heat_ramp: [(u8, u8, u8); HEAT_RAMP_STOPS],
    pub(crate) calm_ramp: [(u8, u8, u8); 2],
    pub(crate) good: Tone,
    pub(crate) warn: Tone,
    pub(crate) caution: Tone,
    pub(crate) alarm: Tone,
    /// The fresh-input cost tone — `alarm` deepened a step past the ramp's red
    /// stop into the reddest marker on screen, so the costliest read always reads
    /// hotter than the bar's scaled-to-red health run. Derived like `heat_ramp`,
    /// not a tunable slot.
    pub(crate) expense: Tone,
    /// The context meter's fixed expired-cache tone, also worn by its age clock
    /// and the stale line's age; derived, not a tunable slot.
    pub(crate) cache_expired: Tone,
    pub(crate) accent: Tone,
    /// The fast-mode bolt's amber, derived from `warn` and kept readable against
    /// the scheme background at the emitted depth; not a tunable slot.
    fast_mode: Tone,
    pub(crate) cool: Tone,
    pub(crate) meta: Tone,
    pub(crate) body: Tone,
    pub(crate) muted: Tone,
    pub(crate) faint: Tone,
    pub(crate) rule: Tone,
    pub(crate) selection: Tone,
    pub(crate) selection_bg: Tone,
}

/// Suppress cool underspend until enough of the window has elapsed.
const COOL_PACE_MIN_ELAPSED: f64 = 0.4;

impl Palette {
    /// Pace tone under the configured burn-rate bands; no tone at rest.
    /// Warm overburn is immediate; cool underspend waits for a meaningful
    /// elapsed share. Misordered bands degrade toward the more visible tone.
    pub fn pace_tone(&self, reading: PaceReading, pace: &BudgetBurnRateConfig) -> Option<Tone> {
        let pace_pct = (reading.ratio * 100.0).max(0.0).round() as u64;
        warm_band_amount(
            pace_pct,
            u64::from(pace.yellow),
            u64::from(pace.amber),
            u64::from(pace.red),
        )
        .map(|amount| {
            let mapped =
                HEAT_RAMP_WARM_START + amount.clamp(0.0, 1.0) * (1.0 - HEAT_RAMP_WARM_START);
            rgb_color(ramp_tone(&self.heat_ramp, mapped), self.depth)
        })
        .or_else(|| {
            (reading.elapsed_share >= COOL_PACE_MIN_ELAPSED)
                .then(|| {
                    cool_band_amount(pace_pct, u64::from(pace.green), u64::from(pace.deep_green))
                })
                .flatten()
                .map(|amount| rgb_color(ramp_tone(&self.calm_ramp, amount), self.depth))
        })
    }

    pub fn resolve(theme: &ThemeConfig, depth: ColorDepth) -> Palette {
        Self::resolve_with_raw(theme, depth, raw_palette_for_theme(theme))
    }

    fn resolve_with_raw(theme: &ThemeConfig, depth: ColorDepth, raw: RawPalette) -> Palette {
        let tones = raw.derive_tones();
        let slot = |override_color: Option<ThemeColor>, builtin| {
            override_color
                .map(|color| theme_color(color, depth, &raw))
                .unwrap_or_else(|| rgb_color(builtin, depth))
        };
        let heat_ramp = [
            derived_rgb_slot(theme.good, tones.good, &raw),
            derived_rgb_slot(theme.warn, tones.warn, &raw),
            derived_rgb_slot(theme.caution, tones.caution, &raw),
            derived_rgb_slot(theme.alarm, tones.alarm, &raw),
        ];
        let calm_ramp = [
            derived_rgb_slot(theme.body, tones.body, &raw),
            derived_rgb_slot(theme.good, tones.good, &raw),
        ];
        // `alarm` (stop 3) is the ramp's reddest tone; the input read must read
        // redder still. Take it directly, enrich its chroma in place (a rotation
        // of zero holds the hue), then deepen its lightness — a hotter red on the
        // same hue that lands its own cell at any depth.
        let expense = rgb_color(
            oklab::lift_lightness(
                oklab::warm_toward(heat_ramp[3], heat_ramp[3], 0.0, INPUT_EXPENSE_CHROMA),
                INPUT_EXPENSE_DEEPEN,
            ),
            depth,
        );
        let cache_expired = rgb_color(
            oklab::blend(
                heat_ramp[3],
                derived_rgb_slot(theme.muted, tones.muted, &raw),
                CACHE_EXPIRED_GREY,
            ),
            depth,
        );
        let fast_mode = oklab::lift_lightness(
            oklab::warm_toward(heat_ramp[1], raw.red, 0.10, 1.15),
            contrast_direction(raw.background) * FAST_MODE_LIGHTNESS_STEP,
        );
        Palette {
            depth,
            raw,
            heat_ramp,
            calm_ramp,
            good: slot(theme.good, tones.good),
            warn: slot(theme.warn, tones.warn),
            caution: slot(theme.caution, tones.caution),
            alarm: slot(theme.alarm, tones.alarm),
            expense,
            cache_expired,
            accent: slot(theme.accent, tones.accent),
            fast_mode: readable_rgb(fast_mode, depth, raw.background),
            cool: slot(theme.cool, tones.cool),
            meta: slot(theme.meta, tones.meta),
            body: slot(theme.body, tones.body),
            muted: slot(theme.muted, tones.muted),
            faint: slot(theme.faint, tones.faint),
            rule: slot(theme.rule, tones.rule),
            selection: slot(theme.selection, tones.selection),
            selection_bg: slot(theme.selection_bg, tones.selection_bg),
        }
    }

    /// Resolve an external-identity tone at the palette's depth. The base hue is
    /// fixed; only the truecolor-vs-indexed emission differs.
    pub fn identity(&self, id: Identity) -> Tone {
        rgb_color(id.base_rgb(), self.depth)
    }

    /// A palette-role tone at the palette's depth — a provider brand pinned to a
    /// scheme role tracks the active palette.
    pub(crate) fn role_tone(&self, role: PaletteRole) -> Tone {
        rgb_color(self.raw.role_rgb(role), self.depth)
    }

    pub(crate) fn animation_color(&self, color: AnimationColor) -> Tone {
        match color {
            AnimationColor::Good => self.good,
            AnimationColor::Warn => self.warn,
            AnimationColor::Caution => self.caution,
            AnimationColor::Alarm => self.alarm,
            AnimationColor::Accent => self.accent,
            AnimationColor::Cool => self.cool,
            AnimationColor::Meta => self.meta,
            AnimationColor::Body => self.body,
            AnimationColor::Muted => self.muted,
            AnimationColor::Faint => self.faint,
            AnimationColor::Clay => self.identity(Identity::Claude),
            AnimationColor::Indexed(index) => Tone::Indexed(index),
            AnimationColor::Rgb(red, green, blue) => rgb_color((red, green, blue), self.depth),
            AnimationColor::Role(role) => self.role_tone(role),
        }
    }

    /// The tone of a budget with `remaining_pct` left: a full green→red drain
    /// across the heat ramp, anchored green at `100%`, with the
    /// `[theme.display.budget_bar]` zones as the warm stops the remaining
    /// figure falls through — `100 → 0.0` green, `yellow → ⅓` warn, `amber → ⅔`
    /// caution, `red`/below `→ 1.0` alarm — so the tone warms continuously as
    /// the budget empties. Each zone names the exclusive upper bound of
    /// remaining budget where its tier is reached ([`BudgetBarConfig`]);
    /// checked worst-first, so a misordered user config degrades to the worse
    /// tier. The sidebar's mana bar and the `rimz accounts` percentages both
    /// read it.
    pub fn budget_tone(&self, remaining_pct: u8, zones: &BudgetBarConfig) -> Tone {
        let remaining = u64::from(remaining_pct.min(100));
        let yellow = u64::from(zones.yellow);
        let amber = u64::from(zones.amber);
        let red = u64::from(zones.red);
        let amount = if remaining < red {
            1.0
        } else if remaining < amber {
            interpolate_heat(remaining, red, amber, 1.0, 2.0 / 3.0)
        } else if remaining < yellow {
            interpolate_heat(remaining, amber, yellow, 2.0 / 3.0, 1.0 / 3.0)
        } else {
            interpolate_heat(remaining, yellow, 100, 1.0 / 3.0, 0.0)
        };
        rgb_color(ramp_tone(&self.heat_ramp, amount), self.depth)
    }

    pub fn rgb_tone(&self, rgb: (u8, u8, u8)) -> Tone {
        Tone::from_rgb(rgb, self.depth)
    }

    /// Assign readable model tones in rank order, reserving each brand for its own models.
    /// Scheme hues nearest the brand are used before a second tier blended toward foreground.
    /// Colliding hues and second-tier candidates search blends from halfway to foreground,
    /// then below halfway toward the source, keeping the first unused emitted color.
    /// Emitted colors stay distinct until the eligible candidates in both tiers are exhausted.
    /// Reused tones first avoid models marked co-visible by the index predicate. Muted is reserved.
    pub fn model_tones(
        &self,
        brands: &[BrandColor],
        co_visible: impl Fn(usize, usize) -> bool,
    ) -> Vec<Tone> {
        let readable = |tone| {
            if oklab::contrast_ratio(emitted_rgb(tone), self.raw.background)
                >= MIN_READABLE_CONTRAST
            {
                tone
            } else {
                readable_rgb(emitted_rgb(tone), self.depth, self.raw.background)
            }
        };
        let muted = emitted_rgb(self.muted());
        let distinct_blend = |tone, pool: &[(Tone, Option<BrandColor>)]| {
            (50..=100)
                .chain((0..50).rev())
                .map(|amount| {
                    readable(self.rgb_tone(oklab::blend(
                        emitted_rgb(tone),
                        self.raw.foreground,
                        amount as f32 / 100.0,
                    )))
                })
                .find(|tone| {
                    emitted_rgb(*tone) != muted
                        && !pool
                            .iter()
                            .any(|(other, _)| emitted_rgb(*other) == emitted_rgb(*tone))
                })
        };
        let mut pool = Vec::new();
        for &brand in brands {
            if pool.iter().any(|(_, owner)| *owner == Some(brand)) {
                continue;
            }
            let mut tone = readable(brand.tone(self));
            if emitted_rgb(tone) == muted {
                let Some(distinct) = distinct_blend(tone, &pool) else {
                    continue;
                };
                tone = distinct;
            }
            pool.push((tone, Some(brand)));
        }
        let raw = self.raw;
        for rgb in [
            raw.red,
            raw.green,
            raw.yellow,
            raw.blue,
            raw.magenta,
            raw.cyan,
        ] {
            let mut tone = readable(self.rgb_tone(rgb));
            if emitted_rgb(tone) == muted
                || pool
                    .iter()
                    .any(|(other, _)| emitted_rgb(*other) == emitted_rgb(tone))
            {
                let Some(distinct) = distinct_blend(tone, &pool) else {
                    continue;
                };
                tone = distinct;
            }
            pool.push((tone, None));
        }
        let tier0_len = pool.len();
        for index in 0..tier0_len {
            let (tone, owner) = pool[index];
            if let Some(distinct) = distinct_blend(tone, &pool) {
                pool.push((distinct, owner));
            }
        }
        let tiers = [&pool[..tier0_len], &pool[tier0_len..]];
        let mut tones = Vec::with_capacity(brands.len());
        for (index, &brand) in brands.iter().enumerate() {
            let base = brand.tone(self);
            let nearest = |candidates: &[(Tone, Option<BrandColor>)], taken: &[Tone]| {
                candidates
                    .iter()
                    .filter(|(tone, owner)| {
                        owner.is_none_or(|owner| owner == brand)
                            && ((*owner == Some(brand) && emitted_rgb(*tone) == emitted_rgb(base))
                                || !brands.iter().any(|other| {
                                    *other != brand
                                        && emitted_rgb(other.tone(self)) == emitted_rgb(*tone)
                                }))
                            && !taken
                                .iter()
                                .any(|other| emitted_rgb(*other) == emitted_rgb(*tone))
                    })
                    .min_by(|(left, _), (right, _)| {
                        oklab::chromatic_distance(emitted_rgb(base), emitted_rgb(*left)).total_cmp(
                            &oklab::chromatic_distance(emitted_rgb(base), emitted_rgb(*right)),
                        )
                    })
                    .map(|&(tone, _)| tone)
            };
            let tone = tiers
                .iter()
                .find_map(|tier| nearest(tier, &tones))
                .or_else(|| {
                    let shown_together = tones
                        .iter()
                        .enumerate()
                        .filter(|(other, _)| co_visible(index, *other))
                        .map(|(_, tone)| *tone)
                        .collect::<Vec<_>>();
                    tiers.iter().find_map(|tier| nearest(tier, &shown_together))
                })
                .or_else(|| nearest(tiers[0], &[]))
                .unwrap_or_else(|| readable(base));
            tones.push(tone);
        }
        tones
    }

    /// Fade a model tone toward the scheme background for projected spend. Start halfway;
    /// advance when depth quantization would erase the distinction or increase its contrast.
    pub fn forecast_tone(&self, tone: Tone) -> Tone {
        let base = emitted_rgb(tone);
        let background = oklab::lightness(self.raw.background);
        let contrast = (oklab::lightness(base) - background).abs();
        for amount in 50..=100 {
            let faded = self.rgb_tone(oklab::blend(
                base,
                self.raw.background,
                amount as f32 / 100.0,
            ));
            if faded != tone && (oklab::lightness(emitted_rgb(faded)) - background).abs() < contrast
            {
                return faded;
            }
        }
        self.rgb_tone(self.raw.background)
    }

    pub fn good(&self) -> Tone {
        self.good
    }
    pub fn warn(&self) -> Tone {
        self.warn
    }
    pub fn alarm(&self) -> Tone {
        self.alarm
    }
    pub fn accent(&self) -> Tone {
        self.accent
    }
    pub fn fast_mode(&self) -> Tone {
        self.fast_mode
    }
    pub fn cool(&self) -> Tone {
        self.cool
    }
    pub fn meta(&self) -> Tone {
        self.meta
    }
    pub fn body(&self) -> Tone {
        self.body
    }
    pub fn muted(&self) -> Tone {
        self.muted
    }
    pub fn faint(&self) -> Tone {
        self.faint
    }
    pub fn rule(&self) -> Tone {
        self.rule
    }
    pub fn selection(&self) -> Tone {
        self.selection
    }
    pub fn selection_bg(&self) -> Tone {
        self.selection_bg
    }
}

fn contrast_direction(background: (u8, u8, u8)) -> f32 {
    if oklab::contrast_ratio((255, 255, 255), background)
        >= oklab::contrast_ratio((0, 0, 0), background)
    {
        1.0
    } else {
        -1.0
    }
}

fn readable_rgb(mut rgb: (u8, u8, u8), depth: ColorDepth, background: (u8, u8, u8)) -> Tone {
    let direction = contrast_direction(background);
    let mut step = READABLE_CONTRAST_STEP;
    let mut tone = rgb_color(rgb, depth);
    // At most one unit of lightness: lift_lightness clamps at either endpoint.
    for _ in 0..100 {
        if oklab::contrast_ratio(emitted_rgb(tone), background) >= MIN_READABLE_CONTRAST {
            break;
        }
        let next = oklab::lift_lightness(rgb, direction * step);
        if next == rgb {
            step += READABLE_CONTRAST_STEP;
            continue;
        }
        rgb = next;
        step = READABLE_CONTRAST_STEP;
        tone = rgb_color(rgb, depth);
    }
    tone
}

/// Position along the warm tail (`warn → caution → alarm`) for a value crossing
/// three escalating thresholds `yellow < amber < red`. `None` at or below
/// `yellow` so the caller keeps its resting tone; then `0.0` just past `yellow`,
/// `0.5` at `amber`, and `1.0` at `red` and beyond — the sweep
/// the warm heat ramp renders. Checked worst-first, so a misordered
/// config degrades to the worse tier.
fn warm_band_amount(value: u64, yellow: u64, amber: u64, red: u64) -> Option<f32> {
    if value > red {
        Some(1.0)
    } else if value > amber {
        Some(interpolate_heat(value, amber, red, 0.5, 1.0))
    } else if value > yellow {
        Some(interpolate_heat(value, yellow, amber, 0.0, 0.5))
    } else {
        None
    }
}

/// Position along the cool tail (`body → good`) for a value crossing two
/// descending thresholds `green > deep_green`. `None` at or above `green` so
/// the caller keeps its resting tone; then the amount climbs toward `1.0` at
/// `deep_green` and below. Checked greenest-first, so a misordered config
/// degrades to the more visible signal.
fn cool_band_amount(value: u64, green: u64, deep_green: u64) -> Option<f32> {
    if value <= deep_green {
        Some(1.0)
    } else if value < green {
        Some(interpolate_heat(value, deep_green, green, 1.0, 0.0))
    } else {
        None
    }
}

fn raw_palette_for_theme(theme: &ThemeConfig) -> RawPalette {
    theme
        .colors
        .as_ref()
        .and_then(|colors| crate::config::parse_colors(colors).ok())
        .map(RawPalette::from)
        .or_else(|| {
            theme
                .scheme
                .as_deref()
                .and_then(crate::config::explicit_scheme)
                .map(RawPalette::from)
        })
        .or_else(|| {
            crate::config::explicit_scheme(crate::config::DEFAULT_SCHEME).map(RawPalette::from)
        })
        .unwrap_or(RawPalette::DEFAULT)
}

fn theme_color(color: ThemeColor, depth: ColorDepth, raw: &RawPalette) -> Tone {
    match color {
        ThemeColor::Role(role) => rgb_color(raw.role_rgb(role), depth),
        ThemeColor::Indexed(index) => Tone::Indexed(index),
        ThemeColor::Rgb(red, green, blue) => rgb_color((red, green, blue), depth),
    }
}

fn derived_rgb_slot(
    color: Option<ThemeColor>,
    builtin: (u8, u8, u8),
    raw: &RawPalette,
) -> (u8, u8, u8) {
    match color {
        Some(ThemeColor::Role(role)) => raw.role_rgb(role),
        Some(ThemeColor::Rgb(red, green, blue)) => (red, green, blue),
        Some(ThemeColor::Indexed(index)) if index >= 16 => xterm_rgb(index),
        Some(ThemeColor::Indexed(_)) | None => builtin,
    }
}

fn rgb_color(rgb: (u8, u8, u8), depth: ColorDepth) -> Tone {
    Tone::from_rgb(rgb, depth)
}

/// The RGB a terminal shows for `tone`: an indexed cell's xterm value.
pub(super) fn emitted_rgb(tone: Tone) -> oklab::Rgb {
    const ANSI: [oklab::Rgb; 16] = [
        (0x00, 0x00, 0x00),
        (0x80, 0x00, 0x00),
        (0x00, 0x80, 0x00),
        (0x80, 0x80, 0x00),
        (0x00, 0x00, 0x80),
        (0x80, 0x00, 0x80),
        (0x00, 0x80, 0x80),
        (0xc0, 0xc0, 0xc0),
        (0x80, 0x80, 0x80),
        (0xff, 0x00, 0x00),
        (0x00, 0xff, 0x00),
        (0xff, 0xff, 0x00),
        (0x00, 0x00, 0xff),
        (0xff, 0x00, 0xff),
        (0x00, 0xff, 0xff),
        (0xff, 0xff, 0xff),
    ];
    match tone {
        Tone::Rgb(red, green, blue) => (red, green, blue),
        Tone::Indexed(index) if index < 16 => ANSI[index as usize],
        Tone::Indexed(index) => xterm_rgb(index),
    }
}

/// Where `value` sits between `start` and `end`, mapped onto `[low, high]`
/// and clamped; an empty span reads `high`.
pub(crate) fn interpolate_heat(value: u64, start: u64, end: u64, low: f32, high: f32) -> f32 {
    if end <= start {
        return high;
    }
    let position = (value - start) as f32 / (end - start) as f32;
    low + (high - low) * position.clamp(0.0, 1.0)
}

/// Piecewise OKLab interpolation across an N-stop ramp: `amount` ∈ `[0, 1]` maps
/// across the `N - 1` segments, blending within the active one. Endpoints clamp,
/// so `0.0` is the first stop and `1.0` the last. One blend regardless of stop
/// count — the ramp can grow or shrink without touching the math.
pub(crate) fn ramp_tone(ramp: &[(u8, u8, u8)], amount: f32) -> (u8, u8, u8) {
    match ramp {
        [] => (0, 0, 0),
        [only] => *only,
        _ => {
            let segments = (ramp.len() - 1) as f32;
            let scaled = amount.clamp(0.0, 1.0) * segments;
            let lower = (scaled.floor() as usize).min(ramp.len() - 2);
            oklab::blend(ramp[lower], ramp[lower + 1], scaled - lower as f32)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model_brands() -> Vec<BrandColor> {
        let brand = |kind| super::super::resolve_provider_brand(kind, &Default::default());
        let mut brands = vec![brand("claude"); 5];
        brands.extend([brand("codex"); 4]);
        brands.extend([
            brand("grok"),
            brand("unregistered"),
            BrandColor::Rgb(10, 80, 30),
            BrandColor::Rgb(0, 0, 0),
            BrandColor::Indexed(0),
            BrandColor::Indexed(16),
        ]);
        brands
    }

    #[test]
    fn model_tones_are_readable_across_bundled_schemes_at_both_depths() {
        let brands = model_brands();
        let mut failures = Vec::new();
        let mut moved_brands = Vec::new();
        for scheme in crate::config::scheme_swatches() {
            let config = ThemeConfig {
                scheme: Some(scheme.name.clone()),
                ..Default::default()
            };
            for depth in [ColorDepth::Truecolor, ColorDepth::Indexed] {
                let palette = Palette::resolve(&config, depth);
                let tones = palette.model_tones(&brands, |_, _| true);
                assert_eq!(tones.len(), brands.len());
                assert_eq!(tones, palette.model_tones(&brands, |_, _| true));
                for (index, brand) in brands.iter().enumerate() {
                    let authored = brand.tone(&palette);
                    if !brands[..index]
                        .iter()
                        .any(|earlier| emitted_rgb(earlier.tone(&palette)) == emitted_rgb(authored))
                        && emitted_rgb(authored) != emitted_rgb(palette.muted())
                        && oklab::contrast_ratio(emitted_rgb(authored), palette.raw.background)
                            >= 3.0
                        && emitted_rgb(tones[index]) != emitted_rgb(authored)
                    {
                        moved_brands.push((
                            scheme.name.clone(),
                            depth,
                            index,
                            authored,
                            tones[index],
                        ));
                    }
                }
                for (index, tone) in tones.into_iter().enumerate() {
                    let contrast = oklab::contrast_ratio(emitted_rgb(tone), palette.raw.background);
                    if contrast < 3.0 {
                        failures.push((scheme.name.clone(), depth, index, tone, contrast));
                    }
                }
            }
        }
        assert!(
            failures.is_empty() && moved_brands.is_empty(),
            "contrast: {failures:?}; moved brands: {moved_brands:?}"
        );
    }

    #[test]
    fn model_tones_make_each_black_brand_readable() {
        for scheme in crate::config::scheme_swatches() {
            for depth in [ColorDepth::Truecolor, ColorDepth::Indexed] {
                let palette = Palette::resolve(
                    &ThemeConfig {
                        scheme: Some(scheme.name.clone()),
                        ..Default::default()
                    },
                    depth,
                );
                for brand in [
                    BrandColor::Rgb(0, 0, 0),
                    BrandColor::Indexed(0),
                    BrandColor::Indexed(16),
                ] {
                    let tone = palette.model_tones(&[brand], |_, _| true)[0];
                    let contrast = oklab::contrast_ratio(emitted_rgb(tone), palette.raw.background);
                    assert!(
                        contrast >= 3.0,
                        "{} {depth:?} {brand:?}: {tone:?} {contrast}",
                        scheme.name
                    );
                }
            }
        }
    }

    #[test]
    fn model_tones_keep_nine_distinct_colours_across_bundled_schemes_at_both_depths() {
        let mut failures = Vec::new();
        for scheme in crate::config::scheme_swatches() {
            let config = ThemeConfig {
                scheme: Some(scheme.name.clone()),
                ..Default::default()
            };
            for depth in [ColorDepth::Truecolor, ColorDepth::Indexed] {
                let palette = Palette::resolve(&config, depth);
                let raw = palette.raw;
                let mut hues = [
                    raw.red,
                    raw.green,
                    raw.yellow,
                    raw.blue,
                    raw.magenta,
                    raw.cyan,
                ]
                .map(|rgb| emitted_rgb(palette.rgb_tone(rgb)))
                .to_vec();
                hues.sort_unstable();
                hues.dedup();
                if hues.len() != 6 {
                    continue;
                }
                let mut tones = palette.model_tones(&model_brands(), |_, _| true)[..9]
                    .iter()
                    .copied()
                    .map(emitted_rgb)
                    .collect::<Vec<_>>();
                tones.sort_unstable();
                tones.dedup();
                if tones.len() != 9 {
                    failures.push((scheme.name.clone(), depth, tones.len()));
                }
            }
        }
        assert!(failures.is_empty(), "{failures:?}");
    }

    #[test]
    fn model_tones_preserve_brands_and_choose_the_nearest_scheme_hues() {
        let palette = Palette::resolve(
            &ThemeConfig {
                scheme: Some("TokyoNight Night".into()),
                ..Default::default()
            },
            ColorDepth::Truecolor,
        );
        let brand = |kind| super::super::resolve_provider_brand(kind, &Default::default());
        let tones = palette.model_tones(
            &[
                brand("claude"),
                brand("claude"),
                brand("claude"),
                brand("codex"),
                brand("codex"),
                brand("codex"),
            ],
            |_, _| true,
        );
        assert_eq!(tones[0], Tone::Rgb(0xd9, 0x77, 0x57));
        assert_eq!(tones[3], Tone::Rgb(0x2f, 0xb1, 0xd1));
        let mut claude_hues = tones[1..3]
            .iter()
            .copied()
            .map(emitted_rgb)
            .collect::<Vec<_>>();
        claude_hues.sort_unstable();
        let mut expected = [palette.raw.red, palette.raw.yellow];
        expected.sort_unstable();
        assert_eq!(claude_hues, expected);
        assert_eq!(emitted_rgb(tones[4]), palette.raw.cyan);
        assert_eq!(emitted_rgb(tones[5]), palette.raw.blue);
        assert!(palette.model_tones(&[], |_, _| true).is_empty());
        let indexed = Palette::resolve(
            &ThemeConfig {
                scheme: Some("TokyoNight Night".into()),
                ..Default::default()
            },
            ColorDepth::Indexed,
        );
        for pair in [
            [brand("kimi"), brand("muse")],
            [brand("antigravity"), brand("kimi")],
            [brand("cursor"), brand("amp")],
            [BrandColor::Rgb(128, 128, 128), BrandColor::Indexed(244)],
        ] {
            let tones = indexed.model_tones(&pair, |_, _| true);
            assert_eq!(
                emitted_rgb(tones[0]),
                emitted_rgb(pair[0].tone(&indexed)),
                "{pair:?}"
            );
            assert_ne!(emitted_rgb(tones[0]), emitted_rgb(tones[1]), "{pair:?}");
        }
    }

    #[test]
    fn model_tones_never_emit_muted_across_bundled_schemes_at_both_depths() {
        let mut failures = Vec::new();
        for scheme in crate::config::scheme_swatches() {
            let config = ThemeConfig {
                scheme: Some(scheme.name.clone()),
                ..Default::default()
            };
            for depth in [ColorDepth::Truecolor, ColorDepth::Indexed] {
                let palette = Palette::resolve(&config, depth);
                for tone in palette.model_tones(&model_brands(), |_, _| true) {
                    if emitted_rgb(tone) == emitted_rgb(palette.muted()) {
                        failures.push((scheme.name.clone(), depth, tone));
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{failures:?}");
    }

    #[test]
    fn model_tones_reuse_colours_only_between_models_not_shown_together() {
        let claude = super::super::resolve_provider_brand("claude", &Default::default());
        for depth in [ColorDepth::Truecolor, ColorDepth::Indexed] {
            let palette = Palette::resolve(
                &ThemeConfig {
                    scheme: Some("TokyoNight Night".into()),
                    ..Default::default()
                },
                depth,
            );
            let tones = palette.model_tones(&[claude; 16], |left, right| left / 8 == right / 8);
            for group in tones.chunks(8) {
                let mut colours = group.iter().copied().map(emitted_rgb).collect::<Vec<_>>();
                colours.sort_unstable();
                colours.dedup();
                assert_eq!(colours.len(), 8, "{depth:?}: {group:?}");
            }
            assert_eq!(
                tones,
                palette.model_tones(&[claude; 16], |left, right| left / 8 == right / 8)
            );
        }
    }

    #[test]
    fn forecast_tone_fades_model_tones_toward_each_schemes_background_at_both_depths() {
        let mut cases = Vec::new();
        for depth in [ColorDepth::Indexed, ColorDepth::Truecolor] {
            for scheme in ["Tokyo Night", "Catppuccin Latte"] {
                let config = ThemeConfig {
                    scheme: Some(scheme.into()),
                    ..Default::default()
                };
                let palette = Palette::resolve(&config, depth);
                let background = oklab::lightness(palette.raw.background);
                for kind in ["claude", "codex"] {
                    let brand = super::super::resolve_provider_brand(kind, &Default::default());
                    for (rank, model) in palette
                        .model_tones(&[brand; 4], |_, _| true)
                        .into_iter()
                        .enumerate()
                    {
                        let forecast = palette.forecast_tone(model);
                        let before = (oklab::lightness(emitted_rgb(model)) - background).abs();
                        let after = (oklab::lightness(emitted_rgb(forecast)) - background).abs();
                        let correct_depth = matches!(
                            (depth, forecast),
                            (ColorDepth::Indexed, Tone::Indexed(_))
                                | (ColorDepth::Truecolor, Tone::Rgb(..))
                        );
                        cases.push((
                            scheme,
                            depth,
                            kind,
                            rank,
                            forecast != model && after < before && correct_depth,
                            (model, forecast, before, after),
                        ));
                    }
                }
            }
        }
        assert!(cases.iter().all(|case| case.4), "{cases:?}");
    }

    fn assert_fast_mode_contrast(name: &str) {
        let config = ThemeConfig {
            scheme: Some(name.to_owned()),
            ..ThemeConfig::default()
        };
        let background = raw_palette_for_theme(&config).background;
        for depth in [ColorDepth::Truecolor, ColorDepth::Indexed] {
            let bolt = emitted_rgb(Palette::resolve(&config, depth).fast_mode());
            let contrast = oklab::contrast_ratio(bolt, background);
            assert!(
                contrast >= 3.0,
                "{name} at {depth:?}: {bolt:?}, {contrast:.3}:1"
            );
        }
    }

    #[test]
    fn fast_mode_is_readable_on_named_dark_and_light_schemes() {
        for name in [
            "TokyoNight Night",
            "TokyoNight Day",
            "Catppuccin Latte",
            "Builtin Light",
        ] {
            assert_fast_mode_contrast(name);
            if name == "TokyoNight Night" {
                continue;
            }
            let config = ThemeConfig {
                scheme: Some(name.to_owned()),
                ..ThemeConfig::default()
            };
            let raw = raw_palette_for_theme(&config);
            let bolt = emitted_rgb(Palette::resolve(&config, ColorDepth::Truecolor).fast_mode());
            let white = (255, 255, 255);
            assert!(
                oklab::contrast_ratio(bolt, white) > oklab::contrast_ratio(raw.yellow, white),
                "{name}: the bolt {bolt:?} must be darker than yellow {:?}",
                raw.yellow
            );
        }
    }

    #[test]
    fn fast_mode_clears_contrast_floor_across_bundled_schemes_at_both_depths() {
        for scheme in crate::config::scheme_swatches() {
            assert_fast_mode_contrast(&scheme.name);
        }
    }

    #[test]
    fn pace_tone_walks_both_ramps_and_follows_configured_bands() {
        for depth in [ColorDepth::Truecolor, ColorDepth::Indexed] {
            let palette = Palette::resolve(&ThemeConfig::default(), depth);
            let warm = |amount| {
                rgb_color(
                    ramp_tone(
                        &palette.heat_ramp,
                        HEAT_RAMP_WARM_START + amount * (1.0 - HEAT_RAMP_WARM_START),
                    ),
                    depth,
                )
            };
            let cool = |amount| rgb_color(ramp_tone(&palette.calm_ramp, amount), depth);
            let defaults = BudgetBurnRateConfig::default();
            let tuned = BudgetBurnRateConfig {
                yellow: 80,
                amber: 120,
                red: 160,
                green: 60,
                deep_green: 20,
            };
            let misordered = BudgetBurnRateConfig {
                yellow: 200,
                amber: 150,
                red: 100,
                green: 20,
                deep_green: 80,
            };
            for (bands, ratio, elapsed_share, expected) in [
                (defaults, 1.0, 1.0, None),
                (defaults, 1.5, 1.0, Some(warm(0.5))),
                (defaults, 2.01, 1.0, Some(warm(1.0))),
                (defaults, 0.33, 0.399, None),
                (defaults, 0.33, 0.4, Some(cool(1.0))),
                (defaults, 0.5, 0.4, Some(cool(0.5))),
                (tuned, 1.2, 1.0, Some(warm(0.5))),
                (tuned, 1.6, 1.0, Some(warm(1.0))),
                (tuned, 0.4, 0.4, Some(cool(0.5))),
                (misordered, 1.2, 1.0, Some(warm(1.0))),
                (misordered, 0.5, 0.4, Some(cool(1.0))),
            ] {
                assert_eq!(
                    palette.pace_tone(
                        PaceReading {
                            ratio,
                            elapsed_share
                        },
                        &bands
                    ),
                    expected,
                    "{bands:?}, {ratio}, {elapsed_share} at {depth:?}"
                );
            }
        }
    }

    #[test]
    fn budget_tone_lands_each_zone_on_its_ramp_stop_and_degrades_worst_first() {
        for depth in [ColorDepth::Truecolor, ColorDepth::Indexed] {
            let palette = Palette::resolve(&ThemeConfig::default(), depth);
            let stop = |amount| rgb_color(ramp_tone(&palette.heat_ramp, amount), depth);
            let tuned = BudgetBarConfig {
                yellow: 80,
                amber: 40,
                red: 20,
                ..BudgetBarConfig::default()
            };
            for (zones, remaining, amount) in [
                (BudgetBarConfig::default(), 100, 0.0),
                (BudgetBarConfig::default(), 50, 1.0 / 3.0),
                (BudgetBarConfig::default(), 25, 2.0 / 3.0),
                (BudgetBarConfig::default(), 10, 1.0),
                (BudgetBarConfig::default(), 9, 1.0),
                (BudgetBarConfig::default(), 0, 1.0),
                (tuned, 80, 1.0 / 3.0),
                (tuned, 40, 2.0 / 3.0),
                (tuned, 19, 1.0),
            ] {
                assert_eq!(
                    palette.budget_tone(remaining, &zones),
                    stop(amount),
                    "{remaining}% left under {zones:?} at {depth:?}"
                );
            }
            // Between two stops the tone is blended, not snapped to either.
            assert_eq!(
                palette.budget_tone(75, &BudgetBarConfig::default()),
                stop(1.0 / 6.0)
            );
            let misordered = BudgetBarConfig {
                yellow: 25,
                amber: 10,
                red: 50,
                ..BudgetBarConfig::default()
            };
            assert_eq!(palette.budget_tone(30, &misordered), stop(1.0));
            assert_ne!(palette.budget_tone(50, &misordered), stop(1.0));
        }
    }

    #[test]
    fn cache_expired_blends_alarm_halfway_to_muted_and_follows_their_overrides() {
        const ALARM: (u8, u8, u8) = (0xd7, 0x00, 0x00);
        const MUTED: (u8, u8, u8) = (0x60, 0x70, 0x80);
        let overridden = ThemeConfig {
            alarm: Some(ThemeColor::Rgb(ALARM.0, ALARM.1, ALARM.2)),
            muted: Some(ThemeColor::Rgb(MUTED.0, MUTED.1, MUTED.2)),
            ..ThemeConfig::default()
        };
        let scheme = raw_palette_for_theme(&ThemeConfig::default()).derive_tones();
        for depth in [ColorDepth::Truecolor, ColorDepth::Indexed] {
            for (theme, alarm, muted) in [
                (ThemeConfig::default(), scheme.alarm, scheme.muted),
                (overridden.clone(), ALARM, MUTED),
            ] {
                assert_eq!(
                    Palette::resolve(&theme, depth).cache_expired,
                    rgb_color(oklab::blend(alarm, muted, 0.5), depth),
                    "alarm {alarm:?} and muted {muted:?} at {depth:?}"
                );
            }
        }
    }
}
