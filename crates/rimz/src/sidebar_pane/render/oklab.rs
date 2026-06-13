//! Perceptual color math in the OKLab space, shared by scheme derivation and
//! the renderer. Blending, lightness lifts, and the chroma-preserving mutes
//! that back derived semantic and component tones all happen here, so every
//! generated tone steps evenly to the eye instead of in raw sRGB.
//!
//! Inputs and outputs are 8-bit sRGB tuples ([`Rgb`]); the OKLab/OKLCH forms
//! stay internal. Conversions use Björn Ottosson's coefficients; round-trips
//! clamp per channel back into sRGB gamut.

pub(crate) type Rgb = (u8, u8, u8);

/// Perceptually-even interpolation between two sRGB colors: `amount` of `0.0`
/// returns `left`, `1.0` returns `right`, blended in OKLab so the midpoint
/// reads as the visual midpoint.
pub(crate) fn blend(left: Rgb, right: Rgb, amount: f32) -> Rgb {
    let left = Oklab::from_rgb(left);
    let right = Oklab::from_rgb(right);
    Oklab {
        l: lerp(left.l, right.l, amount),
        a: lerp(left.a, right.a, amount),
        b: lerp(left.b, right.b, amount),
    }
    .to_rgb()
}

/// Shift a color's OKLab lightness by `delta`, holding its hue. A brightening
/// lift can push a saturated tone past the sRGB ceiling, where a per-channel
/// clamp would skew the hue (red → pink, blue → cyan); there, chroma eases
/// toward neutral just enough to fit, so the tone keeps its color and only loses
/// saturation as it nears white. A dimming lift stays a plain lightness drop —
/// it does not overshoot the ceiling, so its hue holds without easing chroma.
pub(crate) fn lift_lightness(rgb: Rgb, delta: f32) -> Rgb {
    let mut color = Oklab::from_rgb(rgb);
    color.l = (color.l + delta).clamp(0.0, 1.0);
    if delta > 0.0 {
        color = color.fit_to_gamut();
    }
    color.to_rgb()
}

/// Iterations of the chroma-fit bisection: 16 resolves the scale to 1/65536,
/// far finer than 8-bit sRGB can show.
const GAMUT_FIT_ITERS: usize = 16;
/// Linear-channel tolerance when testing sRGB-gamut membership.
const GAMUT_EPS: f32 = 1e-4;

fn lerp(left: f32, right: f32, amount: f32) -> f32 {
    left + (right - left) * amount
}

#[derive(Clone, Copy, Debug)]
struct Oklab {
    l: f32,
    a: f32,
    b: f32,
}

impl Oklab {
    fn from_rgb((red, green, blue): Rgb) -> Self {
        let r = srgb_to_linear(red);
        let g = srgb_to_linear(green);
        let b = srgb_to_linear(blue);

        let l = 0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b;
        let m = 0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b;
        let s = 0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b;

        let l_ = l.cbrt();
        let m_ = m.cbrt();
        let s_ = s.cbrt();

        Self {
            l: 0.210_454_26 * l_ + 0.793_617_8 * m_ - 0.004_072_047 * s_,
            a: 1.977_998_5 * l_ - 2.428_592_2 * m_ + 0.450_593_7 * s_,
            b: 0.025_904_037 * l_ + 0.782_771_77 * m_ - 0.808_675_77 * s_,
        }
    }

    /// Linear sRGB channels before the per-channel encode and clamp, so gamut
    /// tests see the true out-of-range values.
    fn to_linear(self) -> (f32, f32, f32) {
        let l_ = self.l + 0.396_337_78 * self.a + 0.215_803_76 * self.b;
        let m_ = self.l - 0.105_561_346 * self.a - 0.063_854_17 * self.b;
        let s_ = self.l - 0.089_484_18 * self.a - 1.291_485_5 * self.b;

        let l = l_ * l_ * l_;
        let m = m_ * m_ * m_;
        let s = s_ * s_ * s_;

        (
            4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
            -1.268_438 * l + 2.609_757_4 * m - 0.341_319_4 * s,
            -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
        )
    }

    fn to_rgb(self) -> Rgb {
        let (red, green, blue) = self.to_linear();
        (
            linear_to_srgb(red),
            linear_to_srgb(green),
            linear_to_srgb(blue),
        )
    }

    /// Whether the tone sits inside the sRGB gamut: every linear channel within
    /// `[0, 1]`, give or take [`GAMUT_EPS`].
    fn in_gamut(self) -> bool {
        let (red, green, blue) = self.to_linear();
        let within = |value: f32| (-GAMUT_EPS..=1.0 + GAMUT_EPS).contains(&value);
        within(red) && within(green) && within(blue)
    }

    /// Ease chroma toward the neutral axis just until the tone fits the sRGB
    /// gamut, holding lightness and hue. Scaling `a` and `b` by one factor keeps
    /// the hue angle fixed, so only saturation gives way. A no-op when already in
    /// gamut, so an in-range lift is returned unchanged.
    fn fit_to_gamut(self) -> Self {
        if self.in_gamut() {
            return self;
        }
        let (mut lo, mut hi) = (0.0_f32, 1.0_f32);
        for _ in 0..GAMUT_FIT_ITERS {
            let mid = 0.5 * (lo + hi);
            let candidate = Self {
                l: self.l,
                a: self.a * mid,
                b: self.b * mid,
            };
            if candidate.in_gamut() {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        Self {
            l: self.l,
            a: self.a * lo,
            b: self.b * lo,
        }
    }
}

fn srgb_to_linear(value: u8) -> f32 {
    let value = f32::from(value) / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(value: f32) -> u8 {
    let value = value.clamp(0.0, 1.0);
    let value = if value <= 0.003_130_8 {
        12.92 * value
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (value * 255.0).round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lift_lightness_preserves_oklab_hue_axes() {
        let base = Oklab::from_rgb((0xdf, 0xb6, 0x6d));
        let lifted = Oklab::from_rgb(lift_lightness((0xdf, 0xb6, 0x6d), 0.05));
        assert!(
            lifted.l > base.l,
            "lightness should move upward through OKLab"
        );
        assert!((lifted.a - base.a).abs() < 0.01);
        assert!((lifted.b - base.b).abs() < 0.01);
    }

    #[test]
    fn clipping_lift_holds_hue_by_easing_chroma() {
        // A saturated tone lifted hard would clip per-channel and skew its hue
        // (red → pink, blue → cyan); the chroma-fit must hold the hue angle and
        // give up only saturation instead.
        let base = Oklab::from_rgb((0xf7, 0x76, 0x8e));
        let lifted = Oklab::from_rgb(lift_lightness((0xf7, 0x76, 0x8e), 0.10));
        let hue = |c: Oklab| c.b.atan2(c.a);
        assert!(lifted.l > base.l, "lightness still rises");
        assert!(
            (hue(lifted) - hue(base)).abs() < 0.03,
            "hue angle holds despite the gamut-clipping lift"
        );
        assert!(
            lifted.a.hypot(lifted.b) < base.a.hypot(base.b),
            "chroma eases to stay in gamut"
        );
    }

    #[test]
    fn blend_endpoints_return_inputs() {
        let left = (0x10, 0x20, 0x30);
        let right = (0xc0, 0xa0, 0x40);
        assert_eq!(blend(left, right, 0.0), left);
        assert_eq!(blend(left, right, 1.0), right);
    }
}
