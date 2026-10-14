use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use serde::de::DeserializeOwned;

use crate::store::event::EventEnvelope;

use super::{EventLogErr, Result, testkit};

/// Encode one frame: `<decimal payload length> <crc32 of the payload,
/// 8 lowercase hex chars> <payload>\n`. The CRC covers the payload bytes
/// alone — the length is validated structurally on read — and makes
/// post-power-cut recovery deterministic.
pub(super) fn encode_frame(payload: &[u8]) -> Vec<u8> {
    let mut line = Vec::with_capacity(payload.len() + 24);
    line.extend_from_slice(payload.len().to_string().as_bytes());
    line.push(b' ');
    line.extend_from_slice(format!("{:08x}", crc32fast::hash(payload)).as_bytes());
    line.push(b' ');
    line.extend_from_slice(payload);
    line.push(b'\n');
    line
}

/// Split the log into raw `(offset, terminated, line bytes)` rows from byte
/// `start` — the scan `read_from_offset` folds and `repair` validates.
pub(in crate::store) fn read_rows(path: &Path, start: u64) -> Result<Vec<(u64, bool, Vec<u8>)>> {
    // A missing log reads as empty, decided by the open itself: rotation can
    // rename the active log away between any existence check and the open.
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(EventLogErr::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    file.seek(SeekFrom::Start(start))
        .map_err(|source| EventLogErr::Io {
            path: path.to_path_buf(),
            source,
        })?;
    let mut reader = BufReader::new(file);
    let mut rows: Vec<(u64, bool, Vec<u8>)> = Vec::new();
    let mut offset = start;
    loop {
        let mut buf = Vec::new();
        let read = reader
            .read_until(b'\n', &mut buf)
            .map_err(|source| EventLogErr::Io {
                path: path.to_path_buf(),
                source,
            })?;
        if read == 0 {
            break;
        }
        let terminated = buf.last() == Some(&b'\n');
        if terminated {
            buf.pop();
        }
        rows.push((offset, terminated, buf));
        offset += read as u64;
    }
    testkit::count_bytes_read(offset - start);
    testkit::count_path_bytes_read(path, offset - start);
    Ok(rows)
}

/// Read the terminated row ending at `end`, searching backward in bounded blocks.
pub(in crate::store) fn read_row_ending_at(
    path: &Path,
    end: u64,
) -> Result<Option<(u64, Vec<u8>)>> {
    if end == 0 {
        return Ok(None);
    }
    let mut read = 0;
    let result = (|| -> io::Result<_> {
        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(end - 1))?;
        let mut newline = [0];
        file.read_exact(&mut newline)?;
        read += 1;
        if newline[0] != b'\n' {
            return Ok(None);
        }
        let mut at = end - 1;
        let mut row = Vec::new();
        let mut block = [0; 256];
        while at > 0 {
            let count = at.min(block.len() as u64) as usize;
            at -= count as u64;
            file.seek(SeekFrom::Start(at))?;
            file.read_exact(&mut block[..count])?;
            read += count as u64;
            if let Some(previous) = block[..count].iter().rposition(|byte| *byte == b'\n') {
                row.extend(block[previous + 1..count].iter().rev());
                at += previous as u64 + 1;
                break;
            }
            row.extend(block[..count].iter().rev());
        }
        row.reverse();
        Ok(Some((at, row)))
    })();
    testkit::count_bytes_read(read);
    testkit::count_path_bytes_read(path, read);
    match result {
        Err(source)
            if matches!(
                source.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::UnexpectedEof
            ) =>
        {
            Ok(None)
        }
        result => result.map_err(|source| EventLogErr::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Decode one raw row into its event: unterminated and non-UTF-8 rows read as
/// torn, terminated ones go through the frame decoder.
pub(super) fn decode_row(at: u64, terminated: bool, bytes: &[u8]) -> Result<EventEnvelope> {
    decode_record(at, terminated, bytes)
}

pub(in crate::store) fn decode_record<T: DeserializeOwned>(
    at: u64,
    terminated: bool,
    bytes: &[u8],
) -> Result<T> {
    if !terminated {
        return Err(EventLogErr::Torn {
            offset: at,
            reason: "unterminated frame".into(),
        });
    }
    match std::str::from_utf8(bytes) {
        Ok(line) => decode_line(line, at),
        Err(err) => Err(EventLogErr::Torn {
            offset: at,
            reason: format!("utf8: {err}"),
        }),
    }
}

fn decode_line<T: DeserializeOwned>(line: &str, offset: u64) -> Result<T> {
    let (len, rest) = line.split_once(' ').ok_or_else(|| EventLogErr::Torn {
        offset,
        reason: "no length prefix".into(),
    })?;
    let claimed: u64 = len.parse().map_err(|_| EventLogErr::Torn {
        offset,
        reason: format!("bad length `{len}`"),
    })?;
    // CRC form `<len> <crc> <json>` vs the pre-CRC `<len> <json>`: an 8-char
    // lowercase-hex second token is the CRC — a JSON payload always opens with
    // `{`, so the forms cannot be confused.
    let (crc, payload) = match rest.split_once(' ') {
        Some((token, payload))
            if token.len() == 8
                && token
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) =>
        {
            // The guard admits only 8 lowercase-hex bytes, so the parse holds.
            (u32::from_str_radix(token, 16).ok(), payload)
        }
        _ => (None, rest),
    };
    let available = payload.len() as u64;
    if claimed != available {
        return Err(EventLogErr::FrameLength {
            offset,
            claimed,
            available,
        });
    }
    if let Some(claimed_crc) = crc {
        let computed = crc32fast::hash(payload.as_bytes());
        if claimed_crc != computed {
            return Err(EventLogErr::Crc {
                offset,
                claimed: claimed_crc,
                computed,
            });
        }
    }
    serde_json::from_str(payload).map_err(|e| EventLogErr::Torn {
        offset,
        reason: format!("json: {e}"),
    })
}
