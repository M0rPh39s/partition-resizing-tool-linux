//! LUKS layer operations via cryptsetup.
//!
//! Growing a LUKS mapping to fill its backing device is non-destructive.
//! Shrinking sets an explicit size (in 512-byte sectors) and must happen
//! after the contained filesystem/LV has already been shrunk.

use crate::errors::Result;
use crate::util::cmd::require;

pub fn grow(mapper_device: &str, dry_run: bool) -> Result<()> {
    require(&["cryptsetup"])?;
    if dry_run {
        eprintln!("[dry-run] cryptsetup resize {mapper_device}");
        return Ok(());
    }
    crate::util::cmd::run_checked(&["cryptsetup", "resize", mapper_device], None)?;
    Ok(())
}

pub fn shrink(mapper_device: &str, target_bytes: u64, dry_run: bool) -> Result<()> {
    require(&["cryptsetup"])?;
    let sectors = (target_bytes / 512).to_string();
    if dry_run {
        eprintln!("[dry-run] cryptsetup resize --size {sectors} {mapper_device}");
        return Ok(());
    }
    crate::util::cmd::run_checked(
        &["cryptsetup", "resize", "--size", &sectors, mapper_device],
        None,
    )?;
    Ok(())
}
