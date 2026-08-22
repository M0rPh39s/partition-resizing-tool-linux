"""Planner tests using synthetic topologies (no real devices)."""

from __future__ import annotations

import pytest

from partresize import partition as part_mod
from partresize import planner
from partresize.model import Layer, LayerKind, OpKind, Topology
from partresize.partition import Part, Table

MiB = 1024 * 1024
GiB = 1024 * 1024 * 1024


def make_topo(fstype="ext4", part_size=120 * GiB, disk_size=200 * GiB,
              with_lvm=False, with_luks=False, part_dev="/dev/nvme0n1p3"):
    layers = [Layer(LayerKind.DISK, "/dev/nvme0n1", disk_size,
                    props={"pttype": "gpt", "log_sec": 512})]
    layers.append(Layer(LayerKind.PARTITION, part_dev, part_size,
                        props={"start": 2048, "partn": 3}))
    if with_luks:
        layers.append(Layer(LayerKind.LUKS, "/dev/mapper/cryptroot", part_size))
    if with_lvm:
        layers.append(Layer(LayerKind.LVM_VG, "vg0", part_size,
                            props={"pv": part_dev, "vg_free": 0}))
        layers.append(Layer(LayerKind.LVM_LV, "/dev/mapper/vg0-root", part_size,
                            props={"vg": "vg0"}))
    bottom = layers[-1].device
    layers.append(Layer(LayerKind.FILESYSTEM, bottom, part_size,
                        props={"fstype": fstype, "used": 42 * GiB,
                               "backing": bottom}))
    return Topology("/", layers)


@pytest.fixture
def patch_measure(monkeypatch):
    def fake(topo):
        return topo.filesystem.size, 42 * GiB, 4096, 10 * GiB
    monkeypatch.setattr(planner, "_measure", fake)


def _fake_table(part_size_sectors, disk_sectors, following=None):
    parts = [Part(3, "/dev/nvme0n1p3", 2048, part_size_sectors, "linux")]
    if following is not None:
        parts.append(Part(4, "/dev/nvme0n1p4", 2048 + part_size_sectors,
                          following, "linux"))
    return Table("/dev/nvme0n1", "gpt", 512, disk_sectors, 2048,
                 disk_sectors - 34, parts)


class TestShrink:
    def test_ext4_shrink_is_offline_and_ordered(self, patch_measure):
        topo = make_topo(part_size=120 * GiB)
        plan = planner.build_plan(topo, direction="shrink", size=80 * GiB)
        assert plan.online is False
        assert plan.requires_reboot is True
        assert plan.feasible
        kinds = [op.kind for op in plan.operations]
        # fsck must come first, fs shrink before partition shrink
        assert kinds[0] == OpKind.FSCK
        i_fs = kinds.index(OpKind.FS_SHRINK)
        i_part = kinds.index(OpKind.PARTITION_SHRINK)
        assert i_fs < i_part, "filesystem must be shrunk before the partition"

    def test_xfs_shrink_blocked(self, patch_measure):
        topo = make_topo(fstype="xfs")
        plan = planner.build_plan(topo, direction="shrink", size=80 * GiB)
        assert not plan.feasible
        assert any("shrunk" in b or "shrink" in b for b in plan.blockers)

    def test_shrink_below_used_blocked(self, patch_measure):
        topo = make_topo()
        plan = planner.build_plan(topo, direction="shrink", size=10 * GiB)
        assert not plan.feasible

    def test_lvm_shrink_orders_lv_before_partition(self, patch_measure):
        topo = make_topo(with_lvm=True)
        plan = planner.build_plan(topo, direction="shrink", size=80 * GiB)
        kinds = [op.kind for op in plan.operations]
        i_fs = kinds.index(OpKind.FS_SHRINK)
        i_bd = kinds.index(OpKind.BLOCKDEV_SHRINK)
        i_part = kinds.index(OpKind.PARTITION_SHRINK)
        assert i_fs < i_bd < i_part

    def test_luks_shrink_has_blockdev_step(self, patch_measure):
        topo = make_topo(with_luks=True)
        plan = planner.build_plan(topo, direction="shrink", size=80 * GiB)
        assert any(op.kind == OpKind.BLOCKDEV_SHRINK for op in plan.operations)


class TestGrow:
    def test_grow_within_partition_is_online(self, patch_measure, monkeypatch):
        # fs 80 GiB inside a 120 GiB partition, grow to 100 GiB: no part change
        topo = make_topo(part_size=120 * GiB)
        topo.filesystem.size = 80 * GiB
        monkeypatch.setattr(planner, "_measure",
                            lambda t: (80 * GiB, 42 * GiB, 4096, 10 * GiB))
        plan = planner.build_plan(topo, direction="grow", size=100 * GiB)
        assert plan.feasible
        assert plan.online is True
        assert not any(op.kind == OpKind.PARTITION_GROW for op in plan.operations)

    def test_grow_needs_partition_and_space_available(self, patch_measure,
                                                      monkeypatch):
        topo = make_topo(part_size=120 * GiB, disk_size=200 * GiB)
        # free space after partition, no following partition
        monkeypatch.setattr(part_mod, "read_table",
                            lambda d: _fake_table(120 * GiB // 512,
                                                  200 * GiB // 512))
        plan = planner.build_plan(topo, direction="grow", size=150 * GiB)
        assert plan.feasible
        assert any(op.kind == OpKind.PARTITION_GROW for op in plan.operations)

    def test_grow_blocked_by_following_partition(self, patch_measure,
                                                monkeypatch):
        topo = make_topo(part_size=120 * GiB, disk_size=200 * GiB)
        # a following partition immediately abuts p3 -> move required
        monkeypatch.setattr(
            part_mod, "read_table",
            lambda d: _fake_table(120 * GiB // 512, 200 * GiB // 512,
                                  following=40 * GiB // 512),
        )
        plan = planner.build_plan(topo, direction="grow", size=150 * GiB)
        assert not plan.feasible
        assert any("occupies" in b or "Moving" in b for b in plan.blockers)

    def test_resize_direction_inference(self, patch_measure):
        topo = make_topo(part_size=120 * GiB)
        # resize down -> shrink
        p = planner.build_plan(topo, direction="resize", size=80 * GiB)
        assert p.direction == "shrink"
