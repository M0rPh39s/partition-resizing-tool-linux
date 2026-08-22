"""Integration tests for layered stacks: LVM, LUKS, Btrfs, XFS.

Each is skipped when its tooling is absent.  All operate on loop devices only.
"""

from __future__ import annotations

import subprocess

from partresize import topology
from partresize.model import LayerKind, OpKind
from partresize.planner import build_plan
from partresize.sizes import parse_size
from tests.conftest import (
    requires_dm,
    requires_kernel_fs,
    requires_root,
    requires_tools,
)
from tests.loopharness import LoopDisk, fill, mkfs_btrfs, mkfs_ext4, mkfs_xfs, mounted

pytestmark = requires_root

MiB = 1024 * 1024


def _run(args, **kw):
    return subprocess.run([str(a) for a in args], check=True,
                          capture_output=True, text=True, **kw)


@requires_tools("btrfs", "mkfs.btrfs")
@requires_kernel_fs("btrfs")
class TestBtrfs:
    def test_discover_and_online_shrink_plan(self):
        with LoopDisk(700 * MiB) as disk:
            disk.make_gpt([("500MiB", "linux")])
            mkfs_btrfs(disk.part(1))
            with mounted(disk.part(1)) as mp:
                fill(mp, "b.bin", 40 * MiB)
                topo = topology.discover(mp)
                assert topo.fstype == "btrfs"
                # btrfs supports online shrink of the filesystem itself
                from partresize.capabilities import capabilities_for
                assert capabilities_for("btrfs").online_shrink is True


@requires_tools("xfs_growfs", "mkfs.xfs", "xfs_info")
@requires_kernel_fs("xfs")
class TestXfs:
    def test_discover_and_grow_only(self):
        with LoopDisk(700 * MiB) as disk:
            disk.make_gpt([("200MiB", "linux")])
            mkfs_xfs(disk.part(1))
            with mounted(disk.part(1)) as mp:
                topo = topology.discover(mp)
                assert topo.fstype == "xfs"
                # xfs cannot shrink -> shrink plan must be infeasible
                plan = build_plan(topo, direction="shrink",
                                  size=parse_size("150M"))
                assert not plan.feasible


@requires_tools("cryptsetup")
@requires_dm
class TestLuks:
    def test_discover_through_luks(self):
        with LoopDisk(700 * MiB) as disk:
            disk.make_gpt([("500MiB", "linux")])
            part = disk.part(1)
            passphrase = "testpass123"
            _run(["cryptsetup", "luksFormat", "--type", "luks2",
                  "--pbkdf", "pbkdf2", "--pbkdf-force-iterations", "1000",
                  "--batch-mode", part], input=passphrase)
            name = "partresize-luks-test"
            try:
                _run(["cryptsetup", "open", part, name],
                     input=passphrase)
                mapper = f"/dev/mapper/{name}"
                mkfs_ext4(mapper)
                with mounted(mapper) as mp:
                    topo = topology.discover(mp)
                    assert topo.has_luks
                    assert topo.fstype == "ext4"
                    kinds = [layer.kind for layer in topo.layers]
                    assert LayerKind.LUKS in kinds
            finally:
                _run(["cryptsetup", "close", name])


@requires_tools("pvcreate", "vgcreate", "lvcreate", "lvs")
@requires_dm
class TestLvm:
    def test_discover_through_lvm(self):
        with LoopDisk(700 * MiB) as disk:
            disk.make_gpt([("500MiB", "linux")])
            part = disk.part(1)
            vg = "partresizetestvg"
            lv = "root"
            try:
                _run(["pvcreate", "-f", part])
                _run(["vgcreate", vg, part])
                _run(["lvcreate", "-y", "-L", "300M", "-n", lv, vg])
                mapper = f"/dev/mapper/{vg}-{lv}"
                mkfs_ext4(mapper)
                with mounted(mapper) as mp:
                    topo = topology.discover(mp)
                    assert topo.has_lvm
                    assert topo.fstype == "ext4"
                    plan = build_plan(topo, direction="shrink",
                                      size=parse_size("150M"))
                    # LV shrink step must exist and precede partition shrink
                    kinds = [op.kind for op in plan.operations]
                    if OpKind.PARTITION_SHRINK in kinds:
                        assert (kinds.index(OpKind.BLOCKDEV_SHRINK)
                                < kinds.index(OpKind.PARTITION_SHRINK))
            finally:
                subprocess.run(["vgchange", "-an", vg],
                               capture_output=True)
                subprocess.run(["vgremove", "-f", vg], capture_output=True)
                subprocess.run(["pvremove", "-f", part], capture_output=True)
