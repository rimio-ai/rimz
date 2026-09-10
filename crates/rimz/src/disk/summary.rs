//! Streaming byte and physical-line counts for output files.

use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSummary {
    pub bytes: u64,
    pub lines: u64,
}

impl FileSummary {
    pub fn measure(path: &Path) -> io::Result<Self> {
        let mut reader = BufReader::new(File::open(path)?);
        let mut summary = Self::default();
        let mut last = None;
        loop {
            let buffer = reader.fill_buf()?;
            if buffer.is_empty() {
                break;
            }
            let len = buffer.len();
            summary.bytes += len as u64;
            summary.lines += buffer.iter().filter(|&&byte| byte == b'\n').count() as u64;
            last = buffer.last().copied();
            reader.consume(len);
        }
        if last.is_some_and(|byte| byte != b'\n') {
            summary.lines += 1;
        }
        Ok(summary)
    }

    pub fn lines_label(&self) -> String {
        match self.lines {
            1 => "1 line".to_owned(),
            lines => format!("{lines} lines"),
        }
    }
}
