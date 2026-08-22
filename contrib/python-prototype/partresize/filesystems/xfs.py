"""XFS driver.  XFS can only *grow*, and only while mounted."""

from __future__ import annotations

import logging
import os
import tempfile

from ..errors import ExecutionError
from ..run import require, run
from .base import FilesystemDriver, FsProbe

log = logging.getLogger("partresize.fs.xfs")


class XfsDriver(FilesystemDriver):
    def probe(self, device: str, mountpoint: str | None = None) -> FsProbe:
        require("xfs_info")
        mp = mountpoint
        tmp = None
        try:
            if mp is None:
                tmp = tempfile.mkdtemp(prefix="partresize-xfs-")
                run(["mount", device, tmp])
                mp = tmp
            res = run(["xfs_info", mp])
            bsize = 4096
            blocks = 0
            for tok in res.stdout.split():
                if tok.startswith("bsize="):
                    bsize = int(tok.split("=")[1].rstrip(","))
                if tok.startswith("blocks="):
                    try:
                        blocks = int(tok.split("=")[1].rstrip(","))
                    except ValueError:
                        pass
            size = bsize * blocks
            return FsProbe(self.fstype, size, size, bsize, size, clean=True)
        finally:
            if tmp is not None:
                run(["umount", tmp], check=False)
                os.rmdir(tmp)

    def check(self, device: str, *, dry_run: bool = False) -> None:
        require("xfs_repair")
        res = run(["xfs_repair", "-n", device], check=False, dry_run=dry_run)
        if not dry_run and res.returncode != 0:
            raise ExecutionError(f"xfs_repair reported problems on {device}")

    def grow_online(self, device, mountpoint, target_bytes, *, dry_run=False):
        require("xfs_growfs")
        # xfs_growfs only grows to fill the device; a smaller target is not
        # possible, so target_bytes is advisory only.
        run(["xfs_growfs", mountpoint], dry_run=dry_run)
