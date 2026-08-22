"""Execute the operations of a :class:`ResizePlan`.

The same executor drives both the online path (Mode 1, filesystem mounted) and
the offline maintenance path (Mode 2, filesystem unmounted).  A callback lets
the caller record state-machine transitions after each step so an interrupted
run can be recovered.
"""

from __future__ import annotations

import logging
from typing import Callable

from . import blockdev
from . import partition as part_mod
from .errors import ExecutionError
from .filesystems import driver_for
from .model import Operation, OpKind, ResizePlan
from .run import have, run
from .safety import assert_test_device
from .state import State

log = logging.getLogger("partresize.executor")

# Map operation kinds to the state that should be recorded before running them.
_OP_STATE = {
    OpKind.FSCK: State.CHECKING_FILESYSTEM,
    OpKind.FS_SHRINK: State.SHRINKING_FILESYSTEM,
    OpKind.FS_GROW: State.GROWING_FILESYSTEM,
    OpKind.BLOCKDEV_SHRINK: State.RESIZING_BLOCK_DEVICE,
    OpKind.BLOCKDEV_GROW: State.RESIZING_BLOCK_DEVICE,
    OpKind.PARTITION_SHRINK: State.RESIZING_PARTITION,
    OpKind.PARTITION_GROW: State.RESIZING_PARTITION,
    OpKind.VERIFY: State.VERIFYING,
}

StateCallback = Callable[[State, Operation], None]


class Executor:
    def __init__(
        self,
        plan: ResizePlan,
        *,
        online: bool | None = None,
        dry_run: bool = False,
        on_state: StateCallback | None = None,
    ) -> None:
        self.plan = plan
        self.topo = plan.topology
        self.online = plan.online if online is None else online
        self.dry_run = dry_run
        self.on_state = on_state
        self.driver = driver_for(self.topo.fstype)

    # -- helpers ------------------------------------------------------------
    def _guard(self, device: str) -> None:
        # In test-mode, refuse to touch any non-test device.
        assert_test_device(device)
        assert_test_device(self.topo.disk.device)

    def _emit(self, op: Operation) -> None:
        if self.on_state:
            self.on_state(_OP_STATE.get(op.kind, State.VERIFYING), op)

    # -- public -------------------------------------------------------------
    def run(self) -> None:
        for op in self.plan.operations:
            self._guard(op.device)
            self._emit(op)
            log.info("[%s] %s", op.kind.value, op.description)
            self._dispatch(op)

    # -- dispatch -----------------------------------------------------------
    def _dispatch(self, op: Operation) -> None:
        handler = {
            OpKind.FSCK: self._fsck,
            OpKind.FS_GROW: self._fs_grow,
            OpKind.FS_SHRINK: self._fs_shrink,
            OpKind.BLOCKDEV_GROW: self._blockdev_grow,
            OpKind.BLOCKDEV_SHRINK: self._blockdev_shrink,
            OpKind.PARTITION_GROW: self._partition_grow,
            OpKind.PARTITION_SHRINK: self._partition_shrink,
            OpKind.VERIFY: self._verify,
        }[op.kind]
        handler(op)

    def _fs_device(self) -> str:
        fs = self.topo.filesystem
        return fs.props.get("backing", fs.device)

    def _fsck(self, op: Operation) -> None:
        self.driver.check(op.device, dry_run=self.dry_run)

    def _fs_grow(self, op: Operation) -> None:
        if self.online:
            self.driver.grow_online(
                op.device, self.topo.mountpoint, op.target_size,
                dry_run=self.dry_run,
            )
        else:
            self.driver.grow_offline(op.device, op.target_size, dry_run=self.dry_run)

    def _fs_shrink(self, op: Operation) -> None:
        if op.target_size is None:
            raise ExecutionError("filesystem shrink requires a target size")
        self.driver.shrink_offline(op.device, op.target_size, dry_run=self.dry_run)

    def _blockdev_grow(self, op: Operation) -> None:
        if not op.device:
            return
        if self.topo.has_luks and op.device == self.topo.luks.device:
            blockdev.luks_grow(op.device, dry_run=self.dry_run)
        elif op.device.startswith("/dev/") and self._is_pv(op.device):
            blockdev.pv_resize(op.device, dry_run=self.dry_run)
        elif self.topo.has_lvm and op.device == self.topo.lvm_lv.device:
            blockdev.lv_resize(op.device, op.target_size, dry_run=self.dry_run)

    def _blockdev_shrink(self, op: Operation) -> None:
        if not op.device or op.target_size is None:
            return
        if self.topo.has_luks and op.device == self.topo.luks.device:
            blockdev.luks_shrink(op.device, op.target_size, dry_run=self.dry_run)
        elif self._is_pv(op.device):
            blockdev.pv_resize(op.device, op.target_size, dry_run=self.dry_run)
        elif self.topo.has_lvm and op.device == self.topo.lvm_lv.device:
            blockdev.lv_resize(op.device, op.target_size, dry_run=self.dry_run)

    def _is_pv(self, device: str) -> bool:
        vg = next(
            (x for x in self.topo.layers
             if x.kind.value == "lvm-vg" and x.props.get("pv") == device),
            None,
        )
        return vg is not None

    def _partition_grow(self, op: Operation) -> None:
        table = part_mod.read_table(self.topo.disk.device)
        p = table.by_node(op.device)
        if p is None:
            raise ExecutionError(f"partition {op.device} not found")
        free = part_mod.free_after(table, p)
        new_sectors = p.size + free.free_sectors
        # align down to 1 MiB
        align = (1024 * 1024) // table.sector_size
        new_sectors = (new_sectors // align) * align
        if new_sectors <= p.size:
            log.info("partition already at maximum size; nothing to grow")
            return
        part_mod.set_partition_size(
            self.topo.disk.device, p.num, new_sectors, dry_run=self.dry_run
        )

    def _partition_shrink(self, op: Operation) -> None:
        if op.target_size is None:
            raise ExecutionError("partition shrink requires a target size")
        table = part_mod.read_table(self.topo.disk.device)
        p = table.by_node(op.device)
        if p is None:
            raise ExecutionError(f"partition {op.device} not found")
        new_sectors = op.target_size // table.sector_size
        # Never shrink the partition below the filesystem/container size.
        if new_sectors >= p.size:
            log.info("partition already <= target; not shrinking")
            return
        part_mod.set_partition_size(
            self.topo.disk.device, p.num, new_sectors, dry_run=self.dry_run
        )

    def _verify(self, op: Operation) -> None:
        if self.dry_run:
            return
        # Re-read the table and confirm the partition still contains the fs.
        if have("udevadm"):
            run(["udevadm", "settle"], check=False)
        log.info("verification step for %s complete", op.device)
