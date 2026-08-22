//! Post-resize verification.
//!
//! Confirms the fundamental invariant `filesystem_size <= block_device_size`
//! after an operation, and re-reads sizes for reporting.

use crate::errors::{Error, Result};
use crate::filesystem::probe;
use crate::partition as part;
use crate::storage::Topology;
use crate::util::cmd::{have, run_raw};

/// Block device size in bytes (0 if it cannot be determined).
pub fn device_bytes(device: &str) -> u64 {
    if have("blockdev") {
        if let Ok(o) = run_raw(&["blockdev", "--getsize64", device], None) {
            if o.ok() {
                return o.stdout.trim().parse().unwrap_or(0);
            }
        }
    }
    0
}

/// Verify the filesystem is not larger than the block device that holds it.
pub fn verify_fits(topo: &Topology) -> Result<()> {
    let fs = topo.filesystem();
    let dev = device_bytes(&fs.device);
    if dev == 0 {
        return Ok(()); // cannot measure; skip rather than false-positive
    }
    let fs_size = match probe(topo.fstype(), &fs.device, topo.mountpoint.as_deref()) {
        Ok(p) => p.size_bytes,
        Err(_) => fs.size,
    };
    if fs_size > dev {
        return Err(Error::Execution(format!(
            "INVARIANT VIOLATION: filesystem ({}) is larger than its device ({}) on {}",
            crate::util::sizes::format_size(fs_size),
            crate::util::sizes::format_size(dev),
            fs.device
        )));
    }
    Ok(())
}

/// Re-read and report the final partition + filesystem sizes.
pub fn report_final(topo: &Topology) -> Result<String> {
    let mut out = String::new();
    if let Some(p) = topo.partition() {
        if let Ok(table) = part::read_table(&topo.disk().device) {
            if let Some(tp) = table.by_node(&p.device) {
                out.push_str(&format!(
                    "Partition {}: {}\n",
                    p.device,
                    crate::util::sizes::format_size(tp.size * table.sector_size)
                ));
            }
        }
    }
    if let Ok(fsp) = probe(
        topo.fstype(),
        topo.fs_backing_device(),
        topo.mountpoint.as_deref(),
    ) {
        out.push_str(&format!(
            "Filesystem {}: {}\n",
            topo.fs_backing_device(),
            crate::util::sizes::format_size(fsp.size_bytes)
        ));
    }
    Ok(out)
}
