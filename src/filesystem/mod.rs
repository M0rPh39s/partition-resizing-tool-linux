//! Filesystem capability matrix and per-filesystem resize drivers.
//!
//! The matrix is deliberately conservative. The cardinal rules from the spec:
//!
//! * ext4 grows online, but only shrinks **offline**.
//! * XFS can grow (online only) and can **never** shrink.
//! * Btrfs grows and shrinks online.
//! * swap is recreated, not resized in place.

pub mod btrfs;
pub mod ext;
pub mod xfs;

use serde::Serialize;

use crate::errors::{Error, Result};

/// What a filesystem can do, mounted vs. unmounted.
#[derive(Debug, Clone, Serialize)]
pub struct Capabilities {
    pub fstype: String,
    pub online_grow: bool,
    pub online_shrink: bool,
    pub offline_grow: bool,
    pub offline_shrink: bool,
    pub notes: Vec<String>,
}

/// Measured state of a filesystem.
#[derive(Debug, Clone)]
pub struct FsProbe {
    pub size_bytes: u64,
    pub used_bytes: u64,
    pub block_size: u64,
    /// Smallest size the filesystem could shrink to, or 0 if unknown.
    pub min_size_bytes: u64,
}

pub fn capabilities_for(fstype: &str) -> Capabilities {
    let f = fstype.to_ascii_lowercase();
    let (og, os, ofg, ofs, notes): (bool, bool, bool, bool, &[&str]) = match f.as_str() {
        "ext2" | "ext3" => (true, false, true, true, &[]),
        "ext4" => (
            true,
            false,
            true,
            true,
            &["online grow supported; shrink requires the filesystem to be unmounted"],
        ),
        "btrfs" => (
            true,
            true,
            true,
            true,
            &["btrfs supports online grow and online shrink"],
        ),
        "xfs" => (
            true,
            false,
            false,
            false,
            &["xfs can only grow, and only while mounted; it cannot be shrunk"],
        ),
        "f2fs" => (true, false, true, false, &["f2fs cannot be shrunk"]),
        "ntfs" => (
            false,
            false,
            true,
            true,
            &["NTFS is resized offline via ntfsresize (best effort)"],
        ),
        "vfat" | "fat" | "fat32" | "exfat" => (
            false,
            false,
            false,
            false,
            &["FAT/exFAT resizing is not supported by this tool"],
        ),
        "swap" => (
            false,
            false,
            true,
            true,
            &["swap is not resized in place; it is turned off, remade and turned on"],
        ),
        _ => (
            false,
            false,
            false,
            false,
            &["filesystem is not supported for resizing"],
        ),
    };
    Capabilities {
        fstype: f,
        online_grow: og,
        online_shrink: os,
        offline_grow: ofg,
        offline_shrink: ofs,
        notes: notes.iter().map(|s| s.to_string()).collect(),
    }
}

/// Probe a filesystem for its current geometry.
pub fn probe(fstype: &str, device: &str, mountpoint: Option<&str>) -> Result<FsProbe> {
    match fstype.to_ascii_lowercase().as_str() {
        "ext2" | "ext3" | "ext4" => ext::probe(device),
        "xfs" => xfs::probe(device, mountpoint),
        "btrfs" => btrfs::probe(device, mountpoint),
        other => Err(Error::Unsupported(format!(
            "cannot inspect filesystem type {other:?}"
        ))),
    }
}

/// Run a filesystem consistency check (device must be unmounted).
pub fn check(fstype: &str, device: &str, dry_run: bool) -> Result<()> {
    match fstype.to_ascii_lowercase().as_str() {
        "ext2" | "ext3" | "ext4" => ext::check(device, dry_run),
        "btrfs" => btrfs::check(device, dry_run),
        "xfs" => xfs::check(device, dry_run),
        _ => Ok(()),
    }
}

/// Grow a mounted filesystem in place.
pub fn grow_online(
    fstype: &str,
    device: &str,
    mountpoint: &str,
    target: Option<u64>,
    dry_run: bool,
) -> Result<()> {
    match fstype.to_ascii_lowercase().as_str() {
        "ext2" | "ext3" | "ext4" => ext::grow(device, target, dry_run),
        "xfs" => xfs::grow(mountpoint, dry_run),
        "btrfs" => btrfs::resize_mounted(mountpoint, target, dry_run),
        other => Err(Error::Unsupported(format!(
            "{other} does not support online grow"
        ))),
    }
}

/// Grow an unmounted filesystem to fill its device (or to `target`).
pub fn grow_offline(fstype: &str, device: &str, target: Option<u64>, dry_run: bool) -> Result<()> {
    match fstype.to_ascii_lowercase().as_str() {
        "ext2" | "ext3" | "ext4" => {
            ext::check(device, dry_run)?;
            ext::grow(device, target, dry_run)
        }
        "btrfs" => btrfs::resize_offline(device, target, dry_run),
        other => Err(Error::Unsupported(format!(
            "{other} does not support offline grow through this tool"
        ))),
    }
}

/// Shrink an unmounted filesystem to `target` bytes.
pub fn shrink_offline(fstype: &str, device: &str, target: u64, dry_run: bool) -> Result<()> {
    match fstype.to_ascii_lowercase().as_str() {
        "ext2" | "ext3" | "ext4" => ext::shrink(device, target, dry_run),
        "btrfs" => btrfs::resize_offline(device, Some(target), dry_run),
        other => Err(Error::Unsupported(format!("{other} cannot be shrunk"))),
    }
}
