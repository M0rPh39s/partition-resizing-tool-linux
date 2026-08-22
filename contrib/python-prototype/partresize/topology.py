"""Discovery of the complete storage stack behind a mount point.

The running root filesystem may sit on a chain such as::

    disk -> partition -> LUKS -> LVM LV -> ext4

or simply::

    disk -> partition -> ext4

This module walks that chain from the filesystem down to the physical disk
using ``findmnt`` and ``lsblk`` (both present on essentially every Linux
system) and enriches it with LVM/partition geometry where the relevant tools
are available.
"""

from __future__ import annotations

import json
import logging
import os
from typing import Any

from .errors import TopologyError
from .model import Layer, LayerKind, Topology
from .run import have, run

log = logging.getLogger("partresize.topology")

# lsblk columns we rely on.  Kept explicit (rather than -O) so parsing is
# stable across lsblk versions.
_LSBLK_COLS = "NAME,KNAME,PATH,TYPE,FSTYPE,SIZE,FSSIZE,FSUSED,START,PARTN,PTTYPE,MOUNTPOINT,PKNAME,LOG-SEC"


def _lsblk_tree() -> list[dict[str, Any]]:
    res = run(["lsblk", "-J", "-b", "-o", _LSBLK_COLS])
    data = json.loads(res.stdout)
    return data.get("blockdevices", [])


def _resolve_source(mountpoint: str) -> str:
    """Return the canonical block device backing ``mountpoint``."""
    res = run(
        ["findmnt", "-n", "-o", "SOURCE", "--target", mountpoint], check=False
    )
    src = res.stdout.strip().splitlines()
    if not src or not src[0]:
        raise TopologyError(f"could not determine device for {mountpoint!r}")
    source = src[0]
    # findmnt may return e.g. /dev/mapper/vg-root ; canonicalise symlinks
    real = os.path.realpath(source)
    return real if real.startswith("/dev/") else source


def _find_path_to(
    nodes: list[dict[str, Any]], predicate, trail: list | None = None
) -> list[dict[str, Any]] | None:
    """DFS returning the ancestor chain [disk, ..., matched] or None."""
    trail = trail or []
    for node in nodes:
        chain = trail + [node]
        if predicate(node):
            return chain
        children = node.get("children") or []
        found = _find_path_to(children, predicate, chain)
        if found:
            return found
    return None


def _match_device(node: dict[str, Any], target: str) -> bool:
    path = node.get("path")
    if path and os.path.realpath(path) == target:
        return True
    kname = node.get("kname")
    if kname and os.path.realpath(f"/dev/{kname}") == target:
        return True
    return False


def _lvm_pv_vg(lv_device: str) -> tuple[str | None, str | None, int]:
    """Return (pv_device, vg_name, vg_free_bytes) for an LV, best effort."""
    if not have("lvs"):
        return None, None, 0
    vg = None
    res = run(
        ["lvs", "--noheadings", "-o", "vg_name", lv_device], check=False
    )
    if res.ok and res.stdout.strip():
        vg = res.stdout.strip()
    pv = None
    free = 0
    if vg and have("vgs"):
        vres = run(
            ["vgs", "--noheadings", "--units", "b", "--nosuffix",
             "-o", "vg_free", vg],
            check=False,
        )
        if vres.ok and vres.stdout.strip():
            try:
                free = int(vres.stdout.strip())
            except ValueError:
                free = 0
    if vg and have("pvs"):
        pres = run(
            ["pvs", "--noheadings", "-o", "pv_name", "-S", f"vg_name={vg}"],
            check=False,
        )
        if pres.ok and pres.stdout.strip():
            pv = pres.stdout.strip().splitlines()[0].strip()
    return pv, vg, free


