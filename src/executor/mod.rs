//! Executes the operations of a validated [`ResizePlan`].
//!
//! The same executor drives the online path (Mode 1, mounted) and the offline
//! maintenance path (Mode 2, unmounted). Optional state callbacks let an
//! interrupted run be recovered.

use crate::crypt;
use crate::errors::{Error, Result};
use crate::filesystem;
use crate::lvm;
use crate::partition as part;
use crate::planner::{Direction, OpKind, Operation, ResizePlan};
use crate::recovery::{OperationState, State, StateStore};
use crate::safety;
use crate::storage::LayerKind;
use crate::verification;

pub struct Executor<'a> {
    plan: &'a ResizePlan,
    online: bool,
    dry_run: bool,
    store: Option<&'a StateStore>,
    op: Option<&'a mut OperationState>,
}

impl<'a> Executor<'a> {
    pub fn new(plan: &'a ResizePlan, online: bool, dry_run: bool) -> Self {
        Executor {
            plan,
            online,
            dry_run,
            store: None,
            op: None,
        }
    }

    pub fn with_state(mut self, store: &'a StateStore, op: &'a mut OperationState) -> Self {
        self.store = Some(store);
        self.op = Some(op);
        self
    }

    fn record(&mut self, state: State) -> Result<()> {
        if let (Some(store), Some(op)) = (self.store.as_ref(), self.op.as_mut()) {
            store.transition(op, state)?;
        }
        Ok(())
    }

    fn guard(&self, device: &str) -> Result<()> {
        safety::assert_test_device(device)?;
        safety::assert_test_device(&self.plan.topology.disk().device)?;
        Ok(())
    }

    pub fn run(&mut self) -> Result<()> {
        let ops = self.plan.operations.clone();
        for op in &ops {
            self.guard(&op.device)?;
            self.record(state_for(op.kind))?;
            if crate::util::cmd::is_verbose() {
                eprintln!("== {} : {}", op_label(op.kind), op.description);
            }
            let res = self.dispatch(op);
            if let Err(e) = res {
                if let (Some(store), Some(state_op)) = (self.store.as_ref(), self.op.as_mut()) {
                    let _ = store.fail(state_op, e.to_string());
                }
                return Err(e);
            }
        }
        // Final invariant check unless purely a dry run.
        if !self.dry_run {
            verification::verify_fits(&self.plan.topology)?;
        }
        Ok(())
    }

    fn dispatch(&self, op: &Operation) -> Result<()> {
        let topo = &self.plan.topology;
        match op.kind {
            OpKind::Fsck => filesystem::check(topo.fstype(), &op.device, self.dry_run),
            OpKind::FsGrow => {
                if self.online {
                    let mp = topo.mountpoint.as_deref().unwrap_or("");
                    filesystem::grow_online(
                        topo.fstype(),
                        &op.device,
                        mp,
                        op.target_size,
                        self.dry_run,
                    )
                } else {
                    filesystem::grow_offline(
                        topo.fstype(),
                        &op.device,
                        op.target_size,
                        self.dry_run,
                    )
                }
            }
            OpKind::FsShrink => {
                let target = op
                    .target_size
                    .ok_or_else(|| Error::Execution("fs shrink missing target".into()))?;
                filesystem::shrink_offline(topo.fstype(), &op.device, target, self.dry_run)
            }
            OpKind::BlockdevGrow => self.blockdev(op, true),
            OpKind::BlockdevShrink => self.blockdev(op, false),
            OpKind::PartitionGrow => self.partition_grow(op),
            OpKind::PartitionShrink => self.partition_shrink(op),
            OpKind::Verify => Ok(()),
        }
    }

    fn is_pv(&self, device: &str) -> bool {
        self.plan
            .topology
            .layers
            .iter()
            .any(|l| l.kind == LayerKind::LvmVg && l.pv_device.as_deref() == Some(device))
    }

