//! Partition-table geometry and resize operations.
//!
//! Reading uses `sfdisk -J` (stable JSON, sector units). Writing a new size
//! uses `sfdisk -N` which rewrites a single partition entry keeping its start
//! offset — we never move a partition's start as a side effect of a resize.

use serde::Deserialize;

use crate::errors::{Error, Result};
use crate::util::cmd::{have, require, run_owned, run_raw};

pub const ALIGN: u64 = 1024 * 1024; // 1 MiB

#[derive(Debug, Clone)]
pub struct Part {
    pub number: u32,
    pub node: String,
    pub start: u64, // first sector
    pub size: u64,  // sectors
    /// GPT type GUID / MBR type code (kept for reporting and future use).
    #[allow(dead_code)]
    pub ptype: String,
}

impl Part {
    pub fn end(&self) -> u64 {
        self.start + self.size - 1
    }
}

#[derive(Debug, Clone)]
pub struct Table {
    /// Disk device the table was read from (kept for reporting).
    #[allow(dead_code)]
    pub disk: String,
    /// Partition table type: "gpt" | "dos" (kept for reporting).
    #[allow(dead_code)]
    pub label: String,
    pub sector_size: u64,
    pub total_sectors: u64,
    pub last_usable: u64,
    pub partitions: Vec<Part>,
}

// ---- sfdisk -J deserialisation -------------------------------------------

#[derive(Deserialize)]
struct SfdiskDump {
    partitiontable: SfdiskTable,
}

#[derive(Deserialize)]
struct SfdiskTable {
    #[serde(default)]
    label: String,
    #[serde(default)]
    sectorsize: Option<u64>,
    #[serde(default)]
    lastlba: Option<u64>,
    #[serde(default)]
    partitions: Vec<SfdiskPart>,
}

#[derive(Deserialize)]
struct SfdiskPart {
    node: String,
    start: u64,
    size: u64,
    #[serde(default, rename = "type")]
    ptype: String,
}

fn parse_number(node: &str) -> u32 {
    let digits: String = node
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    digits.parse().unwrap_or(0)
}

fn disk_sectors(disk: &str, sector_size: u64) -> u64 {
    if have("blockdev") {
        if let Ok(out) = run_raw(&["blockdev", "--getsz", disk], None) {
            if out.ok() {
                if let Ok(sz512) = out.stdout.trim().parse::<u64>() {
                    // --getsz reports 512-byte sectors
                    return sz512 * 512 / sector_size;
                }
            }
        }
    }
    0
}

pub fn read_table(disk: &str) -> Result<Table> {
    require(&["sfdisk"])?;
    let out = run_raw(&["sfdisk", "-J", disk], None)?;
    if !out.ok() {
        return Err(Error::Command {
            program: format!("sfdisk -J {disk}"),
            code: out.status,
            stderr: out.stderr,
        });
    }
    let dump: SfdiskDump = serde_json::from_str(&out.stdout)?;
    let t = dump.partitiontable;
    let sector_size = t.sectorsize.unwrap_or(512);
    let mut partitions: Vec<Part> = t
        .partitions
        .into_iter()
        .map(|p| Part {
            number: parse_number(&p.node),
            node: p.node,
            start: p.start,
            size: p.size,
            ptype: p.ptype,
        })
        .collect();
    partitions.sort_by_key(|p| p.start);
    let total = disk_sectors(disk, sector_size);
    let last_usable = t.lastlba.unwrap_or(total.saturating_sub(1));
    Ok(Table {
        disk: disk.to_string(),
        label: t.label,
        sector_size,
        total_sectors: total,
        last_usable,
        partitions,
    })
}

impl Table {
    pub fn by_node(&self, node: &str) -> Option<&Part> {
        self.partitions.iter().find(|p| p.node == node)
    }
}

/// Free-space analysis immediately following a partition.
pub struct FreeSpace {
    pub free_sectors: u64,
    /// The partition that limits growth (immediately after), if any.
    pub blocking: Option<Part>,
}

pub fn free_after(table: &Table, part: &Part) -> FreeSpace {
    let mut following: Vec<&Part> = table
        .partitions
        .iter()
        .filter(|p| p.start > part.start)
        .collect();
    following.sort_by_key(|p| p.start);
    if let Some(next) = following.first() {
        let gap = next.start.saturating_sub(part.end() + 1);
        return FreeSpace {
            free_sectors: gap,
            blocking: Some((*next).clone()),
        };
    }
    let limit = if table.last_usable != 0 {
        table.last_usable
    } else {
        table.total_sectors.saturating_sub(1)
    };
    FreeSpace {
        free_sectors: limit.saturating_sub(part.end()),
        blocking: None,
    }
}

pub fn max_grow_bytes(table: &Table, part: &Part) -> u64 {
    free_after(table, part).free_sectors * table.sector_size
}

/// Set partition `number` to `new_size_sectors`, keeping its start offset.
pub fn set_partition_size(
    disk: &str,
    number: u32,
    new_size_sectors: u64,
    dry_run: bool,
) -> Result<()> {
    require(&["sfdisk"])?;
    if new_size_sectors == 0 {
        return Err(Error::Safety("refusing a zero-size partition".into()));
    }
    let spec = format!("size={new_size_sectors}\n");
    if dry_run {
        eprintln!(
            "[dry-run] sfdisk --no-reread -N {number} {disk} <<< '{}'",
            spec.trim()
        );
        return Ok(());
    }
    let args = vec![
        "sfdisk".to_string(),
        "--no-reread".to_string(),
        "-N".to_string(),
        number.to_string(),
        disk.to_string(),
    ];
    let out = run_owned(&args, Some(&spec))?;
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
    reread(disk, dry_run);
    Ok(())
}

pub fn reread(disk: &str, dry_run: bool) {
    if dry_run {
        return;
    }
    if have("partprobe") {
        let _ = run_raw(&["partprobe", disk], None);
    } else if have("partx") {
        let _ = run_raw(&["partx", "-u", disk], None);
    } else if have("blockdev") {
        let _ = run_raw(&["blockdev", "--rereadpt", disk], None);
    }
}

/// Write a restorable sfdisk dump of the partition table to `dest`.
pub fn backup_table(disk: &str, dest: &str) -> Result<()> {
    require(&["sfdisk"])?;
    let out = run_raw(&["sfdisk", "-d", disk], None)?;
    if !out.ok() {
        return Err(Error::Command {
            program: format!("sfdisk -d {disk}"),
            code: out.status,
            stderr: out.stderr,
        });
    }
    std::fs::write(dest, out.stdout)?;
    Ok(())
}

/// Restore a partition table from a previously written sfdisk dump.
pub fn restore_table(disk: &str, src: &str, dry_run: bool) -> Result<()> {
    require(&["sfdisk"])?;
    let dump = std::fs::read_to_string(src)?;
    if dry_run {
        eprintln!("[dry-run] sfdisk {disk} < {src}");
        return Ok(());
    }
    let out = run_owned(&["sfdisk".to_string(), disk.to_string()], Some(&dump))?;
    if !out.ok() {
        return Err(Error::Command {
            program: format!("sfdisk {disk} < {src}"),
            code: out.status,
            stderr: out.stderr,
        });
    }
    Ok(())
}
