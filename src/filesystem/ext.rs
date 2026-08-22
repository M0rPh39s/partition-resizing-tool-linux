//! ext2/3/4 driver built on e2fsprogs (dumpe2fs, e2fsck, resize2fs).

use crate::errors::{Error, Result};
use crate::filesystem::FsProbe;
use crate::util::cmd::{have, require, run_owned, run_raw};

fn dumpe2fs(device: &str) -> Result<std::collections::HashMap<String, String>> {
    require(&["dumpe2fs"])?;
    let out = run_raw(&["dumpe2fs", "-h", device], None)?;
    if out.stdout.is_empty() {
        return Err(Error::Execution(format!(
            "dumpe2fs produced no output for {device}"
        )));
    }
    let mut map = std::collections::HashMap::new();
    for line in out.stdout.lines() {
        if let Some((k, v)) = line.split_once(':') {
            map.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    Ok(map)
}

fn device_bytes(device: &str) -> u64 {
    if have("blockdev") {
        if let Ok(out) = run_raw(&["blockdev", "--getsize64", device], None) {
            if out.ok() {
                if let Ok(v) = out.stdout.trim().parse() {
                    return v;
                }
            }
        }
    }
    0
}

pub fn probe(device: &str) -> Result<FsProbe> {
    let info = dumpe2fs(device)?;
    let block_size: u64 = info
        .get("Block size")
        .and_then(|s| s.parse().ok())
        .unwrap_or(4096);
    let block_count: u64 = info
        .get("Block count")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let free_blocks: u64 = info
        .get("Free blocks")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let size = block_count * block_size;
    let used = block_count.saturating_sub(free_blocks) * block_size;
    let min = minimum_size(device, block_size);
    Ok(FsProbe {
        size_bytes: size,
        used_bytes: used,
        block_size,
        min_size_bytes: min,
    })
}

/// Smallest size resize2fs believes the filesystem can shrink to (bytes).
pub fn minimum_size(device: &str, block_size: u64) -> u64 {
    if !have("resize2fs") {
        return 0;
    }
    if let Ok(out) = run_raw(&["resize2fs", "-P", device], None) {
        for line in out.stdout.lines() {
            if line.to_ascii_lowercase().contains("minimum size") {
                if let Some((_, num)) = line.rsplit_once(':') {
                    if let Ok(blocks) = num.trim().parse::<u64>() {
                        return blocks * block_size;
                    }
                }
            }
        }
    }
    0
}

pub fn check(device: &str, dry_run: bool) -> Result<()> {
    require(&["e2fsck"])?;
    if dry_run {
        eprintln!("[dry-run] e2fsck -f -y {device}");
        return Ok(());
    }
    // -f force, -y assume yes. rc 0=clean, 1/2=fixed, >=4 uncorrected.
    let out = run_owned(
        &["e2fsck", "-f", "-y", device]
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>(),
        None,
    )?;
    match out.status {
        Some(c) if c >= 4 => Err(Error::Execution(format!(
            "e2fsck reported uncorrected errors on {device} (rc={c})\n{}",
            out.stdout
        ))),
        _ => Ok(()),
    }
}

fn resize(device: &str, target: Option<u64>, block_size: u64, dry_run: bool) -> Result<()> {
    require(&["resize2fs"])?;
    let mut args: Vec<String> = vec!["resize2fs".into(), device.into()];
    if let Some(bytes) = target {
        // If the target meets/exceeds device capacity, omit the size so
        // resize2fs fills exactly (an explicit count == capacity is rejected).
        let dev = if dry_run { 0 } else { device_bytes(device) };
        if !(dev != 0 && bytes >= dev) {
            args.push((bytes / block_size).to_string());
        }
    }
    if dry_run {
        eprintln!("[dry-run] {}", args.join(" "));
        return Ok(());
    }
    let out = run_owned(&args, None)?;
    if !out.ok() {
        return Err(Error::Command {
            program: args.join(" "),
            code: out.status,
            stderr: if out.stderr.trim().is_empty() {
                out.stdout
            } else {
                out.stderr
            },
        });
    }
    Ok(())
}

pub fn grow(device: &str, target: Option<u64>, dry_run: bool) -> Result<()> {
    let bs = if dry_run {
        4096
    } else {
        probe(device)?.block_size
    };
    resize(device, target, bs, dry_run)
}

pub fn shrink(device: &str, target: u64, dry_run: bool) -> Result<()> {
    // Always fsck first, then verify against the reported minimum.
    check(device, dry_run)?;
    if dry_run {
        eprintln!("[dry-run] resize2fs {device} {}K", target / 1024);
        return Ok(());
    }
    let p = probe(device)?;
    if p.min_size_bytes != 0 && target < p.min_size_bytes {
        return Err(Error::Safety(format!(
            "target {} is below the ext filesystem minimum of {} bytes",
            target, p.min_size_bytes
        )));
    }
    resize(device, Some(target), p.block_size, false)
}
