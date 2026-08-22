"""Disposable loop-device test harness.

Creates real ``disk.img`` files, attaches them as loop devices, partitions
them and lays down filesystems — so the resize logic runs against genuine
block devices without ever touching a real disk.

Every device produced here is a ``/dev/loop*`` (or ``/dev/mapper/partresize-*``)
device, which is exactly what :func:`partresize.safety.assert_test_device`
permits in test mode.
"""

from __future__ import annotations

import contextlib
import os
import subprocess
import tempfile
import time
from collections.abc import Iterator


def _run(args, **kw):
    return subprocess.run([str(a) for a in args], check=True,
                          capture_output=True, text=True, **kw)


def _run_ok(args):
    return subprocess.run([str(a) for a in args], capture_output=True, text=True)


class LoopDisk:
    """A disposable disk image attached to a loop device with partition scan."""

    def __init__(self, size_bytes: int, sector_size: int = 512) -> None:
        self.size_bytes = size_bytes
        self.sector_size = sector_size
        self.path = tempfile.mktemp(prefix="partresize-disk-", suffix=".img")
        self.loop: str | None = None

    def __enter__(self) -> LoopDisk:
        with open(self.path, "wb") as fh:
            fh.truncate(self.size_bytes)
        res = _run(["losetup", "--show", "-f", "-P", self.path])
        self.loop = res.stdout.strip()
        return self

    def __exit__(self, *exc) -> None:
        self.detach()

    def detach(self) -> None:
        if self.loop:
            _run_ok(["losetup", "-d", self.loop])
            self.loop = None
        with contextlib.suppress(FileNotFoundError):
            os.remove(self.path)

    # -- partitioning -------------------------------------------------------
    def make_gpt(self, partitions: list[tuple[str, str]]) -> None:
        """Create a GPT table.  ``partitions`` is a list of (size, type).

        ``size`` uses sfdisk syntax (e.g. ``100MiB`` or ``+`` for the rest).
        """
        lines = ["label: gpt", ""]
        for size, ptype in partitions:
            spec = f"type={ptype}"
            if size:
                spec = f"size={size}, " + spec
            lines.append(spec)
        script = "\n".join(lines) + "\n"
        _run(["sfdisk", self.loop], input=script)
        self._settle()

    def make_mbr(self, partitions: list[tuple[str, str]]) -> None:
        lines = ["label: dos", ""]
        for size, ptype in partitions:
            spec = f"type={ptype}"
            if size:
                spec = f"size={size}, " + spec
            lines.append(spec)
        _run(["sfdisk", self.loop], input="\n".join(lines) + "\n")
        self._settle()

    def part(self, num: int) -> str:
        return f"{self.loop}p{num}"

    def _settle(self) -> None:
        _run_ok(["partprobe", self.loop])
        _run_ok(["udevadm", "settle"])
        # wait for partition device nodes to appear
        for _ in range(50):
            if os.path.exists(self.part(1)):
                return
            time.sleep(0.05)


# -- filesystem creation ----------------------------------------------------

def mkfs_ext4(device: str, size: str | None = None) -> None:
    args = ["mkfs.ext4", "-F", "-q", device]
    if size:
        args.append(size)
    _run(args)


def mkfs_btrfs(device: str) -> None:
    _run(["mkfs.btrfs", "-f", "-q", device])


def mkfs_xfs(device: str) -> None:
    _run(["mkfs.xfs", "-f", "-q", device])


@contextlib.contextmanager
def mounted(device: str, options: list[str] | None = None) -> Iterator[str]:
    mp = tempfile.mkdtemp(prefix="partresize-mnt-")
    args = ["mount"]
    if options:
        args += options
    args += [device, mp]
    _run(args)
    try:
        yield mp
    finally:
        _run_ok(["umount", mp])
        with contextlib.suppress(OSError):
            os.rmdir(mp)


def fill(mp: str, name: str, size_bytes: int) -> None:
    """Write a file of ``size_bytes`` into a mounted filesystem."""
    with open(os.path.join(mp, name), "wb") as fh:
        remaining = size_bytes
        chunk = b"\0" * (1024 * 1024)
        while remaining > 0:
            n = min(len(chunk), remaining)
            fh.write(chunk[:n])
            remaining -= n
        fh.flush()
        os.fsync(fh.fileno())


def have_tool(tool: str) -> bool:
    from shutil import which
    return which(tool) is not None
