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

/// Shift a color's OKLab lightness by `delta` without moving its hue axes, so
/// a breathing or pulsing element brightens and dims along one perceptual axis.
pub(crate) fn lift_lightness(rgb: Rgb, delta: f32) -> Rgb {
    let mut color = Oklab::from_rgb(rgb);
    color.l = (color.l + delta).clamp(0.0, 1.0);
    color.to_rgb()
}

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

    fn to_rgb(self) -> Rgb {
        let l_ = self.l + 0.396_337_78 * self.a + 0.215_803_76 * self.b;
        let m_ = self.l - 0.105_561_346 * self.a - 0.063_854_17 * self.b;
        let s_ = self.l - 0.089_484_18 * self.a - 1.291_485_5 * self.b;

        let l = l_ * l_ * l_;
        let m = m_ * m_ * m_;
        let s = s_ * s_ * s_;

        let red = 4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s;
        let green = -1.268_438 * l + 2.609_757_4 * m - 0.341_319_4 * s;
        let blue = -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s;

        (
            linear_to_srgb(red),
            linear_to_srgb(green),
            linear_to_srgb(blue),
        )
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
    fn blend_endpoints_return_inputs() {
        let left = (0x10, 0x20, 0x30);
        let right = (0xc0, 0xa0, 0x40);
        assert_eq!(blend(left, right, 0.0), left);
        assert_eq!(blend(left, right, 1.0), right);
    }
}
