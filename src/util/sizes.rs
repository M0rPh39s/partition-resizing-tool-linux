//! Parsing and formatting of byte sizes (decimal and binary units).

use crate::errors::{Error, Result};

/// Parse a human size string into a byte count.
///
/// Accepts binary (KiB/MiB/GiB/TiB, 1024-based) and decimal (KB/MB/GB/TB,
/// 1000-based) suffixes. A bare number is bytes. Following disk-tool
/// convention an unsuffixed `K`/`M`/`G`/`T` means the binary unit.
pub fn parse_size(text: &str) -> Result<u64> {
    let t = text.trim();
    if t.is_empty() {
        return Err(Error::InvalidTarget("empty size".into()));
    }
    let split = t.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(t.len());
    let (num, unit) = t.split_at(split);
    let value: f64 = num
        .trim()
        .parse()
        .map_err(|_| Error::InvalidTarget(format!("invalid size number: {text:?}")))?;
    if value < 0.0 {
        return Err(Error::InvalidTarget(format!("negative size: {text:?}")));
    }
    let mult: f64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "k" | "kib" => 1024.0,
        "kb" => 1000.0,
        "m" | "mib" => 1024f64.powi(2),
        "mb" => 1000f64.powi(2),
        "g" | "gib" => 1024f64.powi(3),
        "gb" => 1000f64.powi(3),
        "t" | "tib" => 1024f64.powi(4),
        "tb" => 1000f64.powi(4),
        "p" | "pib" => 1024f64.powi(5),
        "pb" => 1000f64.powi(5),
        other => {
            return Err(Error::InvalidTarget(format!(
                "unknown size unit {other:?} in {text:?}"
            )))
        }
    };
    Ok((value * mult) as u64)
}

/// Format a byte count into a binary (GiB) human string.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = bytes as f64;
    let mut idx = 0;
    while value >= 1024.0 && idx < UNITS.len() - 1 {
        value /= 1024.0;
        idx += 1;
    }
    if idx == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[idx])
    }
}

/// Round `value` down to a multiple of `granularity`.
pub fn round_down(value: u64, granularity: u64) -> u64 {
    if granularity == 0 {
        return value;
    }
    (value / granularity) * granularity
}

/// Round `value` up to a multiple of `granularity`.
pub fn round_up(value: u64, granularity: u64) -> u64 {
    if granularity == 0 {
        return value;
    }
    value.div_ceil(granularity) * granularity
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_binary_and_decimal() {
        assert_eq!(parse_size("80G").unwrap(), 80 * 1024u64.pow(3));
        assert_eq!(parse_size("80GiB").unwrap(), 80 * 1024u64.pow(3));
        assert_eq!(parse_size("1000MB").unwrap(), 1000 * 1000u64.pow(2));
        assert_eq!(parse_size("512").unwrap(), 512);
        assert_eq!(parse_size("300GiB").unwrap(), 300 * 1024u64.pow(3));
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse_size("banana").is_err());
        assert!(parse_size("10ZB").is_err());
        assert!(parse_size("").is_err());
    }

    #[test]
    fn format_roundtrip() {
        assert_eq!(format_size(80 * 1024u64.pow(3)), "80.00 GiB");
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1024), "1.00 KiB");
    }

    #[test]
    fn rounding() {
        assert_eq!(round_down(1000, 256), 768);
        assert_eq!(round_up(1000, 256), 1024);
        assert_eq!(round_up(1024, 256), 1024);
    }
}
