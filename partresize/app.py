"""High-level orchestration used by the CLI.

Each function corresponds to a user-facing verb: inspect, capabilities, plan,
resize, cancel, status, recover.  ``plan`` NEVER mutates anything.
"""

from __future__ import annotations

import logging
import time
from dataclasses import dataclass

from . import topology as topo_mod
from .boot import detect_boot_environment
from .capabilities import capabilities_for
from .errors import PartresizeError
from .executor import Executor
from .filesystems import driver_for
from .model import Capabilities, ResizePlan, Topology
from .planner import build_plan
from .state import (
    ABORTABLE,
    DANGEROUS,
    OperationState,
    State,
    StateStore,
)

log = logging.getLogger("partresize.app")


def _new_op_id() -> str:
    return time.strftime("%Y%m%d-%H%M%S")


@dataclass
class InspectResult:
    topology: Topology
    fs_size: int
    used: int
    block_size: int
    fs_min: int


def inspect(mountpoint: str) -> InspectResult:
    topo = topo_mod.discover(mountpoint)
    driver = driver_for(topo.fstype)
    fs = topo.filesystem
    device = fs.props.get("backing", fs.device)
    try:
        probe = driver.probe(device, mountpoint=mountpoint)
        return InspectResult(topo, probe.size_bytes, probe.used_bytes,
                             probe.block_size, probe.min_size_bytes)
    except Exception as exc:
        log.debug("probe failed: %s", exc)
        return InspectResult(topo, fs.size, fs.props.get("used", 0), 4096, 0)


def capabilities(mountpoint: str) -> tuple[Topology, Capabilities]:
    topo = topo_mod.discover(mountpoint)
    return topo, capabilities_for(topo.fstype)


def plan(mountpoint: str, *, direction: str, size: int | None = None,
         by: int | None = None) -> ResizePlan:
    topo = topo_mod.discover(mountpoint)
    return build_plan(topo, direction=direction, size=size, by=by)


def _resize_online(plan_obj: ResizePlan, store: StateStore, dry_run: bool) -> None:
    op = OperationState(_new_op_id(), State.PREPARED, plan_obj)
    if not dry_run:
        store.save(op)
    ex = Executor(plan_obj, online=True, dry_run=dry_run,
                  on_state=(None if dry_run else
                            (lambda st, _op: store.transition(op, st))))
    ex.run()
    if not dry_run:
        store.transition(op, State.COMPLETED)
        store.clear()


def _resize_offline(plan_obj: ResizePlan, store: StateStore, dry_run: bool,
                    prefix: str) -> str:
    op_id = _new_op_id()
    op = OperationState(op_id, State.PREPARED, plan_obj)
    if not dry_run:
        store.save(op)
    boot = detect_boot_environment(prefix=prefix, dry_run=dry_run)
    boot.prepare(plan_obj, op_id)
    boot.schedule(op_id)
    if not dry_run:
        store.transition(op, State.BOOTING_MAINTENANCE)
    return op_id


def execute_offline(plan_obj: ResizePlan, *, dry_run: bool = False,
                    store: StateStore | None = None) -> None:
    """Run the offline plan directly (used inside maintenance and by tests)."""
    op = None
    if store is not None:
        op = store.load() or OperationState(_new_op_id(), State.PREPARED, plan_obj)

    def cb(st: State, _op):
        if store is not None and op is not None:
            store.transition(op, st)

    ex = Executor(plan_obj, online=False, dry_run=dry_run, on_state=cb)
    try:
        ex.run()
    except Exception as exc:
        if store is not None and op is not None:
            store.transition(op, State.FAILED, error=str(exc))
        raise
    if store is not None and op is not None:
        store.transition(op, State.COMPLETED)


def status(store: StateStore | None = None) -> OperationState | None:
    store = store or StateStore()
    return store.load()


def cancel(store: StateStore | None = None, prefix: str = "/",
           dry_run: bool = False) -> str:
    store = store or StateStore()
    op = store.load()
    if op is None:
        return "no operation in progress"
    if op.state not in ABORTABLE:
        raise PartresizeError(
            f"operation is in state {op.state.value} and cannot be cancelled "
            "safely; use 'recover' instead"
        )
    try:
        boot = detect_boot_environment(prefix=prefix, dry_run=dry_run)
        boot.recover(op.op_id)
    except PartresizeError as exc:
        log.warning("boot cleanup during cancel: %s", exc)
    store.clear()
    return f"cancelled operation {op.op_id}"


def recover(store: StateStore | None = None, prefix: str = "/",
            dry_run: bool = False) -> str:
    store = store or StateStore()
    op = store.load()
    if op is None:
        return "no operation state found; nothing to recover"
    # Always tidy the temporary boot entry/image so we never re-enter
    # maintenance unintentionally.
    try:
        boot = detect_boot_environment(prefix=prefix, dry_run=dry_run)
        boot.cleanup(op.op_id)
    except PartresizeError as exc:
        log.warning("boot cleanup during recover: %s", exc)

    if op.state in (State.COMPLETED,):
        store.clear()
        return f"operation {op.op_id} already completed; cleaned up"
    if op.state in ABORTABLE:
        store.clear()
        return (f"operation {op.op_id} had not started resizing; "
                "cleaned up safely")
    if op.state in DANGEROUS:
        store.transition(op, State.RECOVERY_REQUIRED,
                         error="interrupted during a destructive step")
        return (f"operation {op.op_id} was interrupted during "
                f"'{op.state.value}'. The filesystem was shrunk before the "
                "partition, so data is intact. Re-run the resize to complete "
                "the partition step, or inspect the layout manually.")
    if op.state == State.VERIFYING:
        store.transition(op, State.COMPLETED)
        store.clear()
        return f"operation {op.op_id} completed the resize; cleaned up"
    return f"operation {op.op_id} is in state {op.state.value}"
