//! Decodes pet WebP and PNG sheets and slices them into fixed-size animation frames.

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
    #[error("pet sheet decode failed: {0}")]
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
    validate_dimensions(sheet_dimensions(bytes)?)
}

pub(crate) fn decode_sheet(bytes: &[u8]) -> Result<Vec<RgbaImage>, FrameErr> {
    let sheet = match sniff(bytes)? {
        SheetFormat::WebP => decode_webp_rgba(bytes)?,
        SheetFormat::Png => decode_png_rgba(bytes)?,
    };
    validate_decoded_sheet(&sheet)?;
    Ok((0..FRAME_COUNT)
        .map(|index| slice_frame(&sheet, index))
        .collect())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SheetFormat {
    WebP,
    Png,
}

const PNG_SIG: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];

fn sniff(bytes: &[u8]) -> Result<SheetFormat, FrameErr> {
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(&b"WEBP"[..]) {
        Ok(SheetFormat::WebP)
    } else if bytes.starts_with(&PNG_SIG) {
        Ok(SheetFormat::Png)
    } else {
        Err(FrameErr::Decode(
            "unrecognized sheet format (expected WebP or PNG)".to_owned(),
        ))
    }
}

fn sheet_dimensions(bytes: &[u8]) -> Result<(u32, u32), FrameErr> {
    match sniff(bytes)? {
        SheetFormat::WebP => Ok(webp_decoder(bytes)?.dimensions()),
        SheetFormat::Png => png_dimensions(bytes),
    }
}

fn validate_decoded_sheet(sheet: &RgbaImage) -> Result<(), FrameErr> {
    validate_dimensions((sheet.width, sheet.height))?;
    if sheet.data.len()
        != expected_rgba_len(sheet.width, sheet.height).ok_or(FrameErr::BufferSize)?
    {
        return Err(FrameErr::BufferSize);
    }
    Ok(())
}

fn validate_dimensions(actual: (u32, u32)) -> Result<(), FrameErr> {
    if actual != (SHEET_WIDTH, SHEET_HEIGHT) {
        return Err(FrameErr::Geometry {
            expected: (SHEET_WIDTH, SHEET_HEIGHT),
            actual,
        });
    }
    Ok(())
}

fn expected_rgba_len(width: u32, height: u32) -> Option<usize> {
    usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4)
}

fn webp_decoder(
    bytes: &[u8],
) -> Result<image_webp::WebPDecoder<BufReader<Cursor<&[u8]>>>, FrameErr> {
    image_webp::WebPDecoder::new(BufReader::new(Cursor::new(bytes))).map_err(decode_err)
}

fn decode_webp_rgba(bytes: &[u8]) -> Result<RgbaImage, FrameErr> {
    let mut decoder = webp_decoder(bytes)?;
    let (width, height) = decoder.dimensions();
    let mut raw = vec![0; decoder.output_buffer_size().ok_or(FrameErr::BufferSize)?];
    let has_alpha = decoder.has_alpha();
    decoder.read_image(&mut raw).map_err(decode_err)?;
    let data = if has_alpha {
        raw
    } else {
        let mut data =
            Vec::with_capacity(expected_rgba_len(width, height).ok_or(FrameErr::BufferSize)?);
        for rgb in raw.chunks_exact(3) {
            data.extend_from_slice(rgb);
            data.push(255);
        }
        data
    };
    Ok(RgbaImage {
        width,
        height,
        data,
    })
}

fn png_dimensions(bytes: &[u8]) -> Result<(u32, u32), FrameErr> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    let info = decoder.read_header_info().map_err(decode_err)?;
    Ok(info.size())
}

fn decode_png_rgba(bytes: &[u8]) -> Result<RgbaImage, FrameErr> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(decode_err)?;
    let mut raw = vec![0; reader.output_buffer_size().ok_or(FrameErr::BufferSize)?];
    let info = reader.next_frame(&mut raw).map_err(decode_err)?;
    if info.bit_depth != png::BitDepth::Eight {
        return Err(FrameErr::Decode(format!(
            "unsupported PNG bit depth after normalization: {:?}",
            info.bit_depth
        )));
    }
    let data = png_to_rgba8(&raw[..info.buffer_size()], info.color_type)?;
    Ok(RgbaImage {
        width: info.width,
        height: info.height,
        data,
    })
}