def _resolve_fstype(device: str, mountpoint: str | None) -> str:
    """Determine a filesystem type via findmnt (if mounted) then blkid.

    lsblk's FSTYPE column depends on a populated blkid cache which is not
    always present, so we never rely on it alone for the bottom layer.
    """
    if mountpoint:
        res = run(["findmnt", "-n", "-o", "FSTYPE", "--target", mountpoint],
                  check=False)
        if res.ok and res.stdout.strip():
            return res.stdout.strip().splitlines()[0]
    if have("blkid"):
        res = run(["blkid", "-o", "value", "-s", "TYPE", device], check=False)
        if res.ok and res.stdout.strip():
            return res.stdout.strip()
    return ""


def _classify(node: dict[str, Any], is_top: bool, is_bottom: bool,
              bottom_fstype: str = "") -> list[Layer]:
    """Turn one lsblk node into one or more topology layers."""
    ntype = node.get("type")
    path = node.get("path") or f"/dev/{node.get('kname')}"
    size = int(node.get("size") or 0)
    fstype = node.get("fstype") or ""
    if is_bottom and not fstype:
        fstype = bottom_fstype
    layers: list[Layer] = []

    if ntype in ("disk", "loop") and is_top:
        layers.append(
            Layer(
                LayerKind.DISK,
                path,
                size,
                props={
                    "pttype": node.get("pttype") or "",
                    "log_sec": int(node.get("log-sec") or 512),
                },
            )
        )
    elif ntype == "part":
        layers.append(
            Layer(
                LayerKind.PARTITION,
                path,
                size,
                props={
                    "partn": node.get("partn"),
                    "start": int(node.get("start") or 0),
                    "pttype": node.get("pttype") or "",
                },
            )
        )
    elif ntype == "crypt":
        layers.append(Layer(LayerKind.LUKS, path, size, props={}))
    elif ntype == "lvm":
        pv, vg, vg_free = _lvm_pv_vg(path)
        if vg:
            layers.append(
                Layer(
                    LayerKind.LVM_VG,
                    vg,
                    size,
                    props={"pv": pv or "", "vg_free": vg_free},
                )
            )
        layers.append(
            Layer(
                LayerKind.LVM_LV,
                path,
                size,
                props={"vg": vg or "", "vg_free": vg_free},
            )
        )
    else:
        # Unknown intermediate node kind; represent generically so the plan can
        # refuse rather than silently mishandle it.
        layers.append(
            Layer(LayerKind.PARTITION, path, size, props={"raw_type": ntype})
        )

    if is_bottom and fstype:
        fs_size = node.get("fssize")
        fs_used = node.get("fsused")
        layers.append(
            Layer(
                LayerKind.FILESYSTEM,
                path,
                int(fs_size) if fs_size else size,
                props={
                    "fstype": fstype,
                    "used": int(fs_used) if fs_used else 0,
                    "backing": path,
                },
            )
        )
    return layers


def discover(mountpoint: str) -> Topology:
    """Discover the full storage topology backing ``mountpoint``."""
    source = _resolve_source(mountpoint)
    tree = _lsblk_tree()
    chain = _find_path_to(tree, lambda n: _match_device(n, source))
    if not chain:
        raise TopologyError(
            f"could not locate {source} ({mountpoint}) in the block device tree"
        )

    bottom_node = chain[-1]
    bottom_device = bottom_node.get("path") or f"/dev/{bottom_node.get('kname')}"
    bottom_fstype = _resolve_fstype(bottom_device, mountpoint)

    layers: list[Layer] = []
    for i, node in enumerate(chain):
        is_top = i == 0
        is_bottom = i == len(chain) - 1
        layers.extend(_classify(node, is_top, is_bottom, bottom_fstype))

    topo = Topology(mountpoint=mountpoint, layers=layers)
    # sanity: must have a disk at top and filesystem at bottom
    if topo.layers[0].kind != LayerKind.DISK:
        raise TopologyError("could not resolve the physical disk for the stack")
    if topo.layers[-1].kind != LayerKind.FILESYSTEM:
        raise TopologyError(
            f"no filesystem detected on {source}; unable to plan a resize"
        )
    return topo
