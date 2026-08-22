"""Btrfs driver.  Btrfs resizes (grow and shrink) on a *mounted* filesystem.

For offline work the maintenance environment mounts the filesystem at a
scratch mountpoint (never as the system root) and resizes it there.
"""

from __future__ import annotations

import logging
import os
import tempfile

from ..errors import ExecutionError
from ..run import require, run
from .base import FilesystemDriver, FsProbe

log = logging.getLogger("partresize.fs.btrfs")


class BtrfsDriver(FilesystemDriver):
    def probe(self, device: str, mountpoint: str | None = None) -> FsProbe:
        require("btrfs")
        mp = mountpoint
        tmp = None
        try:
            if mp is None:
                tmp = tempfile.mkdtemp(prefix="partresize-btrfs-")
                run(["mount", "-o", "ro", device, tmp])
                mp = tmp
            res = run(["btrfs", "filesystem", "usage", "-b", mp])
            size = used = 0
            for line in res.stdout.splitlines():
                s = line.strip()
                if s.startswith("Device size:"):
                    size = int(s.split(":")[1].strip())
                elif s.startswith("Used:"):
                    used = int(s.split(":")[1].strip())
            return FsProbe(self.fstype, size, used, 4096, used, clean=True)
        finally:
            if tmp is not None:
                run(["umount", tmp], check=False)
                os.rmdir(tmp)

    def check(self, device: str, *, dry_run: bool = False) -> None:
        require("btrfs")
        res = run(["btrfs", "check", device], check=False, dry_run=dry_run)
        if not dry_run and res.returncode != 0:
            raise ExecutionError(f"btrfs check failed on {device}:\n{res.stdout}")

    def _resize_mounted(self, mp: str, target_bytes: int | None, *, dry_run):
        require("btrfs")
        size = "max" if target_bytes is None else str(target_bytes)
        run(["btrfs", "filesystem", "resize", size, mp], dry_run=dry_run)

    def grow_online(self, device, mountpoint, target_bytes, *, dry_run=False):
        self._resize_mounted(mountpoint, target_bytes, dry_run=dry_run)

    def _offline_via_mount(self, device, target_bytes, *, dry_run):
        if dry_run:
            self._resize_mounted("<scratch>", target_bytes, dry_run=True)
            return
        tmp = tempfile.mkdtemp(prefix="partresize-btrfs-")
        try:
            run(["mount", device, tmp])
            self._resize_mounted(tmp, target_bytes, dry_run=False)
        finally:
            run(["umount", tmp], check=False)
            os.rmdir(tmp)

    def grow_offline(self, device, target_bytes, *, dry_run=False):
        self._offline_via_mount(device, target_bytes, dry_run=dry_run)

    def shrink_offline(self, device, target_bytes, *, dry_run=False):
        self._offline_via_mount(device, target_bytes, dry_run=dry_run)
