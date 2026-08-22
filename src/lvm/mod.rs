//! LVM layer operations (PV / VG / LV) and metadata backup.

use crate::errors::{Error, Result};
use crate::util::cmd::{have, require, run_raw};

/// Look up (pv_device, vg_name) for a given LV device, best effort.
pub fn pv_and_vg(lv_device: &str) -> (Option<String>, Option<String>) {
    if !have("lvs") {
        return (None, None);
    }
    let vg = run_raw(&["lvs", "--noheadings", "-o", "vg_name", lv_device], None)
        .ok()
        .filter(|o| o.ok())
        .map(|o| o.stdout.trim().to_string())
        .filter(|s| !s.is_empty());

    let pv = vg.as_ref().and_then(|vg| {
        run_raw(
            &[
                "pvs",
                "--noheadings",
                "-o",
                "pv_name",
                "-S",
                &format!("vg_name={vg}"),
            ],
            None,
        )
        .ok()
        .filter(|o| o.ok())
        .and_then(|o| o.stdout.trim().lines().next().map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
    });
    (pv, vg)
}

pub fn pv_grow(pv_device: &str, dry_run: bool) -> Result<()> {
    require(&["pvresize"])?;
    if dry_run {
        eprintln!("[dry-run] pvresize {pv_device}");
        return Ok(());
    }
    crate::util::cmd::run_checked(&["pvresize", pv_device], None)?;
    Ok(())
}

pub fn pv_shrink(pv_device: &str, target_bytes: u64, dry_run: bool) -> Result<()> {
    require(&["pvresize"])?;
    let size = format!("{target_bytes}b");
    if dry_run {
        eprintln!("[dry-run] pvresize --setphysicalvolumesize {size} {pv_device}");
        return Ok(());
    }
    crate::util::cmd::run_checked(
        &["pvresize", "--setphysicalvolumesize", &size, pv_device],
        None,
    )?;
    Ok(())
}

pub fn lv_grow(lv_device: &str, target_bytes: u64, dry_run: bool) -> Result<()> {
    require(&["lvextend"])?;
    let size = format!("{target_bytes}b");
    if dry_run {
        eprintln!("[dry-run] lvextend -L {size} {lv_device}");
        return Ok(());
    }
    // Do not use --resizefs; the filesystem is handled explicitly by the planner.
    let out = run_raw(&["lvextend", "-L", &size, lv_device], None)?;
    if !out.ok() && !out.stderr.contains("matches existing size") {
        return Err(Error::Command {
            program: format!("lvextend -L {size} {lv_device}"),
            code: out.status,
            stderr: out.stderr,
        });
    }
    Ok(())
}

pub fn lv_shrink(lv_device: &str, target_bytes: u64, dry_run: bool) -> Result<()> {
    require(&["lvreduce"])?;
    let size = format!("{target_bytes}b");
    if dry_run {
        eprintln!("[dry-run] lvreduce -f -L {size} {lv_device}");
        return Ok(());
    }
    // -f because the filesystem has already been shrunk by the planner's order.
    crate::util::cmd::run_checked(&["lvreduce", "-f", "-L", &size, lv_device], None)?;
    Ok(())
}

pub fn vg_backup(vg_name: &str, dest: &str, dry_run: bool) -> Result<()> {
    if !have("vgcfgbackup") {
        return Ok(());
    }
    if dry_run {
        eprintln!("[dry-run] vgcfgbackup -f {dest} {vg_name}");
        return Ok(());
    }
    let _ = run_raw(&["vgcfgbackup", "-f", dest, vg_name], None);
    Ok(())
}
