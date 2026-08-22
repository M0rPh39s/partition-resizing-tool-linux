//! Btrfs driver. Btrfs resizes (grow and shrink) on a *mounted* filesystem.
//!
//! For offline work the maintenance environment mounts the filesystem at a
//! scratch mount point (never as the system root) and resizes it there.

use crate::errors::{Error, Result};
use crate::filesystem::FsProbe;
use crate::util::cmd::{require, run_raw};
use crate::util::tempdir;

pub fn probe(device: &str, mountpoint: Option<&str>) -> Result<FsProbe> {
    require(&["btrfs"])?;
    let (mp, temp) = match mountpoint {
        Some(mp) => (mp.to_string(), None),
        None => {
            let dir = tempdir("partresize-btrfs")?;
            run_raw(&["mount", "-o", "ro", device, &dir], None)?;
            (dir.clone(), Some(dir))
        }
    };
    let result = (|| {
        let out = run_raw(&["btrfs", "filesystem", "usage", "-b", &mp], None)?;
        let mut size = 0u64;
        let mut used = 0u64;
        for line in out.stdout.lines() {
            let s = line.trim();
            if let Some(v) = s.strip_prefix("Device size:") {
                size = v.trim().parse().unwrap_or(0);
            } else if let Some(v) = s.strip_prefix("Used:") {
                used = v.trim().parse().unwrap_or(0);
            }
        }
        Ok(FsProbe {
            size_bytes: size,
            used_bytes: used,
            block_size: 4096,
            min_size_bytes: used,
        })
    })();
    if let Some(dir) = temp {
        let _ = run_raw(&["umount", &dir], None);
        let _ = std::fs::remove_dir(&dir);
    }
    result
}

pub fn check(device: &str, dry_run: bool) -> Result<()> {
    require(&["btrfs"])?;
    if dry_run {
        eprintln!("[dry-run] btrfs check {device}");
        return Ok(());
    }
    let out = run_raw(&["btrfs", "check", device], None)?;
    if !out.ok() {
        return Err(Error::Execution(format!(
            "btrfs check failed on {device}\n{}",
            out.stdout
        )));
    }
    Ok(())
}

pub fn resize_mounted(mountpoint: &str, target: Option<u64>, dry_run: bool) -> Result<()> {
    require(&["btrfs"])?;
    let size = match target {
        Some(t) => t.to_string(),
        None => "max".to_string(),
    };
    if dry_run {
        eprintln!("[dry-run] btrfs filesystem resize {size} {mountpoint}");
        return Ok(());
    }
    crate::util::cmd::run_checked(&["btrfs", "filesystem", "resize", &size, mountpoint], None)?;
    Ok(())
}

pub fn resize_offline(device: &str, target: Option<u64>, dry_run: bool) -> Result<()> {
    if dry_run {
        let size = target
            .map(|t| t.to_string())
            .unwrap_or_else(|| "max".into());
        eprintln!("[dry-run] mount {device}; btrfs filesystem resize {size}; umount");
        return Ok(());
    }
    let dir = tempdir("partresize-btrfs")?;
    let r = (|| {
        run_raw(&["mount", device, &dir], None)?;
        resize_mounted(&dir, target, false)
    })();
    let _ = run_raw(&["umount", &dir], None);
    let _ = std::fs::remove_dir(&dir);
    r
}
