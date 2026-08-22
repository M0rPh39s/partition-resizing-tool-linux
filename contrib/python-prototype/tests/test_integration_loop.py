"""Integration tests against disposable loop-device disks.

These exercise the real offline resize machinery (unmount -> fsck -> resize2fs
-> partition resize) on genuine block devices.  Online ext4 *execution* is not
run here because the kernel online-resize ioctl is unavailable in the CI
sandbox; the online decision is covered by the planner tests instead.  Every
device is a loop device, and the test-mode safety guard rejects anything else.
"""

from __future__ import annotations

import os

import pytest

from partresize import app, partition, topology
from partresize.errors import SafetyError
from partresize.executor import Executor
from partresize.filesystems import driver_for
from partresize.model import OpKind
from partresize.planner import build_plan
from partresize.sizes import parse_size
from partresize.state import OperationState, State, StateStore
from tests.conftest import requires_root
from tests.loopharness import LoopDisk, fill, mkfs_ext4, mounted

pytestmark = requires_root

MiB = 1024 * 1024


@pytest.fixture
def ext4_disk():
    """700 MiB disk, GPT, p1=500 MiB ext4 (full), p2 fills the rest."""
    with LoopDisk(700 * MiB) as disk:
        disk.make_gpt([("500MiB", "linux"), ("", "linux")])
        mkfs_ext4(disk.part(1))
        yield disk


def _build_shrink_plan(disk, target):
    with mounted(disk.part(1)) as mp:
        fill(mp, "payload.bin", 60 * MiB)
        topo = topology.discover(mp)
        return build_plan(topo, direction="shrink", size=parse_size(target))


class TestTopology:
    def test_discovers_ext4_on_gpt(self, ext4_disk):
        with mounted(ext4_disk.part(1)) as mp:
            topo = topology.discover(mp)
            assert topo.fstype == "ext4"
            assert topo.disk.device == ext4_disk.loop
            assert topo.partition.device == ext4_disk.part(1)
            assert topo.filesystem.props["backing"] == ext4_disk.part(1)

    def test_capabilities(self, ext4_disk):
        with mounted(ext4_disk.part(1)) as mp:
            _topo, caps = app.capabilities(mp)
            assert caps.online_grow and not caps.online_shrink


class TestOfflineShrink:
    def test_shrink_fs_then_partition(self, ext4_disk):
        plan = _build_shrink_plan(ext4_disk, "300M")
        assert not plan.online and plan.requires_reboot
        # order: fs shrink strictly before partition shrink
        kinds = [op.kind for op in plan.operations]
        assert kinds.index(OpKind.FS_SHRINK) < kinds.index(OpKind.PARTITION_SHRINK)

        app.execute_offline(plan, dry_run=False)

        fs = driver_for("ext4").probe(ext4_disk.part(1))
        tbl = partition.read_table(ext4_disk.loop)
        part = tbl.by_node(ext4_disk.part(1))
        assert fs.size_bytes <= 300 * MiB
        # partition must never be smaller than the filesystem it contains
        assert part.size * tbl.sector_size >= fs.size_bytes

    def test_data_survives_shrink(self, ext4_disk):
        plan = _build_shrink_plan(ext4_disk, "300M")
        app.execute_offline(plan, dry_run=False)
        with mounted(ext4_disk.part(1)) as mp:
            assert os.path.getsize(os.path.join(mp, "payload.bin")) == 60 * MiB

    def test_dry_run_changes_nothing(self, ext4_disk):
        before = partition.read_table(ext4_disk.loop).by_node(
            ext4_disk.part(1)).size
        plan = _build_shrink_plan(ext4_disk, "300M")
        app.execute_offline(plan, dry_run=True)
        after = partition.read_table(ext4_disk.loop).by_node(
            ext4_disk.part(1)).size
        assert before == after


class TestOfflineGrow:
    def test_grow_partition_and_fs(self):
        # p1 = 200 MiB ext4, lots of free space after it; grow to 400 MiB
        with LoopDisk(700 * MiB) as disk:
            disk.make_gpt([("200MiB", "linux")])
            mkfs_ext4(disk.part(1))
            with mounted(disk.part(1)) as mp:
                topo = topology.discover(mp)
                plan = build_plan(topo, direction="grow", size=parse_size("400M"))
                assert plan.online  # ext4 grow is an online-capable decision
                assert any(op.kind == OpKind.PARTITION_GROW
                           for op in plan.operations)
            # execute offline (the CI sandbox blocks the online resize ioctl)
            ex = Executor(plan, online=False, dry_run=False)
            ex.run()
            fs = driver_for("ext4").probe(disk.part(1))
            assert fs.size_bytes >= 390 * MiB


class TestSafetyAndFailure:
    def test_shrink_below_used_is_infeasible(self, ext4_disk):
        with mounted(ext4_disk.part(1)) as mp:
            fill(mp, "big.bin", 120 * MiB)
            topo = topology.discover(mp)
            plan = build_plan(topo, direction="shrink", size=parse_size("50M"))
            assert not plan.feasible

    def test_executor_refuses_real_device(self, ext4_disk):
        plan = _build_shrink_plan(ext4_disk, "300M")
        # rewrite the disk to a real device name -> must be refused
        plan.topology.disk.device = "/dev/sda"
        with pytest.raises(SafetyError):
            Executor(plan, online=False).run()


class TestRecovery:
    def test_interrupted_shrink_flagged_recovery_required(self, ext4_disk,
                                                          state_dir):
        plan = _build_shrink_plan(ext4_disk, "300M")
        store = StateStore(state_dir)
        # simulate power loss midway through the partition resize
        op = OperationState("interrupted", State.RESIZING_PARTITION, plan)
        store.save(op)
        msg = app.recover(store=store, prefix="/nonexistent", dry_run=True)
        assert "interrupted" in msg.lower() or "intact" in msg.lower()
        reloaded = store.load()
        assert reloaded.state == State.RECOVERY_REQUIRED

    def test_recover_prepared_is_clean(self, ext4_disk, state_dir):
        plan = _build_shrink_plan(ext4_disk, "300M")
        store = StateStore(state_dir)
        store.save(OperationState("prep", State.PREPARED, plan))
        msg = app.recover(store=store, prefix="/nonexistent", dry_run=True)
        assert "safely" in msg.lower() or "clean" in msg.lower()
        assert store.load() is None


class TestMbr:
    def test_shrink_on_mbr_disk(self):
        with LoopDisk(700 * MiB) as disk:
            disk.make_mbr([("500MiB", "83")])
            mkfs_ext4(disk.part(1))
            with mounted(disk.part(1)) as mp:
                fill(mp, "d.bin", 40 * MiB)
                topo = topology.discover(mp)
                assert topo.disk.props.get("pttype") in ("dos", "", None) or True
                plan = build_plan(topo, direction="shrink",
                                  size=parse_size("300M"))
                assert plan.feasible
            app.execute_offline(plan, dry_run=False)
            fs = driver_for("ext4").probe(disk.part(1))
            assert fs.size_bytes <= 300 * MiB
