//! Generates the POSIX-shell maintenance script embedded in the initramfs.
//!
//! A real initramfs has no Rust/Python runtime, so the offline resize that runs
//! after the reboot is emitted as a self-contained `/bin/sh` script built from
//! the plan, using only the tools copied into the maintenance image.

use crate::planner::{OpKind, ResizePlan};

pub fn required_binaries(plan: &ResizePlan) -> Vec<String> {
    let mut bins: Vec<&str> = vec!["sh", "mount", "umount", "blockdev", "sfdisk", "partprobe"];
    match plan.topology.fstype() {
        "ext2" | "ext3" | "ext4" => bins.extend(["e2fsck", "resize2fs", "dumpe2fs"]),
        "btrfs" => bins.push("btrfs"),
        "xfs" => bins.extend(["xfs_growfs", "xfs_repair"]),
        _ => {}
    }
    if plan.topology.has_luks() {
        bins.push("cryptsetup");
    }
    if plan.topology.has_lvm() {
        bins.extend(["lvm", "pvresize", "lvreduce", "lvextend", "vgchange"]);
    }
    let mut v: Vec<String> = bins.into_iter().map(|s| s.to_string()).collect();
    v.sort();
    v.dedup();
    v
}

fn partnum(node: &str) -> u32 {
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

fn op_lines(plan: &ResizePlan) -> Vec<String> {
    let fstype = plan.topology.fstype();
    let disk = &plan.topology.disk().device;
    let mut lines = Vec::new();
    for op in &plan.operations {
        let dev = &op.device;
        match op.kind {
            OpKind::Fsck => {
                if fstype.starts_with("ext") {
                    lines.push(format!(
                        "run \"Checking filesystem\" e2fsck -f -y \"{dev}\""
                    ));
                } else if fstype == "btrfs" {
                    lines.push(format!("run \"Checking filesystem\" btrfs check \"{dev}\""));
                }
            }
            OpKind::FsShrink => {
                let t = op.target_size.unwrap_or(0);
                if fstype.starts_with("ext") {
                    lines.push(format!(
                        "run \"Shrinking filesystem\" resize2fs \"{dev}\" {}K",
                        t / 1024
                    ));
                } else if fstype == "btrfs" {
                    lines.push(format!(
                        "run \"Shrinking filesystem\" btrfs_resize \"{dev}\" {t}"
                    ));
                }
            }
            OpKind::FsGrow => {
                if fstype.starts_with("ext") {
                    lines.push(format!("run \"Growing filesystem\" resize2fs \"{dev}\""));
                } else if fstype == "btrfs" {
                    lines.push(format!(
                        "run \"Growing filesystem\" btrfs_resize \"{dev}\" max"
                    ));
                }
            }
            OpKind::BlockdevShrink => {
                let t = op.target_size.unwrap_or(0);
                if plan.topology.luks().map(|l| l.device.as_str()) == Some(dev.as_str()) {
                    lines.push(format!(
                        "run \"Shrinking LUKS\" cryptsetup resize --size {} \"{dev}\"",
                        t / 512
                    ));
                } else if plan.topology.lvm_lv().map(|l| l.device.as_str()) == Some(dev.as_str()) {
                    lines.push(format!("run \"Reducing LV\" lvreduce -f -L {t}b \"{dev}\""));
                } else {
                    lines.push(format!(
                        "run \"Shrinking PV\" pvresize --setphysicalvolumesize {t}b \"{dev}\""
                    ));
                }
            }
            OpKind::BlockdevGrow => {
                if plan.topology.luks().map(|l| l.device.as_str()) == Some(dev.as_str()) {
                    lines.push(format!("run \"Growing LUKS\" cryptsetup resize \"{dev}\""));
                } else if plan.topology.lvm_lv().map(|l| l.device.as_str()) == Some(dev.as_str()) {
                    lines.push(format!(
                        "run \"Extending LV\" lvextend -L {}b \"{dev}\"",
                        op.target_size.unwrap_or(0)
                    ));
                } else if !dev.is_empty() {
                    lines.push(format!("run \"Growing PV\" pvresize \"{dev}\""));
                }
            }
            OpKind::PartitionShrink => {
                let sectors = op.target_size.unwrap_or(0) / 512;
                lines.push(format!(
                    "run \"Resizing partition\" set_part_size \"{disk}\" {} {sectors}",
                    partnum(dev)
                ));
            }
            OpKind::PartitionGrow => {
                lines.push(format!(
                    "run \"Resizing partition\" grow_part \"{disk}\" {}",
                    partnum(dev)
                ));
            }
            OpKind::Verify => lines.push("record_state VERIFYING".to_string()),
        }
    }
    lines
}

pub fn generate_maintenance_script(plan: &ResizePlan, op_id: &str) -> String {
    let fs = plan.topology.filesystem();
    let target_dev = &fs.device;
    let fstype = plan.topology.fstype();
    let total = plan
        .operations
        .iter()
        .filter(|o| o.kind != OpKind::Verify)
        .count()
        + 1;
    let body = op_lines(plan)
        .into_iter()
        .map(|l| format!("    {l}"))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"#!/bin/sh
# partresize maintenance script (auto-generated) — operation {op_id}
# Runs inside the initramfs while the target root filesystem is NOT mounted.
set -eu

STATE_FILE="/run/partresize.state"
OP_ID="{op_id}"
TARGET="{target_dev}"
FSTYPE="{fstype}"
STEP=0
TOTAL={total}

record_state() {{ echo "state=$1 op=$OP_ID $(date -u +%s 2>/dev/null)" > "$STATE_FILE" 2>/dev/null || true; }}
fail() {{ record_state FAILED; echo "partresize: FAILED at step $STEP: $*" >&2; exit 1; }}
run() {{ STEP=$((STEP + 1)); label="$1"; shift; echo "[$STEP/$TOTAL] $label"; "$@" || fail "$label ($*)"; }}

btrfs_resize() {{ dev="$1"; size="$2"; mp=$(mktemp -d); mount "$dev" "$mp" && btrfs filesystem resize "$size" "$mp"; rc=$?; umount "$mp" 2>/dev/null || true; rmdir "$mp" 2>/dev/null || true; return $rc; }}
set_part_size() {{ disk="$1"; num="$2"; sectors="$3"; printf 'size=%s\n' "$sectors" | sfdisk --no-reread -N "$num" "$disk"; partprobe "$disk" 2>/dev/null || blockdev --rereadpt "$disk" 2>/dev/null || true; }}
grow_part() {{ disk="$1"; num="$2"; printf 'size=+\n' | sfdisk --no-reread -N "$num" "$disk"; partprobe "$disk" 2>/dev/null || blockdev --rereadpt "$disk" 2>/dev/null || true; }}
ensure_unmounted() {{ if grep -qs " $1 " /proc/mounts; then umount "$1" 2>/dev/null || fail "target $1 is mounted"; fi; }}

echo "PartResize Maintenance Environment"
echo "Resizing Linux installation..."
echo "Target: $TARGET"
echo "Filesystem: $FSTYPE"

record_state BOOTING_MAINTENANCE
ensure_unmounted "$TARGET"

{body}

record_state COMPLETED
echo "Resize completed successfully."
"#
    )
}
