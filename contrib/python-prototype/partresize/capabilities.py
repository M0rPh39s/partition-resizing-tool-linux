"""The online/offline grow/shrink capability matrix per filesystem.

This is deliberately conservative.  The guiding rule from the specification:

    ext4:  online grow YES, online shrink NO

We never claim an online shrink for a filesystem that does not truly support
it.  When in doubt the answer is "offline".
"""

from __future__ import annotations

from .model import Capabilities

# fstype -> (online_grow, online_shrink, offline_grow, offline_shrink, notes)
_MATRIX: dict[str, tuple[bool, bool, bool, bool, list[str]]] = {
    # ext family: online grow supported by resize2fs on a mounted fs; shrink
    # must be done offline (resize2fs refuses to shrink a mounted fs).
    "ext2": (True, False, True, True, []),
    "ext3": (True, False, True, True, []),
    "ext4": (
        True,
        False,
        True,
        True,
        ["online grow supported; shrink requires the filesystem to be unmounted"],
    ),
    # Btrfs can grow AND shrink online via `btrfs filesystem resize`.
    "btrfs": (
        True,
        True,
        True,
        True,
        ["btrfs supports online grow and online shrink"],
    ),
    # XFS can only ever grow, and only while mounted.  It cannot shrink at all.
    "xfs": (
        True,
        False,
        False,
        False,
        ["xfs can only grow, and only while mounted; it cannot be shrunk"],
    ),
    # f2fs: online grow only.
    "f2fs": (True, False, True, False, ["f2fs cannot be shrunk"]),
}


def capabilities_for(fstype: str) -> Capabilities:
    fstype = (fstype or "").lower()
    if fstype in _MATRIX:
        og, os_, ofg, ofs, notes = _MATRIX[fstype]
        return Capabilities(fstype, og, os_, ofg, ofs, list(notes))
    return Capabilities(
        fstype,
        online_grow=False,
        online_shrink=False,
        offline_grow=False,
        offline_shrink=False,
        notes=[f"filesystem {fstype!r} is not supported for resizing"],
    )
