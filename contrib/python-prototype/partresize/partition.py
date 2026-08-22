"""Partition-table geometry and resize operations.

Reading is done with ``sfdisk -J`` (stable JSON, sector units).  Writing a new
partition size is done with ``sfdisk -N`` which rewrites a single partition
entry while keeping its start offset — we NEVER move a partition's start as a
side effect of a resize.  Growing only ever consumes contiguous free space
immediately after the partition; if a following partition is in the way we
report it rather than overwrite it.
"""

from __future__ import annotations

import json
import logging
from dataclasses import dataclass

from .errors import ExecutionError, SafetyError
from .run import have, require, run

log = logging.getLogger("partresize.partition")


@dataclass
class Part:
    num: int
    node: str
    start: int          # first sector
    size: int           # size in sectors
    type: str = ""

    @property
    def end(self) -> int:
        return self.start + self.size - 1


@dataclass
class Table:
    disk: str
    label: str          # "gpt" | "dos"
    sector_size: int
    total_sectors: int
    first_usable: int
    last_usable: int
    partitions: list[Part]

    def by_node(self, node: str) -> Part | None:
        for p in self.partitions:
            if p.node == node:
                return p
        return None

    def by_num(self, num: int) -> Part | None:
        for p in self.partitions:
            if p.num == num:
                return p
        return None


def _disk_sectors(disk: str, sector_size: int) -> int:
    if have("blockdev"):
        res = run(["blockdev", "--getsz", disk], check=False)
        if res.ok and res.stdout.strip():
            # --getsz returns size in 512-byte sectors
            sz512 = int(res.stdout.strip())
            return sz512 * 512 // sector_size
    return 0


def read_table(disk: str) -> Table:
    require("sfdisk")
    res = run(["sfdisk", "-J", disk])
    data = json.loads(res.stdout)["partitiontable"]
    label = data.get("label", "")
    sector_size = int(data.get("sectorsize", 512))
    parts: list[Part] = []
    for entry in data.get("partitions", []):
        node = entry["node"]
        num = _node_num(node)
        parts.append(
            Part(
                num=num,
                node=node,
                start=int(entry["start"]),
                size=int(entry["size"]),
                type=str(entry.get("type", "")),
            )
        )
    parts.sort(key=lambda p: p.start)
    total = _disk_sectors(disk, sector_size)
    first_usable = int(data.get("firstlba", 2048))
    # For GPT, sfdisk reports lastlba; for MBR fall back to total-1.
    last_usable = int(data.get("lastlba", (total - 1) if total else 0))
    return Table(
        disk=disk,
        label=label,
        sector_size=sector_size,
        total_sectors=total,
        first_usable=first_usable,
        last_usable=last_usable,
        partitions=parts,
    )


def _node_num(node: str) -> int:
    digits = ""
    for ch in reversed(node):
        if ch.isdigit():
            digits = ch + digits
        else:
            break
    return int(digits) if digits else 0


@dataclass
class FreeSpace:
    """Free space analysis immediately after a partition."""

    free_sectors: int
    blocking_partition: Part | None  # the partition that limits growth, if any
    max_end_sector: int                 # furthest sector the part could grow to


def free_after(table: Table, part: Part) -> FreeSpace:
    """Analyse contiguous free space immediately following ``part``."""
    following = [p for p in table.partitions if p.start > part.start]
    following.sort(key=lambda p: p.start)
    if following:
        nxt = following[0]
        gap = nxt.start - (part.end + 1)
        if gap > 0:
            # free gap then a partition: growth is limited to the gap.
            return FreeSpace(gap, nxt, nxt.start - 1)
        # partition immediately abuts: no room, nxt is blocking.
        return FreeSpace(0, nxt, part.end)
    # nothing after: free space up to last usable sector.
    limit = table.last_usable if table.last_usable else (table.total_sectors - 1)
    free = limit - part.end
    return FreeSpace(max(free, 0), None, limit)


def max_grow_bytes(table: Table, part: Part) -> int:
    fs = free_after(table, part)
    return fs.free_sectors * table.sector_size


def set_partition_size(
    disk: str, num: int, new_size_sectors: int, *, dry_run: bool = False
) -> None:
    """Set partition ``num`` to ``new_size_sectors`` keeping its start offset."""
    require("sfdisk")
    if new_size_sectors <= 0:
        raise SafetyError("refusing to set a non-positive partition size")
    # sfdisk -N N applies a partial spec; blank start keeps the existing start.
    spec = f"size={new_size_sectors}\n"
    log.info("resize partition %s#%d -> %d sectors", disk, num, new_size_sectors)
    res = run(
        ["sfdisk", "--no-reread", "-N", str(num), disk],
        input_text=spec,
        check=False,
        dry_run=dry_run,
    )
    if dry_run:
        return
    if not res.ok:
        raise ExecutionError(
            f"sfdisk failed to resize partition {num} on {disk}:\n"
            f"{res.stdout}\n{res.stderr}"
        )
    reread(disk, dry_run=dry_run)


def reread(disk: str, *, dry_run: bool = False) -> None:
    """Ask the kernel to re-read the partition table."""
    if have("partprobe"):
        run(["partprobe", disk], check=False, dry_run=dry_run)
    elif have("partx"):
        run(["partx", "-u", disk], check=False, dry_run=dry_run)
    elif have("blockdev"):
        run(["blockdev", "--rereadpt", disk], check=False, dry_run=dry_run)


def backup_table(disk: str, dest_path: str, *, dry_run: bool = False) -> None:
    """Write a restorable dump of the partition table to ``dest_path``."""
    require("sfdisk")
    if dry_run:
        log.info("DRY-RUN: sfdisk -d %s > %s", disk, dest_path)
        return
    res = run(["sfdisk", "-d", disk])
    with open(dest_path, "w", encoding="utf-8") as fh:
        fh.write(res.stdout)
    log.info("partition table backup written to %s", dest_path)
