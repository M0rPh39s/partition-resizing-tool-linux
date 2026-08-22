"""Bootloader integration: GRUB and systemd-boot.

Both add a *one-time* boot entry that points at the maintenance initramfs and
carries the ``partresize.maint=<id>`` marker on the kernel command line.  One
time means the very next boot uses it and every subsequent boot returns to the
normal default — so even a crash inside maintenance cannot trap the machine in
the maintenance environment.
"""

from __future__ import annotations

import logging
import os

from ..errors import BootEnvironmentError
from ..run import have, run

log = logging.getLogger("partresize.boot.loader")

ENTRY_TITLE = "PartResize Maintenance"


class Bootloader:
    name = "base"

    def __init__(self, prefix: str = "/", dry_run: bool = False) -> None:
        self.prefix = prefix
        self.dry_run = dry_run

    @classmethod
    def available(cls, prefix: str = "/") -> bool:
        raise NotImplementedError

    def _p(self, path: str) -> str:
        return os.path.join(self.prefix, path.lstrip("/"))

    def add_and_schedule(self, image_path: str, kernel_version: str,
                         cmdline_marker: str, op_id: str) -> None:
        raise NotImplementedError

    def cleanup(self, op_id: str) -> None:
        raise NotImplementedError


class Grub(Bootloader):
    name = "grub"

    @classmethod
    def available(cls, prefix: str = "/") -> bool:
        def j(p):
            return os.path.join(prefix, p.lstrip("/"))
        return (
            have("grub-reboot") or have("grub2-reboot")
            or os.path.isdir(j("/boot/grub")) or os.path.isdir(j("/boot/grub2"))
        )

    def _snippet_path(self, op_id: str) -> str:
        return f"/etc/grub.d/09_partresize_{op_id}"

    def add_and_schedule(self, image_path, kernel_version, cmdline_marker, op_id):
        vmlinuz = f"/boot/vmlinuz-{kernel_version}"
        # A custom grub.d snippet emits a dedicated menuentry.
        snippet = f"""#!/bin/sh
exec tail -n +3 $0
menuentry '{ENTRY_TITLE}' --id partresize-{op_id} {{
    load_video
    insmod gzio
    insmod part_gpt
    insmod ext2
    linux {vmlinuz} root=/dev/ram0 {cmdline_marker} ro
    initrd {image_path}
}}
"""
        path = self._p(self._snippet_path(op_id))
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w", encoding="utf-8") as fh:
            fh.write(snippet)
        os.chmod(path, 0o755)

        mkconfig = "grub-mkconfig" if have("grub-mkconfig") else "grub2-mkconfig"
        cfg = "/boot/grub/grub.cfg" if os.path.isdir(self._p("/boot/grub")) \
            else "/boot/grub2/grub.cfg"
        run([mkconfig, "-o", self._p(cfg)], dry_run=self.dry_run, check=False)

        # grub-reboot sets a *one-shot* default for the next boot only.
        reboot_tool = "grub-reboot" if have("grub-reboot") else "grub2-reboot"
        run([reboot_tool, f"partresize-{op_id}"], dry_run=self.dry_run, check=False)

    def cleanup(self, op_id):
        path = self._p(self._snippet_path(op_id))
        if os.path.exists(path):
            os.remove(path)
        mkconfig = "grub-mkconfig" if have("grub-mkconfig") else "grub2-mkconfig"
        cfg = "/boot/grub/grub.cfg" if os.path.isdir(self._p("/boot/grub")) \
            else "/boot/grub2/grub.cfg"
        run([mkconfig, "-o", self._p(cfg)], dry_run=self.dry_run, check=False)


class SystemdBoot(Bootloader):
    name = "systemd-boot"

    @classmethod
    def available(cls, prefix: str = "/") -> bool:
        def j(p):
            return os.path.join(prefix, p.lstrip("/"))
        if prefix in ("/", "") and have("bootctl"):
            res = run(["bootctl", "is-installed"], check=False)
            if res.ok and "yes" in res.stdout.lower():
                return True
        return os.path.isdir(j("/boot/loader/entries")) or \
            os.path.isdir(j("/efi/loader/entries"))

    def _esp(self) -> str:
        for cand in ("/boot", "/efi", "/boot/efi"):
            if os.path.isdir(self._p(os.path.join(cand, "loader"))):
                return cand
        return "/boot"

    def _entry_path(self, op_id: str) -> str:
        return os.path.join(self._esp(), "loader", "entries",
                            f"partresize-{op_id}.conf")

    def add_and_schedule(self, image_path, kernel_version, cmdline_marker, op_id):
        entry = f"""title   {ENTRY_TITLE}
linux   /vmlinuz-{kernel_version}
initrd  {image_path.replace('/boot/', '/')}
options root=/dev/ram0 {cmdline_marker} ro
"""
        path = self._p(self._entry_path(op_id))
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w", encoding="utf-8") as fh:
            fh.write(entry)
        # bootctl set-oneshot arranges a single next-boot into this entry.
        run(["bootctl", "set-oneshot", f"partresize-{op_id}.conf"],
            dry_run=self.dry_run, check=False)

    def cleanup(self, op_id):
        path = self._p(self._entry_path(op_id))
        if os.path.exists(path):
            os.remove(path)


_LOADERS = [Grub, SystemdBoot]


def detect_bootloader(prefix: str = "/", dry_run: bool = False) -> Bootloader:
    for cls in _LOADERS:
        try:
            if cls.available(prefix):
                return cls(prefix=prefix, dry_run=dry_run)
        except Exception:  # pragma: no cover
            continue
    raise BootEnvironmentError(
        "no supported bootloader found (need GRUB or systemd-boot)"
    )