    fn blockdev(&self, op: &Operation, grow: bool) -> Result<()> {
        let topo = &self.plan.topology;
        if op.device.is_empty() {
            return Ok(());
        }
        // LUKS?
        if topo.luks().map(|l| l.device.as_str()) == Some(op.device.as_str()) {
            return if grow {
                crypt::grow(&op.device, self.dry_run)
            } else {
                crypt::shrink(&op.device, op.target_size.unwrap_or(0), self.dry_run)
            };
        }
        // PV?
        if self.is_pv(&op.device) {
            return if grow {
                lvm::pv_grow(&op.device, self.dry_run)
            } else {
                lvm::pv_shrink(&op.device, op.target_size.unwrap_or(0), self.dry_run)
            };
        }
        // LV?
        if topo.lvm_lv().map(|l| l.device.as_str()) == Some(op.device.as_str()) {
            return if grow {
                lvm::lv_grow(&op.device, op.target_size.unwrap_or(0), self.dry_run)
            } else {
                lvm::lv_shrink(&op.device, op.target_size.unwrap_or(0), self.dry_run)
            };
        }
        Ok(())
    }

    fn partition_grow(&self, op: &Operation) -> Result<()> {
        let topo = &self.plan.topology;
        let table = part::read_table(&topo.disk().device)?;
        let p = table
            .by_node(&op.device)
            .ok_or_else(|| Error::Execution(format!("partition {} not found", op.device)))?;
        let free = part::free_after(&table, p);
        let mut new_sectors = p.size + free.free_sectors;
        let align = part::ALIGN / table.sector_size;
        if align > 0 {
            new_sectors = (new_sectors / align) * align;
        }
        if new_sectors <= p.size {
            return Ok(());
        }
        part::set_partition_size(&topo.disk().device, p.number, new_sectors, self.dry_run)
    }

    fn partition_shrink(&self, op: &Operation) -> Result<()> {
        let topo = &self.plan.topology;
        let target = op
            .target_size
            .ok_or_else(|| Error::Execution("partition shrink missing target".into()))?;
        let table = part::read_table(&topo.disk().device)?;
        let p = table
            .by_node(&op.device)
            .ok_or_else(|| Error::Execution(format!("partition {} not found", op.device)))?;
        let new_sectors = target / table.sector_size;
        if new_sectors >= p.size {
            return Ok(()); // never grow here, and never shrink below fs
        }
        part::set_partition_size(&topo.disk().device, p.number, new_sectors, self.dry_run)
    }
}

fn state_for(kind: OpKind) -> State {
    match kind {
        OpKind::Fsck => State::CheckingFilesystem,
        OpKind::FsShrink => State::ShrinkingFilesystem,
        OpKind::FsGrow => State::GrowingFilesystem,
        OpKind::BlockdevGrow | OpKind::BlockdevShrink => State::ResizingBlockDevice,
        OpKind::PartitionGrow | OpKind::PartitionShrink => State::ResizingPartition,
        OpKind::Verify => State::Verifying,
    }
}

fn op_label(kind: OpKind) -> &'static str {
    match kind {
        OpKind::Fsck => "fsck",
        OpKind::FsGrow => "fs-grow",
        OpKind::FsShrink => "fs-shrink",
        OpKind::BlockdevGrow => "blockdev-grow",
        OpKind::BlockdevShrink => "blockdev-shrink",
        OpKind::PartitionGrow => "partition-grow",
        OpKind::PartitionShrink => "partition-shrink",
        OpKind::Verify => "verify",
    }
}

/// Take best-effort backups of the partition table and LVM metadata before a
/// destructive operation. Returns a human description of what was saved.
pub fn take_backups(plan: &ResizePlan, backup_dir: &str, dry_run: bool) -> Result<Vec<String>> {
    let mut saved = Vec::new();
    if plan.direction != Direction::Shrink {
        return Ok(saved);
    }
    let topo = &plan.topology;
    let disk = &topo.disk().device;
    let ptbak = format!("{backup_dir}/partition-table.sfdisk");
    if dry_run {
        saved.push(format!("would back up partition table -> {ptbak}"));
    } else if part::backup_table(disk, &ptbak).is_ok() {
        saved.push(format!("partition table backup: {ptbak}"));
    }
    if let Some(vg) = topo.lvm_vg() {
        if let Some(vg_name) = &vg.vg_name {
            let vgbak = format!("{backup_dir}/lvm-{vg_name}.vgcfg");
            if dry_run {
                saved.push(format!("would back up LVM metadata -> {vgbak}"));
            } else if lvm::vg_backup(vg_name, &vgbak, false).is_ok() {
                saved.push(format!("LVM metadata backup: {vgbak}"));
            }
        }
    }
    Ok(saved)
}
