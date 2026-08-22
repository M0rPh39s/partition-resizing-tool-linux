//! Turns a resize request + discovered topology into an ordered, safe plan.
//!
//! The planner is pure analysis — it never touches a device. It decides the
//! concrete target size, whether the operation is online or offline, the exact
//! ordered operations (filesystem-before-partition on shrink; container-first
//! on grow), the risk level, warnings and hard blockers.

use serde::{Deserialize, Serialize};

use crate::errors::{Error, Result};
use crate::filesystem::{capabilities_for, probe};
use crate::partition as part;
use crate::safety;
use crate::storage::Topology;
use crate::util::sizes::{format_size, round_down, round_up};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OpKind {
    Fsck,
    FsGrow,
    FsShrink,
    BlockdevGrow,
    BlockdevShrink,
    PartitionGrow,
    PartitionShrink,
    Verify,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Operation {
    pub kind: OpKind,
    pub device: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_size: Option<u64>,
    pub description: String,
}

impl Operation {
    fn new(kind: OpKind, device: &str, target: Option<u64>, desc: &str) -> Self {
        Operation {
            kind,
            device: device.to_string(),
            target_size: target,
            description: desc.to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

impl RiskLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            RiskLevel::Low => "LOW",
            RiskLevel::Medium => "MEDIUM",
            RiskLevel::High => "HIGH",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Direction {
    Grow,
    Shrink,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResizePlan {
    pub target: String,
    pub topology: Topology,
    pub direction: Direction,
    pub current_fs_size: u64,
    pub target_fs_size: u64,
    pub used_bytes: u64,
    pub online: bool,
    pub requires_reboot: bool,
    pub destructive: bool,
    pub risk_level: RiskLevel,
    pub operations: Vec<Operation>,
    pub warnings: Vec<String>,
    pub blockers: Vec<String>,
}

impl ResizePlan {
    pub fn feasible(&self) -> bool {
        self.blockers.is_empty()
    }
}

/// A requested size, in one of the accepted forms.
#[derive(Debug, Clone, Copy)]
pub enum Request {
    /// Absolute target size.
    Size(u64),
    /// Grow by this many bytes.
    GrowBy(u64),
    /// Shrink by this many bytes.
    ShrinkBy(u64),
    /// Grow to fill all available space.
    GrowMax,
}

fn measure(topo: &Topology) -> (u64, u64, u64, u64) {
    let fs = topo.filesystem();
    match probe(topo.fstype(), &fs.device, topo.mountpoint.as_deref()) {
        Ok(p) => (p.size_bytes, p.used_bytes, p.block_size, p.min_size_bytes),
        Err(_) => (fs.size, fs.used.unwrap_or(0), 4096, 0),
    }
}

fn max_fill(topo: &Topology) -> Result<u64> {
    match topo.partition() {
        None => Ok(topo.filesystem().size),
        Some(p) => {
            let table = part::read_table(&topo.disk().device)?;
            match table.by_node(&p.device) {
                Some(tp) => Ok(p.size + part::max_grow_bytes(&table, tp)),
                None => Ok(topo.filesystem().size),
            }
        }
    }
}

pub fn build_plan(topo: Topology, request: Request) -> Result<ResizePlan> {
    let fstype = topo.fstype().to_string();
    let caps = capabilities_for(&fstype);
    let (current, used, block_size, fs_min) = measure(&topo);

    let target = match request {
        Request::Size(s) => s,
        Request::GrowBy(b) => current + b,
        Request::ShrinkBy(b) => current.saturating_sub(b),
        Request::GrowMax => max_fill(&topo)?,
    };

    if target == 0 {
        return Err(Error::Plan(
            "requested target size resolves to zero bytes".into(),
        ));
    }

    let direction = if target > current {
        Direction::Grow
    } else if target < current {
        Direction::Shrink
    } else {
        Direction::None
    };

    if direction == Direction::None {
        return Ok(ResizePlan {
            target: topo.target.clone(),
            topology: topo,
            direction,
            current_fs_size: current,
            target_fs_size: current,
            used_bytes: used,
            online: true,
            requires_reboot: false,
            destructive: false,
            risk_level: RiskLevel::Low,
            operations: vec![],
            warnings: vec!["target equals current size; nothing to do".into()],
            blockers: vec![],
        });
    }

    match direction {
        Direction::Grow => plan_grow(topo, &caps, current, target, used),
        Direction::Shrink => plan_shrink(topo, &caps, current, target, used, block_size, fs_min),
        Direction::None => unreachable!(),
    }
}

fn plan_grow(
    topo: Topology,
    caps: &crate::filesystem::Capabilities,
    current: u64,
    target: u64,
    used: u64,
) -> Result<ResizePlan> {
    let warnings = Vec::new();
    let mut blockers = Vec::new();

    if !(caps.online_grow || caps.offline_grow) {
        blockers.push(format!("{} cannot be grown", caps.fstype));
    }

    // A filesystem can grow up to its containing partition with no partition
    // change. The partition (and layers below it) only grow when the target
    // exceeds the current partition size.
    let mut partition_grow_needed = false;
    if let Some(pl) = topo.partition() {
        if target > pl.size {
            partition_grow_needed = true;
            let extra = target - pl.size;
            let table = part::read_table(&topo.disk().device)?;
            if let Some(tp) = table.by_node(&pl.device) {
                let free = part::free_after(&table, tp);
                let free_bytes = free.free_sectors * table.sector_size;
                if free_bytes < extra {
                    match free.blocking {
                        Some(next) => blockers.push(format!(
                            "the partition cannot be extended because {} occupies the \
                             space immediately after it. Moving {} would be required — \
                             this relocates filesystem data and carries significantly \
                             higher risk.",
                            next.node, next.node
                        )),
                        None => blockers.push(format!(
                            "not enough free space after {}: need {} more, only {} available",
                            pl.device,
                            format_size(extra),
                            format_size(free_bytes)
                        )),
                    }
                }
            } else {
                blockers.push(format!("partition {} not found in table", pl.device));
            }
        }
    }

    let online = blockers.is_empty() && caps.online_grow;
    let requires_reboot = blockers.is_empty() && !online && caps.offline_grow;

    let ops = grow_ops(&topo, target, partition_grow_needed);
    Ok(ResizePlan {
        target: topo.target.clone(),
        topology: topo,
        direction: Direction::Grow,
        current_fs_size: current,
        target_fs_size: target,
        used_bytes: used,
        online,
        requires_reboot,
        destructive: false,
        risk_level: RiskLevel::Low,
        operations: ops,
        warnings,
        blockers,
    })
}

fn grow_ops(topo: &Topology, target: u64, partition_grow_needed: bool) -> Vec<Operation> {
    let mut ops = Vec::new();
    if partition_grow_needed {
        if let Some(p) = topo.partition() {
            ops.push(Operation::new(
                OpKind::PartitionGrow,
                &p.device,
                None,
                "Grow partition to consume the free space after it",
            ));
        }
        if let Some(l) = topo.luks() {
            ops.push(Operation::new(
                OpKind::BlockdevGrow,
                &l.device,
                None,
                "Grow the LUKS mapping to fill the enlarged partition",
            ));
        }
        if let Some(vg) = topo.lvm_vg() {
            if let Some(pv) = &vg.pv_device {
                ops.push(Operation::new(
                    OpKind::BlockdevGrow,
                    pv,
                    None,
                    "Grow the LVM physical volume",
                ));
            }
        }
    }
    if let Some(lv) = topo.lvm_lv() {
        ops.push(Operation::new(
            OpKind::BlockdevGrow,
            &lv.device,
            Some(target),
            "Extend the logical volume",
        ));
    }
    let fs_dev = topo.fs_backing_device().to_string();
    ops.push(Operation::new(
        OpKind::FsGrow,
        &fs_dev,
        Some(target),
        &format!("Grow {} filesystem to fill the container", topo.fstype()),
    ));
    ops.push(Operation::new(
        OpKind::Verify,
        &fs_dev,
        Some(target),
        "Verify final sizes",
    ));
    ops
}

#[allow(clippy::too_many_arguments)]
fn plan_shrink(
    topo: Topology,
    caps: &crate::filesystem::Capabilities,
    current: u64,
    target: u64,
    used: u64,
    block_size: u64,
    fs_min: u64,
) -> Result<ResizePlan> {
    let mut warnings = Vec::new();
    let mut blockers = Vec::new();

    if !caps.offline_shrink && !caps.online_shrink {
        blockers.push(format!(
            "{} cannot be shrunk (it only supports growing)",
            caps.fstype
        ));
    }

    if let Err(Error::Safety(m)) = safety::assert_shrink_safe(target, used, fs_min) {
        blockers.push(m);
    }

    let target_fs = if block_size > 0 {
        round_down(target, block_size)
    } else {
        target
    };

    // Reclaiming partition space cannot be done online (the kernel refuses to
    // shrink an in-use partition), so a shrink always requires the offline
    // maintenance environment.
    if caps.online_shrink {
        warnings.push(format!(
            "{} supports online filesystem shrink, but reclaiming the freed \
             partition space still requires the offline maintenance environment",
            caps.fstype
        ));
    }

    let ops = shrink_ops(&topo, target_fs);
    Ok(ResizePlan {
        target: topo.target.clone(),
        topology: topo,
        direction: Direction::Shrink,
        current_fs_size: current,
        target_fs_size: target_fs,
        used_bytes: used,
        online: false,
        requires_reboot: true,
        destructive: true,
        risk_level: RiskLevel::High,
        operations: ops,
        warnings,
        blockers,
    })
}

fn shrink_ops(topo: &Topology, target_fs: u64) -> Vec<Operation> {
    let mut ops = Vec::new();
    let fs_dev = topo.fs_backing_device().to_string();

    ops.push(Operation::new(
        OpKind::Fsck,
        &fs_dev,
        None,
        "Check filesystem integrity",
    ));
    ops.push(Operation::new(
        OpKind::FsShrink,
        &fs_dev,
        Some(target_fs),
        &format!("Shrink {} filesystem to the target size", topo.fstype()),
    ));
    ops.push(Operation::new(
        OpKind::Verify,
        &fs_dev,
        Some(target_fs),
        "Verify the shrunk filesystem",
    ));

    // Container layers, innermost first: LV, then PV, then LUKS.
    let container_target = round_up(target_fs, 4 * 1024 * 1024);
    if let Some(lv) = topo.lvm_lv() {
        ops.push(Operation::new(
            OpKind::BlockdevShrink,
            &lv.device,
            Some(container_target),
            "Reduce the logical volume to fit the filesystem",
        ));
        if let Some(vg) = topo.lvm_vg() {
            if let Some(pv) = &vg.pv_device {
                ops.push(Operation::new(
                    OpKind::BlockdevShrink,
                    pv,
                    Some(container_target),
                    "Shrink the physical volume",
                ));
            }
        }
    }
    if let Some(l) = topo.luks() {
        ops.push(Operation::new(
            OpKind::BlockdevShrink,
            &l.device,
            Some(container_target),
            "Shrink the LUKS mapping",
        ));
    }

    // Partition last, aligned up to 1 MiB and never below the fs/container.
    if let Some(p) = topo.partition() {
        let part_target = round_up(target_fs.max(container_target), part::ALIGN);
        ops.push(Operation::new(
            OpKind::PartitionShrink,
            &p.device,
            Some(part_target),
            "Shrink the partition (never before the filesystem)",
        ));
        ops.push(Operation::new(
            OpKind::Verify,
            &p.device,
            Some(part_target),
            "Verify the partition and filesystem sizes",
        ));
    }
    ops
}
