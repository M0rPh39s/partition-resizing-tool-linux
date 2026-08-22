"""Safety guards shared across the tool and, critically, the test-suite.

Two independent concerns live here:

1. **Size sanity** — a shrink target must comfortably exceed the used data.
2. **Test-device whitelisting** — when ``PARTRESIZE_TEST_MODE`` is set, every
   destructive operation must target a loop / device-mapper test device.  Any
   attempt to touch a real host disk (``/dev/sda``, ``/dev/vda``,
   ``/dev/nvme*``, ...) is refused outright so the integration tests can never
   damage the machine they run on.
"""

from __future__ import annotations

import os
import re

from .errors import SafetyError

# Minimum free head-room to keep above used data when shrinking a filesystem.
DEFAULT_SHRINK_MARGIN = 0.10          # 10 %
# At least this many bytes, whichever is larger.  Overridable (mainly so the
# integration tests can shrink small disposable images tightly); production use
# keeps the conservative 256 MiB default.
MIN_ABSOLUTE_MARGIN = 256 * 1024 * 1024


def _min_absolute_margin() -> int:
    override = os.environ.get("PARTRESIZE_MIN_MARGIN")
    if override:
        try:
            return int(override)
        except ValueError:
            pass
    return MIN_ABSOLUTE_MARGIN

# Devices that are always permissible test targets.
_TEST_DEVICE_RE = re.compile(r"^/dev/(loop\d+|mapper/partresize[\w-]*|dm-\d+)")
# Real disks we must never touch in test mode.
_REAL_DEVICE_RE = re.compile(r"^/dev/(sd[a-z]|vd[a-z]|nvme\d+n\d+|hd[a-z]|mmcblk\d+)")


def test_mode() -> bool:
    return os.environ.get("PARTRESIZE_TEST_MODE") == "1"


def assert_test_device(device: str) -> None:
    """In test mode, reject anything that is not an explicit test device."""
    if not test_mode():
        return
    base = _base_device(device)
    if _REAL_DEVICE_RE.match(base) or _REAL_DEVICE_RE.match(device):
        raise SafetyError(
            f"refusing to operate on real host device {device!r} in test mode"
        )
    if not (_TEST_DEVICE_RE.match(device) or _TEST_DEVICE_RE.match(base)):
        raise SafetyError(
            f"device {device!r} is not a recognised disposable test device; "
            "integration tests may only touch loop/dm test devices"
        )


def _base_device(device: str) -> str:
    """Map a partition node to its parent disk (best effort, string-only)."""
    m = re.match(r"^(/dev/loop\d+)p?\d+$", device)
    if m:
        return m.group(1)
    m = re.match(r"^(/dev/nvme\d+n\d+)p\d+$", device)
    if m:
        return m.group(1)
    m = re.match(r"^(/dev/(?:sd[a-z]|vd[a-z]|hd[a-z]))\d+$", device)
    if m:
        return m.group(1)
    return device


def shrink_margin(used_bytes: int) -> int:
    return max(int(used_bytes * DEFAULT_SHRINK_MARGIN), _min_absolute_margin())


def assert_shrink_safe(target_fs_bytes: int, used_bytes: int,
                       min_fs_bytes: int = 0) -> None:
    """Verify a shrink target leaves enough room above used data."""
    margin = shrink_margin(used_bytes)
    floor = used_bytes + margin
    if target_fs_bytes < used_bytes:
        raise SafetyError(
            f"target size {target_fs_bytes} is smaller than used data "
            f"{used_bytes}; this would destroy data"
        )
    if target_fs_bytes < floor:
        raise SafetyError(
            f"target size {target_fs_bytes} leaves less than the required "
            f"safety margin above used data ({floor} needed)"
        )
    if min_fs_bytes and target_fs_bytes < min_fs_bytes:
        raise SafetyError(
            f"target size {target_fs_bytes} is below the filesystem minimum "
            f"of {min_fs_bytes} bytes"
        )
