"""The :class:`BootEnvironment` coordinator implementing the full lifecycle."""

from __future__ import annotations

import logging

from ..maintenance import generate_script, required_binaries
from ..model import ResizePlan
from ..run import have, run
from ..safety import test_mode
from .bootloader import detect_bootloader
from .generators import detect_initramfs_builder

log = logging.getLogger("partresize.boot")


class BootEnvironment:
    """Coordinates the detected initramfs builder and bootloader.

    Lifecycle: :meth:`detect` -> :meth:`prepare` -> :meth:`schedule` ->
    :meth:`boot`; and later :meth:`cleanup` / :meth:`recover`.
    """

    def __init__(self, prefix: str = "/", dry_run: bool = False) -> None:
        self.prefix = prefix
        self.dry_run = dry_run
        self.initramfs = detect_initramfs_builder(prefix=prefix, dry_run=dry_run)
        self.bootloader = detect_bootloader(prefix=prefix, dry_run=dry_run)
        self._prepared = None

    # -- lifecycle ----------------------------------------------------------
    def describe(self) -> str:
        return (f"initramfs generator: {self.initramfs.name}; "
                f"bootloader: {self.bootloader.name}")

    def prepare(self, plan: ResizePlan, op_id: str):
        """Build the maintenance image containing the resize script + tools."""
        script = generate_script(plan, op_id)
        binaries = required_binaries(plan)
        log.info("preparing maintenance image (%s)", self.describe())
        self._prepared = self.initramfs.build(script, binaries, op_id)
        return self._prepared

    def schedule(self, op_id: str) -> None:
        """Register the one-shot boot into the maintenance image."""
        if self._prepared is None:
            raise RuntimeError("prepare() must be called before schedule()")
        p = self._prepared
        self.bootloader.add_and_schedule(
            p.image_path, p.kernel_version, p.cmdline_marker, op_id
        )

    def boot(self) -> None:
        """Reboot the machine.  Never actually reboots in test mode/dry-run."""
        if test_mode() or self.dry_run:
            log.info("DRY-RUN/TEST: would reboot now")
            return
        if have("systemctl"):
            run(["systemctl", "reboot"], check=False)
        else:
            run(["reboot"], check=False)

    def cleanup(self, op_id: str) -> None:
        """Remove the temporary image, hook files and boot entry."""
        try:
            self.bootloader.cleanup(op_id)
        finally:
            self.initramfs.cleanup(op_id)

    def recover(self, op_id: str) -> None:
        """Undo a scheduled-but-not-completed maintenance boot."""
        self.cleanup(op_id)


def detect_boot_environment(prefix: str = "/", dry_run: bool = False) -> BootEnvironment:
    return BootEnvironment(prefix=prefix, dry_run=dry_run)
