"""Generate the POSIX-shell maintenance script that runs inside the initramfs.

A real initramfs does not contain a Python interpreter, so the offline resize
that runs *after the reboot* is emitted as a self-contained ``/bin/sh`` script
built from the plan.  It uses only the small set of binaries the boot module
copies into the maintenance image (e2fsprogs, sfdisk, lvm, cryptsetup, ...).

The very same operations are also executable directly through
:class:`partresize.executor.Executor` (``online=False``), which is what the
integration tests drive against loop devices — so the offline logic is fully
exercised without a reboot.
"""

from __future__ import annotations

from .model import OpKind, ResizePlan

_STATE_FILE = "/run/partresize.state"
_MARKER = "partresize.maint"


def required_binaries(plan: ResizePlan) -> list[str]:
    """The external tools the maintenance image must contain for this plan."""
    fstype = plan.topology.fstype
    bins = {"sh", "mount", "umount", "blockdev", "sfdisk", "partprobe"}
    if fstype in ("ext2", "ext3", "ext4"):
        bins |= {"e2fsck", "resize2fs", "dumpe2fs"}
    elif fstype == "btrfs":
        bins |= {"btrfs"}
    elif fstype == "xfs":
        bins |= {"xfs_growfs", "xfs_repair"}
    if plan.topology.has_luks:
        bins |= {"cryptsetup"}
    if plan.topology.has_lvm:
        bins |= {"lvm", "pvresize", "lvresize", "vgchange"}
    return sorted(bins)


def _op_commands(plan: ResizePlan) -> list[str]:
    """Translate plan operations into shell commands."""
    fstype = plan.topology.fstype
    disk = plan.topology.disk.device
    lines: list[str] = []
    for op in plan.operations:
        dev = op.device
        if op.kind == OpKind.FSCK:
            if fstype.startswith("ext"):
                lines.append(f'run "Checking filesystem" e2fsck -f -y "{dev}"')
            elif fstype == "btrfs":
                lines.append(f'run "Checking filesystem" btrfs check "{dev}"')
        elif op.kind == OpKind.FS_SHRINK:
            if fstype.startswith("ext"):
                kib = op.target_size // 1024
                lines.append(
                    f'run "Shrinking filesystem" resize2fs "{dev}" {kib}K'
                )
            elif fstype == "btrfs":
                lines.append(
                    f'run "Shrinking filesystem" btrfs_resize "{dev}" {op.target_size}'
                )
        elif op.kind == OpKind.FS_GROW:
            if fstype.startswith("ext"):
                lines.append(f'run "Growing filesystem" resize2fs "{dev}"')
            elif fstype == "btrfs":
                lines.append(f'run "Growing filesystem" btrfs_resize "{dev}" max')
        elif op.kind == OpKind.BLOCKDEV_SHRINK:
            sectors = (op.target_size or 0) // 512
            if plan.topology.has_luks and dev == plan.topology.luks.device:
                lines.append(
                    f'run "Shrinking LUKS" cryptsetup resize --size {sectors} "{dev}"'
                )
            elif plan.topology.has_lvm and dev == plan.topology.lvm_lv.device:
                lines.append(
                    f'run "Shrinking LV" lvresize -f --size {op.target_size}b "{dev}"'
                )
            else:
                lines.append(
                    f'run "Shrinking PV" pvresize --setphysicalvolumesize '
                    f'{op.target_size}b "{dev}"'
                )
        elif op.kind == OpKind.BLOCKDEV_GROW:
            if plan.topology.has_luks and dev == plan.topology.luks.device:
                lines.append(f'run "Growing LUKS" cryptsetup resize "{dev}"')
            elif plan.topology.has_lvm and dev == plan.topology.lvm_lv.device:
                lines.append(
                    f'run "Extending LV" lvresize -f --size {op.target_size}b "{dev}"'
                )
            elif dev:
                lines.append(f'run "Growing PV" pvresize "{dev}"')
        elif op.kind == OpKind.PARTITION_SHRINK:
            sectors = (op.target_size or 0) // 512
            num = _partnum(dev)
            lines.append(
                f'run "Resizing partition" set_part_size "{disk}" {num} {sectors}'
            )
        elif op.kind == OpKind.PARTITION_GROW:
            num = _partnum(dev)
            lines.append(
                f'run "Resizing partition" grow_part "{disk}" {num}'
            )
        elif op.kind == OpKind.VERIFY:
            lines.append('record_state VERIFYING')
    return lines


def _partnum(node: str) -> int:
    digits = ""
    for ch in reversed(node):
        if ch.isdigit():
            digits = ch + digits
        else:
            break
    return int(digits) if digits else 0


def generate_script(plan: ResizePlan, op_id: str) -> str:
    """Emit the full maintenance shell script for ``plan``."""
    fs = plan.topology.filesystem
    target_dev = fs.props.get("backing", fs.device)
    total = len([op for op in plan.operations if op.kind != OpKind.VERIFY]) + 1
    body = "\n".join("    " + line for line in _op_commands(plan))

    return f"""#!/bin/sh
# partresize maintenance script (auto-generated) — operation {op_id}
# Runs inside the initramfs while the target root filesystem is NOT mounted.
set -eu

STATE_FILE="{_STATE_FILE}"
OP_ID="{op_id}"
TARGET="{target_dev}"
FSTYPE="{plan.topology.fstype}"
STEP=0
TOTAL={total}

record_state() {{
    echo "state=$1 op=$OP_ID $(date -u +%s)" > "$STATE_FILE" 2>/dev/null || true
}}

fail() {{
    record_state FAILED
    echo "partresize: FAILED at step $STEP: $*" >&2
    # Leave the filesystem no larger than its partition: a failed partition
    # shrink after a completed fs shrink is safe (fs smaller than partition).
    exit 1
}}

run() {{
    STEP=$((STEP + 1))
    label="$1"; shift
    echo "[$STEP/$TOTAL] $label"
    "$@" || fail "$label ($*)"
}}

btrfs_resize() {{
    dev="$1"; size="$2"; mp=$(mktemp -d)
    mount "$dev" "$mp" && btrfs filesystem resize "$size" "$mp"; rc=$?
    umount "$mp" 2>/dev/null || true; rmdir "$mp" 2>/dev/null || true
    return $rc
}}

set_part_size() {{
    disk="$1"; num="$2"; sectors="$3"
    printf 'size=%s\\n' "$sectors" | sfdisk --no-reread -N "$num" "$disk"
    partprobe "$disk" 2>/dev/null || blockdev --rereadpt "$disk" 2>/dev/null || true
}}

grow_part() {{
    disk="$1"; num="$2"
    # Grow to fill: recompute via sfdisk by setting size to the max free span.
    echo ", +" | sfdisk --no-reread -N "$num" "$disk" 2>/dev/null || \\
        printf 'size=+\\n' | sfdisk --no-reread -N "$num" "$disk"
    partprobe "$disk" 2>/dev/null || blockdev --rereadpt "$disk" 2>/dev/null || true
}}

ensure_unmounted() {{
    if grep -qs " $1 " /proc/mounts; then
        umount "$1" 2>/dev/null || fail "target $1 is mounted and could not be unmounted"
    fi
}}

echo "PartResize Maintenance Environment"
echo "Resizing Linux installation..."
echo "Target: $TARGET"
echo "Filesystem: $FSTYPE"

record_state BOOTING_MAINTENANCE
ensure_unmounted "$TARGET"

{body}

record_state COMPLETED
echo "Resize completed successfully."
"""
