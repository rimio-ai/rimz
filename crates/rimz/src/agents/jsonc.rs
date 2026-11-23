//! Comment-tolerant JSON parsing for read-only upstream configuration probes.
//!
//! Every read-only probe of a user-editable upstream JSON file parses here. An
//! install flow that rewrites the file keeps strict parsing, so comments are
//! never silently discarded.

use serde::de::DeserializeOwned;

/// Parses `bytes` after one leading UTF-8 byte order mark, which some editors write and no
/// valid JSON begins with.
pub(super) fn from_slice<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, serde_json::Error> {
    let bytes = bytes.strip_prefix("\u{feff}".as_bytes()).unwrap_or(bytes);
    serde_json::from_slice(&normalize(bytes))
}

fn normalize(bytes: &[u8]) -> Vec<u8> {
    #[derive(Clone, Copy)]
    enum State {
        Normal,
        String { escaped: bool },
        LineComment,
        BlockComment { start: usize },
    }

    let mut normalized = bytes.to_vec();
    let mut state = State::Normal;
    let mut last_significant = None;
    let mut index = 0;

    while index < bytes.len() {
        match state {
            State::Normal => match bytes[index] {
                b'"' => {
                    last_significant = Some(index);
                    state = State::String { escaped: false };
                    index += 1;
                }
                b'/' if bytes.get(index + 1) == Some(&b'/') => {
                    normalized[index] = b' ';
                    normalized[index + 1] = b' ';
                    state = State::LineComment;
                    index += 2;
                }
                b'/' if bytes.get(index + 1) == Some(&b'*') => {
                    normalized[index] = b' ';
                    normalized[index + 1] = b' ';
                    state = State::BlockComment { start: index };
                    index += 2;
                }
                b'}' | b']' => {
                    if let Some(comma) = last_significant.filter(|&last| bytes[last] == b',') {
                        normalized[comma] = b' ';
                    }
                    last_significant = Some(index);
                    index += 1;
                }
                byte if byte.is_ascii_whitespace() => index += 1,
                _ => {
                    last_significant = Some(index);
                    index += 1;
                }
            },
            State::String { escaped } => {
                if escaped {
                    state = State::String { escaped: false };
                } else if bytes[index] == b'\\' {
                    state = State::String { escaped: true };
                } else if bytes[index] == b'"' {
                    state = State::Normal;
                }
                index += 1;
            }
            State::LineComment => {
                if matches!(bytes[index], b'\n' | b'\r') {
                    state = State::Normal;
                } else {
                    normalized[index] = b' ';
                }
                index += 1;
            }
            State::BlockComment { .. } => {
                if bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/') {
                    normalized[index] = b' ';
                    normalized[index + 1] = b' ';
                    state = State::Normal;
                    index += 2;
                } else {
                    if !matches!(bytes[index], b'\n' | b'\r') {
                        normalized[index] = b' ';
                    }
                    index += 1;
                }
            }
        }
    }

    if let State::BlockComment { start } = state {
        // Leave the opener visible so an unterminated comment remains invalid JSON.
        normalized[start] = b'/';
        normalized[start + 1] = b'*';
    }

    normalized
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn parses_comments_and_trailing_commas_without_touching_strings() {
        let input = br#"// leading
        {
          "url": "https://example.com/a/*literal*/", /* between fields */
          "quote": "escaped: \" // literal",
          "items": [1, 2, // final item
          ],
        } // trailing"#;

        let value: serde_json::Value = from_slice(input).unwrap();

        assert_eq!(
            value,
            json!({
                "url": "https://example.com/a/*literal*/",
                "quote": "escaped: \" // literal",
                "items": [1, 2],
            })
        );
        assert_eq!(normalize(input).len(), input.len());
    }

    #[test]
    fn strict_json_passes_through_byte_identical() {
        let input = br#"{"nested":[true,false],"text":"comma, slash // star /*"}"#;

        assert_eq!(normalize(input), input);
        assert!(from_slice::<serde_json::Value>(input).is_ok());
    }

    #[test]
    fn scans_non_utf8_comment_bytes_without_lossy_conversion() {
        let input = [b'{', b'/', b'/', 0xff, b'\n', b'}'];

        assert_eq!(from_slice::<serde_json::Value>(&input).unwrap(), json!({}));
    }

    #[test]
    fn one_leading_bom_is_skipped_and_malformed_json_behind_it_still_errors() {
        let value: serde_json::Value = from_slice("\u{feff}{\"a\": 1, // c\n}".as_bytes()).unwrap();
        assert_eq!(value, json!({"a": 1}));
        assert!(from_slice::<serde_json::Value>("\u{feff}{\"a\": [1,,]}".as_bytes()).is_err());
        assert!(from_slice::<serde_json::Value>("\u{feff}\u{feff}{}".as_bytes()).is_err());
    }

    #[test]
    fn malformed_json_and_unterminated_comments_still_error() {
        assert!(from_slice::<serde_json::Value>(br#"{"value": [1,,]}"#).is_err());
        assert!(from_slice::<serde_json::Value>(br#"{"value": 1} /* open"#).is_err());
        assert!(from_slice::<serde_json::Value>(br#"{"value": 1} /*"#).is_err());
    }
}
