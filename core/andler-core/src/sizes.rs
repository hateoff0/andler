//! Byte-size parsing and formatting shared by the CLI and config key-path
//! table. Hoisted from `apps/cli/src/helpers.rs` so the daemon's `config
//! set` accepts exactly the same size strings the CLI does.

/// Parses a human size string (`512000`, `8G`, `1.5GB` is rejected, `64GiB`)
/// into a byte count.
pub fn parse_size(input: &str) -> Result<u64, String> {
    let input = input.trim().to_uppercase().replace(' ', "");
    if input.is_empty() {
        return Err("empty size string".to_string());
    }

    let split = input
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(input.len());

    let number_part = &input[..split];
    let unit_part = &input[split..];

    if number_part.is_empty() {
        return Err(format!("missing number before `{unit_part}`"));
    }
    if unit_part.starts_with('.') {
        return Err("decimal sizes not supported (use e.g. 64GB, not 1.5GB)".to_string());
    }

    let number: u64 = number_part
        .parse()
        .map_err(|e| format!("invalid number `{number_part}`: {e}"))?;

    let bytes = match unit_part {
        "" => number,
        "B" => number,
        "KB" | "KIB" | "K" => number
            .checked_mul(1024)
            .ok_or_else(|| format!("size too large: {input}"))?,
        "MB" | "MIB" | "M" => number
            .checked_mul(1024 * 1024)
            .ok_or_else(|| format!("size too large: {input}"))?,
        "GB" | "GIB" | "G" => number
            .checked_mul(1024 * 1024 * 1024)
            .ok_or_else(|| format!("size too large: {input}"))?,
        "TB" | "TIB" | "T" => number
            .checked_mul(1024 * 1024 * 1024 * 1024)
            .ok_or_else(|| format!("size too large: {input}"))?,
        _ => {
            return Err(format!(
                "unknown size unit `{unit_part}` (use B/KB/MB/GB/TB or KiB/MiB/GiB/TiB)"
            ))
        }
    };

    Ok(bytes)
}

/// Formats a byte count the way the CLI does: exact unit when the count is a
/// clean multiple, one decimal otherwise.
pub fn format_size(bytes: u64) -> String {
    const TIB: u64 = 1024 * 1024 * 1024 * 1024;
    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;
    const KIB: u64 = 1024;

    if bytes > 0 && bytes.is_multiple_of(TIB) {
        format!("{} TiB", bytes / TIB)
    } else if bytes > 0 && bytes.is_multiple_of(GIB) {
        format!("{} GiB", bytes / GIB)
    } else if bytes > 0 && bytes.is_multiple_of(MIB) {
        format!("{} MiB", bytes / MIB)
    } else if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_size_plain_bytes() {
        assert_eq!(parse_size("512000").unwrap(), 512000);
    }

    #[test]
    fn parse_size_case_and_space_insensitive() {
        assert_eq!(parse_size("8g").unwrap(), 8 * 1024 * 1024 * 1024);
        assert_eq!(parse_size(" 2 GB ").unwrap(), 2 * 1024 * 1024 * 1024);
    }

    #[test]
    fn parse_size_units() {
        assert_eq!(parse_size("1K").unwrap(), 1024);
        assert_eq!(parse_size("2MIB").unwrap(), 2 * 1024 * 1024);
        assert_eq!(parse_size("3TB").unwrap(), 3 * 1024 * 1024 * 1024 * 1024);
        assert_eq!(parse_size("64GiB").unwrap(), 64 * 1024 * 1024 * 1024);
    }

    #[test]
    fn parse_size_rejects_decimal_and_garbage() {
        assert!(parse_size("1.5GB").is_err());
        assert!(parse_size("abc").is_err());
        assert!(parse_size("").is_err());
        assert!(parse_size("12XB").is_err());
    }

    #[test]
    fn format_size_exact_multiples_are_integer() {
        assert_eq!(format_size(8 * 1024 * 1024 * 1024), "8 GiB");
        assert_eq!(format_size(1500 * 1024 * 1024), "1500 MiB");
    }

    #[test]
    fn format_size_fractional_falls_back_to_one_decimal() {
        assert_eq!(format_size(2_000_000_000), "1.9 GiB");
        assert_eq!(format_size(512), "512 B");
        // Sub-MiB counts hit the KiB fallback branch (no exact-multiple
        // branch for KiB, matching the historical CLI behavior).
        assert_eq!(format_size(1024), "1.0 KiB");
    }
}
