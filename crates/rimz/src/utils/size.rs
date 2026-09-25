//! Human byte sizes shared by configuration and command-line parsing.

pub fn decimal_bytes(bytes: u64) -> String {
    for (factor, unit) in [(1_000_000_000_u64, "GB"), (1_000_000, "MB"), (1_000, "KB")] {
        if bytes >= factor {
            let amount = format!("{:.1}", bytes as f64 / factor as f64);
            return format!("{} {unit}", amount.strip_suffix(".0").unwrap_or(&amount));
        }
    }
    format!("{bytes} B")
}

pub fn parse_byte_size(raw: &str) -> std::result::Result<u64, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("size is empty".to_owned());
    }
    let (digits, suffix) = split_suffix(trimmed);
    let n: u64 = digits
        .parse()
        .map_err(|err| format!("size `{raw}` is not an integer: {err}"))?;
    let factor: u64 = match suffix {
        "" | "B" => 1,
        "K" | "KB" => 1_000,
        "KiB" => 1_024,
        "M" | "MB" => 1_000_000,
        "MiB" => 1_024 * 1_024,
        "G" | "GB" => 1_000_000_000,
        "GiB" => 1_024 * 1_024 * 1_024,
        other => {
            return Err(format!(
                "unknown size unit `{other}`; use B/KB/KiB/MB/MiB/GB/GiB"
            ));
        }
    };
    n.checked_mul(factor)
        .ok_or_else(|| format!("size `{raw}` overflows u64"))
}

fn split_suffix(raw: &str) -> (&str, &str) {
    let cut = raw
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit())
        .map(|(i, _)| i)
        .unwrap_or(raw.len());
    (&raw[..cut], &raw[cut..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_size_parses_units() {
        assert_eq!(parse_byte_size("0").unwrap(), 0);
        assert_eq!(parse_byte_size("512").unwrap(), 512);
        assert_eq!(parse_byte_size("1KB").unwrap(), 1_000);
        assert_eq!(parse_byte_size("1KiB").unwrap(), 1_024);
        assert_eq!(parse_byte_size("64MiB").unwrap(), 64 * 1024 * 1024);
        assert!(parse_byte_size("").is_err());
        assert!(parse_byte_size("3PB").is_err());
    }
}
