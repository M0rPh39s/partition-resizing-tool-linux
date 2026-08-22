"""Distribution-independent reboot-assisted maintenance environment.

``BootEnvironment`` is the abstraction with the lifecycle from the spec::

    detect() -> prepare() -> schedule() -> boot()
                                   ... reboot into initramfs ...
                cleanup() / recover()

It composes two detected pieces:

* an :class:`~partresize.boot.generators.InitramfsBuilder`
  (initramfs-tools / dracut / mkinitcpio), and
* a :class:`~partresize.boot.bootloader.Bootloader` (GRUB / systemd-boot).
"""

from __future__ import annotations

from .base import BootEnvironment, detect_boot_environment
from .bootloader import Bootloader, Grub, SystemdBoot, detect_bootloader
from .generators import (
    Dracut,
    InitramfsBuilder,
    InitramfsTools,
    Mkinitcpio,
    detect_initramfs_builder,
)

__all__ = [
    "BootEnvironment",
    "detect_boot_environment",
    "InitramfsBuilder",
    "InitramfsTools",
    "Dracut",
    "Mkinitcpio",
    "detect_initramfs_builder",
    "Bootloader",
    "Grub",
    "SystemdBoot",
    "detect_bootloader",
]
