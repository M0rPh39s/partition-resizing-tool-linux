"""ext2/3/4 driver built on e2fsprogs (dumpe2fs, e2fsck, resize2fs)."""

from __future__ import annotations

import logging

from ..errors import ExecutionError
from ..run import have, require, run
from .base import FilesystemDriver, FsProbe

log = logging.getLogger("partresize.fs.ext")


class ExtDriver(FilesystemDriver):
    def _dumpe2fs(self, device: str) -> dict[str, str]:
        require("dumpe2fs")
        res = run(["dumpe2fs", "-h", device], check=False)
        # dumpe2fs prints the superblock summary on stdout even when it warns.
        if not res.stdout:
            raise ExecutionError(f"dumpe2fs produced no output for {device}")
        info: dict[str, str] = {}
        for line in res.stdout.splitlines():
            if ":" in line:
                key, _, val = line.partition(":")
                info[key.strip()] = val.strip()
        return info

    def probe(self, device: str, mountpoint: str | None = None) -> FsProbe:
        info = self._dumpe2fs(device)
        block_size = int(info.get("Block size", "4096"))
        block_count = int(info.get("Block count", "0"))
        free_blocks = int(info.get("Free blocks", "0"))
        size = block_count * block_size
        used = (block_count - free_blocks) * block_size
        state = info.get("Filesystem state", "")
        clean = "clean" in state if state else None
        min_size = self.minimum_size(device, block_size)
        return FsProbe(
            fstype=self.fstype,
            size_bytes=size,
            used_bytes=used,
            block_size=block_size,
            min_size_bytes=min_size,
            clean=clean,
        )

    def minimum_size(self, device: str, block_size: int) -> int:
        """Smallest size (bytes) resize2fs believes the fs can shrink to."""
        require("resize2fs")
        res = run(["resize2fs", "-P", device], check=False)
        # Output: "Estimated minimum size of the filesystem: <blocks>"
        for line in res.stdout.splitlines():
            if "minimum size" in line.lower():
                try:
                    blocks = int(line.rsplit(":", 1)[1].strip())
                    return blocks * block_size
                except (ValueError, IndexError):
                    pass
        return 0

    def check(self, device: str, *, dry_run: bool = False) -> None:
        require("e2fsck")
        # -f force, -y answer yes.  Return codes: 0 clean, 1 fixed, 2 fixed
        # (reboot recommended); >=4 indicate uncorrected / operational errors.
        res = run(["e2fsck", "-f", "-y", device], check=False, dry_run=dry_run)
        if dry_run:
            return
        if res.returncode >= 4:
            raise ExecutionError(
                f"e2fsck reported uncorrected errors on {device} "
                f"(rc={res.returncode}):\n{res.stdout}\n{res.stderr}"
            )

    def _device_bytes(self, device: str) -> int:
        if have("blockdev"):
            res = run(["blockdev", "--getsize64", device], check=False)
            if res.ok and res.stdout.strip():
                return int(res.stdout.strip())
        return 0

    def _resize(self, device: str, target_bytes: int | None,
                block_size: int, *, dry_run: bool) -> None:
        require("resize2fs")
        args = ["resize2fs", device]
        if target_bytes is not None and not dry_run:
            dev_bytes = self._device_bytes(device)
            # If the target meets or exceeds the device capacity, omit the size
            # so resize2fs grows to exactly fill the device (an explicit block
            # count equal to capacity is rejected as "permission denied").
            if not (dev_bytes and target_bytes >= dev_bytes):
                args.append(str(target_bytes // block_size))
        elif target_bytes is not None:
            args.append(str(target_bytes // block_size))
        run(args, dry_run=dry_run)

    def grow_online(self, device, mountpoint, target_bytes, *, dry_run=False):
        # resize2fs grows a mounted ext4 in place; omitting the size fills the
        # device.  A target may be given to grow to a specific size.
        bs = self.probe(device).block_size
        self._resize(device, target_bytes, bs, dry_run=dry_run)

    def grow_offline(self, device, target_bytes, *, dry_run=False):
        self.check(device, dry_run=dry_run)
        bs = self.probe(device).block_size if not dry_run else 4096
        self._resize(device, target_bytes, bs, dry_run=dry_run)

    def shrink_offline(self, device, target_bytes, *, dry_run=False):
        # Must be unmounted.  Always fsck first, then shrink to the target.
        self.check(device, dry_run=dry_run)
        bs = 4096
        min_size = 0
        if not dry_run:
            probe = self.probe(device)
            bs = probe.block_size
            min_size = probe.min_size_bytes
            if min_size and target_bytes < min_size:
                raise ExecutionError(
                    f"target {target_bytes} bytes is below the filesystem "
                    f"minimum of {min_size} bytes; refusing to shrink"
                )
        self._resize(device, target_bytes, bs, dry_run=dry_run)
