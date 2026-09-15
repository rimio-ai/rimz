//! Streaming byte, physical-line, and estimated-token counts for output files.

use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::iter::Sum;
use std::ops::AddAssign;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::utils::tokens;

/// Bytes tokenized per file; the rest of a larger file scales the sample's count.
const TOKEN_SAMPLE_BYTES: usize = 1 << 20;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSummary {
    pub bytes: u64,
    pub lines: u64,
    #[serde(default)]
    pub tokens: u64,
}

impl FileSummary {
    pub fn measure(path: &Path) -> io::Result<Self> {
        let mut reader = BufReader::new(File::open(path)?);
        let mut summary = Self::default();
        let mut sample = Vec::new();
        let mut last = None;
        loop {
            let buffer = reader.fill_buf()?;
            if buffer.is_empty() {
                break;
            }
            let len = buffer.len();
            let room = TOKEN_SAMPLE_BYTES.saturating_sub(sample.len());
            sample.extend_from_slice(&buffer[..len.min(room)]);
            summary.bytes += len as u64;
            summary.lines += buffer.iter().filter(|&&byte| byte == b'\n').count() as u64;
            last = buffer.last().copied();
            reader.consume(len);
        }
        if last.is_some_and(|byte| byte != b'\n') {
            summary.lines += 1;
        }
        summary.tokens = scaled_estimate(&sample, summary.bytes);
        Ok(summary)
    }

    pub fn is_empty(&self) -> bool {
        self.bytes == 0
    }

    /// `~1.2k tokens, 84 lines`: the one rendering of an output file's size.
    pub fn label(&self) -> String {
        let lines = match self.lines {
            1 => "1 line".to_owned(),
            lines => format!("{lines} lines"),
        };
        format!("{} tokens, {lines}", approx_tokens(self.tokens))
    }
}

fn scaled_estimate(sample: &[u8], bytes: u64) -> u64 {
    let sample_len = sample.len() as u64;
    if sample_len == 0 {
        return 0;
    }
    let sampled = tokens::estimate(&String::from_utf8_lossy(sample));
    if bytes <= sample_len {
        return sampled;
    }
    let scaled = u128::from(sampled) * u128::from(bytes) + u128::from(sample_len / 2);
    u64::try_from(scaled / u128::from(sample_len)).unwrap_or(u64::MAX)
}

/// An estimated token count: `<1k`, `~1.2k`, `~22k`, `~1.2M`, rounded half-up.
fn approx_tokens(tokens: u64) -> String {
    if tokens < 1_000 {
        return "<1k".to_owned();
    }
    let thousand_tenths = (tokens + 50) / 100;
    if thousand_tenths < 100 {
        return format!("~{}k", tenths_label(thousand_tenths));
    }
    let thousands = (tokens + 500) / 1_000;
    if thousands < 1_000 {
        return format!("~{thousands}k");
    }
    let million_tenths = (tokens + 50_000) / 100_000;
    if million_tenths < 100 {
        return format!("~{}M", tenths_label(million_tenths));
    }
    format!("~{}M", (tokens + 500_000) / 1_000_000)
}

fn tenths_label(tenths: u64) -> String {
    match tenths % 10 {
        0 => (tenths / 10).to_string(),
        decimal => format!("{}.{decimal}", tenths / 10),
    }
}

impl AddAssign for FileSummary {
    fn add_assign(&mut self, other: Self) {
        self.bytes += other.bytes;
        self.lines += other.lines;
        self.tokens += other.tokens;
    }
}

impl Sum for FileSummary {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::default(), |mut total, summary| {
            total += summary;
            total
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approx_tokens_rounds_to_one_significant_step() {
        for (tokens, label) in [
            (0, "<1k"),
            (999, "<1k"),
            (1_000, "~1k"),
            (1_049, "~1k"),
            (1_250, "~1.3k"),
            (9_949, "~9.9k"),
            (9_950, "~10k"),
            (22_400, "~22k"),
            (999_499, "~999k"),
            (999_500, "~1M"),
            (1_234_567, "~1.2M"),
            (12_600_000, "~13M"),
        ] {
            assert_eq!(approx_tokens(tokens), label, "{tokens}");
        }
    }
}
