"""Turn a resize request + discovered topology into an ordered, safe plan.

The planner is pure analysis: it never touches a device.  It decides:

* the concrete target filesystem size,
* whether the operation can be done online or needs the offline maintenance
  environment,
* the exact ordered list of operations (always filesystem-before-partition on
  a shrink, container-before-content on a grow),
* the risk level, any warnings, and any hard blockers.
"""

from __future__ import annotations

import logging

from . import partition as part_mod
from .capabilities import capabilities_for
from .errors import PlanError
from .filesystems import driver_for
from .model import (
    LayerKind,
    Operation,
    OpKind,
    ResizePlan,
    RiskLevel,
    Topology,
)
from .safety import assert_shrink_safe
from .sizes import round_down, round_up

log = logging.getLogger("partresize.planner")

ALIGN = 1024 * 1024  # 1 MiB partition alignment


def _measure(topo: Topology) -> tuple[int, int, int, int]:
    """Return (fs_size, used, block_size, fs_min) for the filesystem."""
    fs = topo.filesystem
    driver = driver_for(topo.fstype)
    device = fs.props.get("backing", fs.device)
    mp = topo.mountpoint
    try:
        probe = driver.probe(device, mountpoint=mp)
        return probe.size_bytes, probe.used_bytes, probe.block_size, probe.min_size_bytes
    except Exception as exc:  # pragma: no cover - fall back to lsblk numbers
        log.debug("probe failed (%s); using topology sizes", exc)
        size = fs.size or fs.props.get("size", 0)
        used = fs.props.get("used", 0)
        return size, used, 4096, 0


def _resolve_target(
    direction: str,
    current: int,
    size: int | None,
    by: int | None,
    topo: Topology,
) -> int:
    """Compute the concrete target filesystem size in bytes."""
    if direction == "grow":
        if by is not None:
            return current + by
        if size is not None:
            return size
        # grow to fill: partition max, minus container overhead (approx).
        return _max_fill(topo)
    if direction == "shrink":
        if size is not None:
            return size
        if by is not None:
            return current - by
        raise PlanError("shrink requires --size or --by")
    # resize
    if size is not None:
        return size
    raise PlanError("resize requires --size")


def _max_fill(topo: Topology) -> int:
    part = topo.partition
    if part is None:
        return topo.filesystem.size
    table = part_mod.read_table(topo.disk.device)
    p = table.by_node(part.device)
    if p is None:
        return topo.filesystem.size
    grow_bytes = part_mod.max_grow_bytes(table, p)
    return part.size + grow_bytes


def build_plan(
    topo: Topology,
    *,
    direction: str,
    size: int | None = None,
    by: int | None = None,
) -> ResizePlan:
    fstype = topo.fstype
    caps = capabilities_for(fstype)
    current, used, block_size, fs_min = _measure(topo)

    if direction == "resize":
        # decide grow/shrink from target
        tentative = size if size is not None else current
        direction = "grow" if tentative >= current else "shrink"

    target = _resolve_target(direction, current, size, by, topo)
    # normalise direction if target implies otherwise
    if target > current:
        direction = "grow"
    elif target < current:
        direction = "shrink"
    else:
        direction = "none"

    warnings: list[str] = []
    blockers: list[str] = []

    if direction == "none":
        return ResizePlan(
            mountpoint=topo.mountpoint, topology=topo, direction="none",
            current_fs_size=current, target_fs_size=current, used_bytes=used,
            online=True, requires_reboot=False, risk=RiskLevel.LOW,
            operations=[], warnings=["target equals current size; nothing to do"],
        )

    if direction == "grow":
        return _plan_grow(
            topo, caps, current, target, used, block_size,
            warnings, blockers,
        )
    return _plan_shrink(
        topo, caps, current, target, used, block_size, fs_min,
        warnings, blockers,
    )


# --------------------------------------------------------------------------
# GROW
# --------------------------------------------------------------------------

def _plan_grow(topo, caps, current, target, used, block_size, warnings, blockers):
    part = topo.partition
    online = False
    requires_reboot = False
    risk = RiskLevel.LOW

    if not (caps.online_grow or caps.offline_grow):
        blockers.append(f"{caps.fstype} cannot be grown")

    # A filesystem can grow up to its containing partition with no partition
    # change at all.  The partition (and the layers below it) only need to grow
    # when the target exceeds the current partition size.
    partition_grow_needed = False
    if part is not None:
        partition_bytes = part.size
        extra_partition = target - partition_bytes
        if extra_partition > 0:
            partition_grow_needed = True
            table = part_mod.read_table(topo.disk.device)
            p = table.by_node(part.device)
            if p is None:
                blockers.append(f"partition {part.device} not found in table")
            else:
                free = part_mod.free_after(table, p)
                free_bytes = free.free_sectors * table.sector_size
                if free_bytes < extra_partition:
                    if free.blocking_partition is not None:
                        blockers.append(
                            f"the partition cannot be extended because "
                            f"{free.blocking_partition.node} occupies the "
                            f"required space immediately after it. Moving "
                            f"{free.blocking_partition.node} would be required "
                            f"— this relocates filesystem data and carries "
                            f"significantly higher risk."
                        )
                    else:
                        blockers.append(
                            f"not enough free space after {part.device}: need "
                            f"{extra_partition} more bytes, only {free_bytes} "
                            f"available"
                        )

    # ext4/btrfs/xfs online grow is possible while mounted; growing the last
    # partition online (the growpart pattern) is also supported.
    if not blockers and caps.online_grow:
        online = True
        requires_reboot = False
    elif not blockers and caps.offline_grow:
        online = False
        requires_reboot = True

    # Build operations (container-first): partition -> luks -> pv -> lv -> fs.
    ops = _grow_ops(topo, target, partition_grow_needed)
    return ResizePlan(
        mountpoint=topo.mountpoint, topology=topo, direction="grow",
        current_fs_size=current, target_fs_size=target, used_bytes=used,
        online=online, requires_reboot=requires_reboot, risk=risk,
        operations=ops, warnings=warnings, blockers=blockers,
    )