pub fn encode_png(width: u32, height: u32, data: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fastest);
        // Callers pass decoded RGBA frame data into an in-memory Vec sink.
        let mut writer = encoder
            .write_header()
            .expect("encoding in-memory PNG header cannot fail");
        writer
            .write_image_data(data)
            .expect("encoding valid RGBA frame data cannot fail");
    }
    bytes
}

fn png_to_rgba8(data: &[u8], color_type: png::ColorType) -> Result<Vec<u8>, FrameErr> {
    match color_type {
        png::ColorType::Rgba => Ok(data.to_vec()),
        png::ColorType::Rgb => expand_chunks(data, 3, |rgb, out| {
            out.extend_from_slice(rgb);
            out.push(255);
        }),
        png::ColorType::Grayscale => expand_chunks(data, 1, |gray, out| {
            out.extend_from_slice(&[gray[0], gray[0], gray[0], 255]);
        }),
        png::ColorType::GrayscaleAlpha => expand_chunks(data, 2, |gray, out| {
            out.extend_from_slice(&[gray[0], gray[0], gray[0], gray[1]]);
        }),
        png::ColorType::Indexed => Err(FrameErr::Decode(
            "unsupported PNG color type after normalization: Indexed".to_owned(),
        )),
    }
}

fn expand_chunks(
    data: &[u8],
    chunk_size: usize,
    mut push_rgba: impl FnMut(&[u8], &mut Vec<u8>),
) -> Result<Vec<u8>, FrameErr> {
    let mut chunks = data.chunks_exact(chunk_size);
    let mut rgba = Vec::with_capacity((data.len() / chunk_size) * 4);
    for chunk in chunks.by_ref() {
        push_rgba(chunk, &mut rgba);
    }
    if !chunks.remainder().is_empty() {
        return Err(FrameErr::BufferSize);
    }
    Ok(rgba)
}

fn decode_err(err: impl std::fmt::Display) -> FrameErr {
    FrameErr::Decode(err.to_string())
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
    fn sniff_recognizes_webp_png_and_rejects_unknown() {
        let mut webp = [0; 12];
        webp[..4].copy_from_slice(b"RIFF");
        webp[8..12].copy_from_slice(b"WEBP");

        assert_eq!(sniff(&webp).unwrap(), SheetFormat::WebP);
        assert_eq!(sniff(&PNG_SIG).unwrap(), SheetFormat::Png);
        assert!(matches!(
            sniff(b"not an image"),
            Err(FrameErr::Decode(message))
                if message == "unrecognized sheet format (expected WebP or PNG)"
        ));
    }

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

    #[test]
    fn expected_geometry_png_decodes_to_frames() {
        let bytes = solid_png(SHEET_WIDTH, SHEET_HEIGHT, [9, 8, 7, 6]);

        validate_sheet_geometry(&bytes).unwrap();
        let frames = decode_sheet(&bytes).unwrap();

        assert_eq!(frames.len(), FRAME_COUNT);
        assert_eq!(frames[0].width, FRAME_WIDTH);
        assert_eq!(frames[0].height, FRAME_HEIGHT);
        assert_eq!(frames[0].pixel(0, 0), [9, 8, 7, 6]);
    }

    #[test]
    fn geometry_rejects_non_sheet_png() {
        let bytes = solid_png(3, 3, [1, 2, 3, 4]);

        assert!(matches!(
            validate_sheet_geometry(&bytes),
            Err(FrameErr::Geometry {
                expected: (SHEET_WIDTH, SHEET_HEIGHT),
                actual: (3, 3)
            })
        ));
        assert!(matches!(
            decode_sheet(&bytes),
            Err(FrameErr::Geometry {
                expected: (SHEET_WIDTH, SHEET_HEIGHT),
                actual: (3, 3)
            })
        ));
    }

    #[test]
    fn encode_png_round_trips_through_decoder() {
        let data = vec![0, 1, 2, 3, 4, 5, 6, 7];

        let encoded = encode_png(2, 1, &data);
        let decoded = decode_png_rgba(&encoded).expect("encoded png decodes");

        assert_eq!(decoded.width, 2);
        assert_eq!(decoded.height, 1);
        assert_eq!(decoded.data, data);
    }

    fn solid_png(width: u32, height: u32, pixel: [u8; 4]) -> Vec<u8> {
        let mut data = Vec::with_capacity(width as usize * height as usize * 4);
        for _ in 0..(width * height) {
            data.extend_from_slice(&pixel);
        }
        encode_png(width, height, &data)
    }
}
