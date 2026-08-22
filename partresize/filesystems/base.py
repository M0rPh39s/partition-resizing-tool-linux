"""Filesystem driver interface."""

from __future__ import annotations

from dataclasses import dataclass


@dataclass
class FsProbe:
    """The measurable state of a filesystem."""

    fstype: str
    size_bytes: int          # current filesystem size
    used_bytes: int          # bytes in use
    block_size: int          # fundamental block size
    min_size_bytes: int      # smallest size the fs could be shrunk to
    clean: bool | None = None  # None if unknown


class FilesystemDriver:
    """Base class.  Subclasses implement the per-filesystem primitives.

    Methods that mutate state take ``dry_run`` and simply log under dry-run.
    ``grow_online`` / ``shrink_offline`` are the two primary operations; a
    driver that does not support one raises :class:`NotImplementedError` (the
    capability matrix is expected to have prevented the call).
    """

    def __init__(self, fstype: str) -> None:
        self.fstype = fstype

    # ---- inspection -------------------------------------------------------
    def probe(self, device: str, mountpoint: str | None = None) -> FsProbe:
        raise NotImplementedError

    # ---- integrity --------------------------------------------------------
    def check(self, device: str, *, dry_run: bool = False) -> None:
        """Run a filesystem consistency check (device must be unmounted)."""
        raise NotImplementedError

    # ---- resize primitives -----------------------------------------------
    def grow_online(
        self, device: str, mountpoint: str, target_bytes: int | None,
        *, dry_run: bool = False,
    ) -> None:
        raise NotImplementedError(f"{self.fstype}: online grow not supported")

    def grow_offline(
        self, device: str, target_bytes: int | None, *, dry_run: bool = False
    ) -> None:
        raise NotImplementedError(f"{self.fstype}: offline grow not supported")

    def shrink_offline(
        self, device: str, target_bytes: int, *, dry_run: bool = False
    ) -> None:
        raise NotImplementedError(f"{self.fstype}: offline shrink not supported")
