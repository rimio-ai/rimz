//! Content-Length framing and the read-only LSP request vocabulary.

use std::io::{BufRead, Read, Write};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::Result;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Location {
    pub uri: String,
    pub range: Range,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolInformation {
    pub name: String,
    pub kind: u32,
    pub location: Location,
    pub container_name: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DocumentSymbol {
    pub name: String,
    pub kind: u32,
    pub range: Range,
    pub selection_range: Range,
    #[serde(default)]
    pub children: Vec<DocumentSymbol>,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum Symbols {
    Flat(Vec<SymbolInformation>),
    Tree(Vec<DocumentSymbol>),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CallHierarchyItem {
    pub name: String,
    pub kind: u32,
    pub uri: String,
    pub range: Range,
    pub selection_range: Range,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// The broker's last frame on an editor connection after a terminal stop; attach consumes it.
pub(super) const STOPPED: &str = "rimz/stopped";

pub fn read_frame(reader: &mut impl BufRead) -> Result<Value> {
    let mut length = None;
    let mut header_bytes = 0;
    loop {
        let mut line = String::new();
        let read = reader.take(8193).read_line(&mut line)?;
        header_bytes += read;
        if read == 0 || header_bytes > 8192 {
            return Err(super::LspErr::Protocol(
                "missing or oversized header".into(),
            ));
        }
        if line == "\r\n" {
            break;
        }
        let (name, value) = line
            .trim_end()
            .split_once(':')
            .ok_or_else(|| super::LspErr::Protocol("invalid header".into()))?;
        if name.eq_ignore_ascii_case("Content-Length") {
            if length.is_some() {
                return Err(super::LspErr::Protocol("duplicate Content-Length".into()));
            }
            length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| super::LspErr::Protocol("invalid Content-Length".into()))?,
            );
        }
    }
    let length = length
        .filter(|length| *length <= 64 * 1024 * 1024)
        .ok_or_else(|| super::LspErr::Protocol("missing or oversized Content-Length".into()))?;
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub fn write_frame(writer: &mut impl Write, value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    write!(writer, "Content-Length: {}\r\n\r\n", bytes.len())?;
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn content_length_round_trip_and_rejects_unbounded_frames() {
        let value = json!({"id": 1, "result": "λ"});
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &value).unwrap();
        assert_eq!(read_frame(&mut bytes.as_slice()).unwrap(), value);
        assert!(read_frame(&mut b"Content-Length: 999999999\r\n\r\n".as_slice()).is_err());
    }
}
