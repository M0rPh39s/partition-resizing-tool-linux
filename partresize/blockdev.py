"""Resize operations for the intermediate block-device layers: LVM and LUKS.

These sit between the partition and the filesystem.  When growing, the order is
partition -> LUKS -> PV/LV -> filesystem (each container first).  When
shrinking, the order is reversed (filesystem first, then containers, then
partition).  This module provides the container primitives; the planner is
responsible for ordering.
"""

from __future__ import annotations

import logging

from .run import have, require, run

log = logging.getLogger("partresize.blockdev")


# ---- LUKS -----------------------------------------------------------------

def luks_grow(mapper_device: str, *, dry_run: bool = False) -> None:
    """Grow a LUKS mapping to fill its backing device (no size shrink risk)."""
    require("cryptsetup")
    run(["cryptsetup", "resize", mapper_device], dry_run=dry_run)


def luks_shrink(mapper_device: str, target_bytes: int, *, dry_run: bool = False) -> None:
    """Shrink a LUKS mapping to ``target_bytes`` (rounded to 512-byte sectors)."""
    require("cryptsetup")
    sectors = target_bytes // 512
    run(
        ["cryptsetup", "resize", "--size", str(sectors), mapper_device],
        dry_run=dry_run,
    )


# ---- LVM ------------------------------------------------------------------

def pv_resize(pv_device: str, target_bytes: int | None = None,
              *, dry_run: bool = False) -> None:
    require("pvresize")
    args = ["pvresize"]
    if target_bytes is not None:
        args += ["--setphysicalvolumesize", f"{target_bytes}b"]
    args.append(pv_device)
    run(args, dry_run=dry_run)


def lv_resize(lv_device: str, target_bytes: int, *, dry_run: bool = False) -> None:
    """Set an LV to an absolute size.  Filesystem is resized separately."""
    require("lvresize")
    run(
        ["lvresize", "-f", "--size", f"{target_bytes}b", lv_device],
        dry_run=dry_run,
    )


def vg_backup(vg_name: str, dest_path: str, *, dry_run: bool = False) -> None:
    if not have("vgcfgbackup"):
        return
    run(["vgcfgbackup", "-f", dest_path, vg_name], check=False, dry_run=dry_run)


def lv_min_from_fs(fs_size_bytes: int, extent_bytes: int = 4 * 1024 * 1024) -> int:
    """Round an LV target up to a whole number of extents (default 4 MiB)."""
    extents = (fs_size_bytes + extent_bytes - 1) // extent_bytes
    return extents * extent_bytes
