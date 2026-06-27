//! Decodes pet WebP sheets and slices them into fixed-size animation frames.

use std::io::{BufReader, Cursor};

use super::catalog::{
    FRAME_COUNT, FRAME_HEIGHT, FRAME_WIDTH, SHEET_COLS, SHEET_HEIGHT, SHEET_WIDTH,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RgbaImage {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) data: Vec<u8>,
}

impl RgbaImage {
    pub(crate) fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let offset = ((y * self.width + x) * 4) as usize;
        [
            self.data[offset],
            self.data[offset + 1],
            self.data[offset + 2],
            self.data[offset + 3],
        ]
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum FrameErr {
    #[error("webp decode failed: {0}")]
    Decode(String),
    #[error("expected pet sheet geometry {expected:?}, got {actual:?}")]
    Geometry {
        expected: (u32, u32),
        actual: (u32, u32),
    },
    #[error("decoded buffer size does not match dimensions")]
    BufferSize,
}

pub(crate) fn validate_sheet_geometry(bytes: &[u8]) -> Result<(), FrameErr> {
    let decoder = decoder(bytes)?;
    let actual = decoder.dimensions();
    if actual != (SHEET_WIDTH, SHEET_HEIGHT) {
        return Err(FrameErr::Geometry {
            expected: (SHEET_WIDTH, SHEET_HEIGHT),
            actual,
        });
    }
    Ok(())
}

pub(crate) fn decode_sheet(bytes: &[u8]) -> Result<Vec<RgbaImage>, FrameErr> {
    let mut decoder = decoder(bytes)?;
    let actual = decoder.dimensions();
    if actual != (SHEET_WIDTH, SHEET_HEIGHT) {
        return Err(FrameErr::Geometry {
            expected: (SHEET_WIDTH, SHEET_HEIGHT),
            actual,
        });
    }
    let mut raw = vec![0; decoder.output_buffer_size().ok_or(FrameErr::BufferSize)?];
    let has_alpha = decoder.has_alpha();
    decoder
        .read_image(&mut raw)
        .map_err(|err| FrameErr::Decode(err.to_string()))?;
    let sheet = if has_alpha {
        RgbaImage {
            width: SHEET_WIDTH,
            height: SHEET_HEIGHT,
            data: raw,
        }
    } else {
        let mut data = Vec::with_capacity((SHEET_WIDTH * SHEET_HEIGHT * 4) as usize);
        for rgb in raw.chunks_exact(3) {
            data.extend_from_slice(rgb);
            data.push(255);
        }
        RgbaImage {
            width: SHEET_WIDTH,
            height: SHEET_HEIGHT,
            data,
        }
    };
    Ok((0..FRAME_COUNT)
        .map(|index| slice_frame(&sheet, index))
        .collect())
}

fn decoder(bytes: &[u8]) -> Result<image_webp::WebPDecoder<BufReader<Cursor<&[u8]>>>, FrameErr> {
    image_webp::WebPDecoder::new(BufReader::new(Cursor::new(bytes)))
        .map_err(|err| FrameErr::Decode(err.to_string()))
}

pub(crate) fn slice_frame(sheet: &RgbaImage, sprite_index: usize) -> RgbaImage {
    let col = (sprite_index as u32) % SHEET_COLS;
    let row = (sprite_index as u32) / SHEET_COLS;
    let x0 = col * FRAME_WIDTH;
    let y0 = row * FRAME_HEIGHT;
    let mut data = Vec::with_capacity((FRAME_WIDTH * FRAME_HEIGHT * 4) as usize);
    for y in y0..(y0 + FRAME_HEIGHT) {
        let start = ((y * sheet.width + x0) * 4) as usize;
        let end = start + (FRAME_WIDTH * 4) as usize;
        data.extend_from_slice(&sheet.data[start..end]);
    }
    RgbaImage {
        width: FRAME_WIDTH,
        height: FRAME_HEIGHT,
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sprite_index_slices_expected_sheet_region() {
        let mut sheet = RgbaImage {
            width: SHEET_WIDTH,
            height: SHEET_HEIGHT,
            data: vec![0; (SHEET_WIDTH * SHEET_HEIGHT * 4) as usize],
        };
        let index = 9;
        let x0 = FRAME_WIDTH;
        let y0 = FRAME_HEIGHT;
        let offset = ((y0 * SHEET_WIDTH + x0) * 4) as usize;
        sheet.data[offset..offset + 4].copy_from_slice(&[1, 2, 3, 4]);

        let frame = slice_frame(&sheet, index);

        assert_eq!(frame.width, FRAME_WIDTH);
        assert_eq!(frame.height, FRAME_HEIGHT);
        assert_eq!(frame.pixel(0, 0), [1, 2, 3, 4]);
    }

    #[test]
    fn geometry_rejects_non_sheet_webp() {
        let bytes = [
            0x52, 0x49, 0x46, 0x46, 0x3c, 0x00, 0x00, 0x00, 0x57, 0x45, 0x42, 0x50, 0x56, 0x50,
            0x38, 0x20, 0x30, 0x00, 0x00, 0x00, 0xd0, 0x01, 0x00, 0x9d, 0x01, 0x2a, 0x03, 0x00,
            0x03, 0x00, 0x02, 0x00, 0x34, 0x25, 0xa0, 0x02, 0x74, 0xba, 0x01, 0xf8, 0x00, 0x03,
            0xb0, 0x00, 0xfe, 0xf0, 0xc4, 0x0b, 0xff, 0x20, 0xb9, 0x61, 0x75, 0xc8, 0xd7, 0xff,
            0x20, 0x3f, 0xe4, 0x07, 0xfc, 0x80, 0xff, 0xf8, 0xf2, 0x00, 0x00, 0x00,
        ];

        assert!(matches!(
            validate_sheet_geometry(&bytes),
            Err(FrameErr::Geometry {
                expected: (SHEET_WIDTH, SHEET_HEIGHT),
                actual: (3, 3)
            })
        ));
    }
}
