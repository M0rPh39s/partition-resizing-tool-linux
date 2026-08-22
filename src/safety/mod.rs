//! Safety guards: size sanity, privilege checks, and a test-mode device guard.
//!
//! In test mode (`PARTRESIZE_TEST_MODE=1`) every destructive operation must
//! target a loop / device-mapper test device; any attempt to touch a real host
//! disk (`/dev/sda`, `/dev/vda`, `/dev/nvme*`, ...) is refused, so the
//! integration tests can never damage the machine they run on.

use crate::errors::{Error, Result};

pub const DEFAULT_SHRINK_MARGIN_PCT: u64 = 10;

fn min_absolute_margin() -> u64 {
    // 256 MiB by default; overridable so tests can shrink small images tightly.
    std::env::var("PARTRESIZE_MIN_MARGIN")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(256 * 1024 * 1024)
}

pub fn test_mode() -> bool {
    std::env::var("PARTRESIZE_TEST_MODE").as_deref() == Ok("1")
}

pub fn is_root() -> bool {
    // Avoid unsafe libc; read the real UID from /proc/self/status.
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("Uid:") {
                if let Some(first) = rest.split_whitespace().next() {
                    return first == "0";
                }
            }
        }
    }
    std::env::var("USER").as_deref() == Ok("root")
}

fn base_device(device: &str) -> String {
    // Map a partition node to its parent disk (string-only, best effort).
    for (prefix, needs_p) in [
        ("/dev/nvme", true),
        ("/dev/mmcblk", true),
        ("/dev/loop", true),
    ] {
        if device.starts_with(prefix) {
            if needs_p {
                if let Some(idx) = device.rfind('p') {
                    // only treat trailing pN as a partition suffix
                    if device[idx + 1..].chars().all(|c| c.is_ascii_digit())
                        && !device[idx + 1..].is_empty()
                    {
                        return device[..idx].to_string();
                    }
                }
            }
            return device.to_string();
        }
    }
    for prefix in ["/dev/sd", "/dev/vd", "/dev/hd"] {
        if device.starts_with(prefix) {
            let trimmed = device.trim_end_matches(|c: char| c.is_ascii_digit());
            return trimmed.to_string();
        }
    }
    device.to_string()
}

fn is_real_disk(device: &str) -> bool {
    let base = base_device(device);
    for d in [device, base.as_str()] {
        for prefix in ["/dev/sd", "/dev/vd", "/dev/hd", "/dev/nvme", "/dev/mmcblk"] {
            if d.starts_with(prefix) {
                return true;
            }
        }
    }
    false
}

fn is_test_device(device: &str) -> bool {
    device.starts_with("/dev/loop")
        || device.starts_with("/dev/dm-")
        || device.starts_with("/dev/mapper/partresize")
}

/// In test mode, reject anything that is not an explicit test device.
pub fn assert_test_device(device: &str) -> Result<()> {
    if !test_mode() {
        return Ok(());
    }
    if is_real_disk(device) {
        return Err(Error::Safety(format!(
            "refusing to operate on real host device {device:?} in test mode"
        )));
    }
    if !is_test_device(device) {
        return Err(Error::Safety(format!(
            "device {device:?} is not a recognised disposable test device"
        )));
    }
    Ok(())
}

pub fn shrink_margin(used: u64) -> u64 {
    (used * DEFAULT_SHRINK_MARGIN_PCT / 100).max(min_absolute_margin())
}

/// Verify a shrink target leaves enough room above the used data.
pub fn assert_shrink_safe(target_fs: u64, used: u64, fs_min: u64) -> Result<()> {
    if target_fs < used {
        return Err(Error::Safety(format!(
            "target size {target_fs} is smaller than used data {used}; this would destroy data"
        )));
    }
    let floor = used + shrink_margin(used);
    if target_fs < floor {
        return Err(Error::Safety(format!(
            "target size {target_fs} leaves less than the required safety margin \
             above used data ({floor} needed)"
        )));
    }
    if fs_min != 0 && target_fs < fs_min {
        return Err(Error::Safety(format!(
            "target size {target_fs} is below the filesystem minimum of {fs_min} bytes"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_device_maps_partitions() {
        assert_eq!(base_device("/dev/nvme0n1p3"), "/dev/nvme0n1");
        assert_eq!(base_device("/dev/sda2"), "/dev/sda");
        assert_eq!(base_device("/dev/loop0p1"), "/dev/loop0");
    }

    #[test]
    fn test_mode_rejects_real_and_allows_loop() {
        std::env::set_var("PARTRESIZE_TEST_MODE", "1");
        assert!(assert_test_device("/dev/sda1").is_err());
        assert!(assert_test_device("/dev/nvme0n1p3").is_err());
        assert!(assert_test_device("/dev/loop0p1").is_ok());
        assert!(assert_test_device("/dev/mapper/partresize-x").is_ok());
        std::env::remove_var("PARTRESIZE_TEST_MODE");
    }

    #[test]
    fn shrink_guard() {
        assert!(assert_shrink_safe(50, 100, 0).is_err());
        std::env::set_var("PARTRESIZE_MIN_MARGIN", "16777216");
        assert!(assert_shrink_safe(1_000_000_000, 100_000_000, 0).is_ok());
        std::env::remove_var("PARTRESIZE_MIN_MARGIN");
    }
}
