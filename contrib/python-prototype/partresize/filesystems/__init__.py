"""Per-filesystem resize drivers."""

from __future__ import annotations

from .base import FilesystemDriver, FsProbe
from .btrfs import BtrfsDriver
from .ext import ExtDriver
from .xfs import XfsDriver

_DRIVERS: dict[str, type[FilesystemDriver]] = {
    "ext2": ExtDriver,
    "ext3": ExtDriver,
    "ext4": ExtDriver,
    "btrfs": BtrfsDriver,
    "xfs": XfsDriver,
}


def driver_for(fstype: str) -> FilesystemDriver:
    fstype = (fstype or "").lower()
    cls = _DRIVERS.get(fstype)
    if cls is None:
        raise KeyError(f"no filesystem driver for {fstype!r}")
    return cls(fstype)


__all__ = [
    "FilesystemDriver",
    "FsProbe",
    "ExtDriver",
    "BtrfsDriver",
    "XfsDriver",
    "driver_for",
]
