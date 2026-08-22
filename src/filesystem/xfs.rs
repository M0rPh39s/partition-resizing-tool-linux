//! XFS driver. XFS can only *grow*, and only while mounted.

use crate::errors::{Error, Result};
use crate::filesystem::FsProbe;
use crate::util::cmd::{require, run_raw};

/// Probe XFS. Requires either a mount point or a temporary mount.
pub fn probe(device: &str, mountpoint: Option<&str>) -> Result<FsProbe> {
    require(&["xfs_info"])?;
    let (mp, temp) = match mountpoint {
        Some(mp) => (mp.to_string(), None),
        None => {
            let dir = crate::util::tempdir("partresize-xfs")?;
            crate::util::cmd::run_raw(&["mount", device, &dir], None)?;
            (dir.clone(), Some(dir))
        }
    };
    let result = (|| {
        let out = run_raw(&["xfs_info", &mp], None)?;
        let mut bsize = 4096u64;
        let mut blocks = 0u64;
        for tok in out.stdout.split_whitespace() {
            if let Some(v) = tok.strip_prefix("bsize=") {
                bsize = v.trim_end_matches(',').parse().unwrap_or(4096);
            }
            if let Some(v) = tok.strip_prefix("blocks=") {
                if let Ok(b) = v.trim_end_matches(',').parse::<u64>() {
                    if b > 0 {
                        blocks = b;
                    }
                }
            }
        }
        let size = bsize * blocks;
        Ok(FsProbe {
            size_bytes: size,
            used_bytes: size,
            block_size: bsize,
            min_size_bytes: size,
        })
    })();
    if let Some(dir) = temp {
        let _ = run_raw(&["umount", &dir], None);
        let _ = std::fs::remove_dir(&dir);
    }
    result
}

pub fn check(device: &str, dry_run: bool) -> Result<()> {
    require(&["xfs_repair"])?;
    if dry_run {
        eprintln!("[dry-run] xfs_repair -n {device}");
        return Ok(());
    }
    let out = run_raw(&["xfs_repair", "-n", device], None)?;
    if !out.ok() {
        return Err(Error::Execution(format!(
            "xfs_repair reported problems on {device}"
        )));
    }
    Ok(())
}

pub fn grow(mountpoint: &str, dry_run: bool) -> Result<()> {
    require(&["xfs_growfs"])?;
    if dry_run {
        eprintln!("[dry-run] xfs_growfs {mountpoint}");
        return Ok(());
    }
    crate::util::cmd::run_checked(&["xfs_growfs", mountpoint], None)?;
    Ok(())
}
