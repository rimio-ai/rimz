use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

use crate::ledger::event::EventEnvelope;

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
pub(super) fn read_rows(path: &Path, start: u64) -> Result<Vec<(u64, bool, Vec<u8>)>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut file = File::open(path).map_err(|e| EventLogErr::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
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
    Ok(rows)
}

/// Decode one raw row into its event: unterminated and non-UTF-8 rows read as
/// torn, terminated ones go through the frame decoder.
pub(super) fn decode_row(at: u64, terminated: bool, bytes: &[u8]) -> Result<EventEnvelope> {
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

fn decode_line(line: &str, offset: u64) -> Result<EventEnvelope> {
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
