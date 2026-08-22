"""Shared pytest fixtures and guards.

The whole suite runs with ``PARTRESIZE_TEST_MODE=1`` so the device-safety guard
is armed: any attempt to touch a real host disk raises immediately.
"""

from __future__ import annotations

import os
import shutil

import pytest

os.environ.setdefault("PARTRESIZE_TEST_MODE", "1")
# Allow tight shrinks on small disposable images.
os.environ.setdefault("PARTRESIZE_MIN_MARGIN", str(16 * 1024 * 1024))


def _is_root() -> bool:
    return os.geteuid() == 0


def _have(*tools: str) -> bool:
    return all(shutil.which(t) for t in tools)


requires_root = pytest.mark.skipif(
    not _is_root() or not _have("losetup", "sfdisk"),
    reason="needs root + loop devices + sfdisk",
)


def requires_tools(*tools: str):
    return pytest.mark.skipif(
        not _have(*tools), reason=f"needs tools: {', '.join(tools)}"
    )


def _kernel_fs(fstype: str) -> bool:
    """True if the running kernel can mount ``fstype``."""
    try:
        with open("/proc/filesystems") as fh:
            supported = {line.split()[-1] for line in fh if line.strip()}
    except OSError:
        return False
    return fstype in supported


def _dm_available() -> bool:
    """True if the kernel device-mapper driver is usable (LVM/LUKS)."""
    import subprocess
    if not shutil.which("dmsetup"):
        return False
    res = subprocess.run(["dmsetup", "version"], capture_output=True, text=True)
    return res.returncode == 0 and "kernel" in res.stdout.lower()


def requires_kernel_fs(fstype: str):
    return pytest.mark.skipif(
        not _kernel_fs(fstype),
        reason=f"kernel does not support the {fstype} filesystem",
    )


requires_dm = pytest.mark.skipif(
    not _dm_available(),
    reason="kernel device-mapper (LVM/LUKS) not available",
)


@pytest.fixture
def state_dir(tmp_path, monkeypatch):
    d = tmp_path / "state"
    d.mkdir()
    monkeypatch.setenv("PARTRESIZE_STATE_DIR", str(d))
    return str(d)