def _grow_ops(topo, target, partition_grow_needed=True):
    ops: list[Operation] = []
    part = topo.partition
    if part is not None and partition_grow_needed:
        ops.append(Operation(
            OpKind.PARTITION_GROW, part.device, None,
            "Grow partition to consume the free space after it",
        ))
    if topo.has_luks and partition_grow_needed:
        ops.append(Operation(
            OpKind.BLOCKDEV_GROW, topo.luks.device, None,
            "Grow the LUKS mapping to fill the enlarged partition",
        ))
    if topo.has_lvm:
        vg = next((x for x in topo.layers if x.kind == LayerKind.LVM_VG), None)
        if vg is not None:
            ops.append(Operation(
                OpKind.BLOCKDEV_GROW, vg.props.get("pv", ""), None,
                "Grow the LVM physical volume",
            ))
        lv = topo.lvm_lv
        ops.append(Operation(
            OpKind.BLOCKDEV_GROW, lv.device, target,
            "Extend the logical volume",
        ))
    fs_dev = topo.filesystem.props.get("backing", topo.filesystem.device)
    ops.append(Operation(
        OpKind.FS_GROW, fs_dev, target,
        f"Grow {topo.fstype} filesystem to fill the container",
    ))
    ops.append(Operation(OpKind.VERIFY, fs_dev, target, "Verify final sizes"))
    return ops


# --------------------------------------------------------------------------
# SHRINK
# --------------------------------------------------------------------------

def _plan_shrink(topo, caps, current, target, used, block_size, fs_min,
                 warnings, blockers):
    risk = RiskLevel.HIGH

    if not caps.offline_shrink and not caps.online_shrink:
        blockers.append(
            f"{caps.fstype} cannot be shrunk (it only supports growing)"
        )

    # Safety: target must clear used data + margin, and the fs minimum.
    try:
        assert_shrink_safe(target, used, fs_min)
    except Exception as exc:
        blockers.append(str(exc))

    # Align filesystem target down to a block boundary.
    target_fs = round_down(target, block_size) if block_size else target

    # A shrink that must reclaim partition space cannot be done online, because
    # the kernel will not shrink an in-use partition.  So: reboot required.
    online = False
    requires_reboot = True
    if caps.online_shrink:
        warnings.append(
            f"{caps.fstype} supports online filesystem shrink, but reclaiming "
            "the freed partition space still requires the offline maintenance "
            "environment"
        )

    ops = _shrink_ops(topo, target_fs, block_size)
    return ResizePlan(
        mountpoint=topo.mountpoint, topology=topo, direction="shrink",
        current_fs_size=current, target_fs_size=target_fs, used_bytes=used,
        online=online, requires_reboot=requires_reboot, risk=risk,
        operations=ops, warnings=warnings, blockers=blockers,
    )


def _shrink_ops(topo, target_fs, block_size):
    """Ordered shrink ops: fsck -> fs shrink -> verify -> containers -> part."""
    ops: list[Operation] = []
    fs_dev = topo.filesystem.props.get("backing", topo.filesystem.device)

    ops.append(Operation(OpKind.FSCK, fs_dev, None,
                         "Force a filesystem integrity check"))
    ops.append(Operation(OpKind.FS_SHRINK, fs_dev, target_fs,
                         f"Shrink {topo.fstype} filesystem to the target size"))
    ops.append(Operation(OpKind.VERIFY, fs_dev, target_fs,
                         "Verify the shrunk filesystem"))

    # Container layers, innermost first (LV, then PV, then LUKS).
    container_target = round_up(target_fs, 4 * 1024 * 1024)  # extent-aligned
    if topo.has_lvm:
        lv = topo.lvm_lv
        ops.append(Operation(OpKind.BLOCKDEV_SHRINK, lv.device, container_target,
                             "Shrink the logical volume to fit the filesystem"))
        vg = next((x for x in topo.layers if x.kind == LayerKind.LVM_VG), None)
        if vg is not None and vg.props.get("pv"):
            ops.append(Operation(OpKind.BLOCKDEV_SHRINK, vg.props["pv"],
                                 container_target,
                                 "Shrink the physical volume"))
    if topo.has_luks:
        ops.append(Operation(OpKind.BLOCKDEV_SHRINK, topo.luks.device,
                             container_target,
                             "Shrink the LUKS mapping"))

    # Partition last.  Align the partition size up to 1 MiB and keep it >= the
    # innermost container/filesystem size so nothing is ever truncated.
    part = topo.partition
    if part is not None:
        part_target = round_up(max(target_fs, container_target), ALIGN)
        ops.append(Operation(OpKind.PARTITION_SHRINK, part.device, part_target,
                             "Shrink the partition (never before the filesystem)"))
        ops.append(Operation(OpKind.VERIFY, part.device, part_target,
                             "Verify the partition and filesystem sizes"))
    return ops
