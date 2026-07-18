//! Terminal-color data resolved independently of a renderer.

use crate::config::{ColorDepth, nearest_xterm_index};

/// A color ready for a renderer edge to translate into its native carrier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Indexed(u8),
    Rgb(u8, u8, u8),
}

impl Tone {
    pub fn from_rgb((red, green, blue): (u8, u8, u8), depth: ColorDepth) -> Self {
        match depth {
            ColorDepth::Truecolor => Self::Rgb(red, green, blue),
            ColorDepth::Indexed => Self::Indexed(nearest_xterm_index(red, green, blue)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_quantizes_at_indexed_depth() {
        assert_eq!(
            Tone::from_rgb((0xd9, 0x77, 0x57), ColorDepth::Indexed),
            Tone::Indexed(173)
        );
        assert_eq!(
            Tone::from_rgb((1, 2, 3), ColorDepth::Truecolor),
            Tone::Rgb(1, 2, 3)
        );
    }
}
