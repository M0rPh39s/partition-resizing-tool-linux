"""Initramfs builders for the three common generators.

Each builder knows how to embed the maintenance script + the required binaries
into a *separate* initramfs image (never the system's real one) and how to make
that image run the script during the pre-mount stage — the point where the root
device exists but root is not yet mounted.

All filesystem writes go under ``prefix`` (``/`` in production, a temp dir in
tests) and every external command runs through :func:`run` with ``dry_run`` so
the test-suite never rebuilds the host initramfs.
"""

from __future__ import annotations

import logging
import os
from dataclasses import dataclass, field

from ..errors import BootEnvironmentError
from ..run import have, run

log = logging.getLogger("partresize.boot.initramfs")

MARKER = "partresize.maint"


@dataclass
class PrepareResult:
    image_path: str
    kernel_version: str
    cmdline_marker: str
    files_written: list[str] = field(default_factory=list)


def _kernel_version() -> str:
    res = run(["uname", "-r"], check=False)
    return res.stdout.strip() or "unknown"


class InitramfsBuilder:
    name = "base"

    def __init__(self, prefix: str = "/", dry_run: bool = False) -> None:
        self.prefix = prefix
        self.dry_run = dry_run

    @classmethod
    def available(cls, prefix: str = "/") -> bool:
        raise NotImplementedError

    def _p(self, path: str) -> str:
        return os.path.join(self.prefix, path.lstrip("/"))

    def _write(self, path: str, content: str, mode: int = 0o644) -> str:
        full = self._p(path)
        os.makedirs(os.path.dirname(full), exist_ok=True)
        with open(full, "w", encoding="utf-8") as fh:
            fh.write(content)
        os.chmod(full, mode)
        log.debug("wrote %s", full)
        return full

    def image_path(self, op_id: str) -> str:
        return f"/boot/partresize-maint-{op_id}.img"

    def build(self, script: str, binaries: list[str], op_id: str) -> PrepareResult:
        raise NotImplementedError

    def cleanup(self, op_id: str) -> None:
        img = self._p(self.image_path(op_id))
        if os.path.exists(img):
            os.remove(img)


class InitramfsTools(InitramfsBuilder):
    """Debian/Ubuntu initramfs-tools."""

    name = "initramfs-tools"

    @classmethod
    def available(cls, prefix: str = "/") -> bool:
        return have("update-initramfs") or os.path.isdir(
            os.path.join(prefix, "etc/initramfs-tools"))

    def build(self, script, binaries, op_id):
        written: list[str] = []
        # A boot script placed in the local-premount stage runs after the root
        # device node is created but before the root fs is mounted.
        premount = f"""#!/bin/sh
PREREQ=""
prereqs() {{ echo "$PREREQ"; }}
case "$1" in prereqs) prereqs; exit 0;; esac
# only act during a partresize maintenance boot
for x in $(cat /proc/cmdline); do
    case "$x" in
        {MARKER}={op_id}) /partresize/maint.sh; sync;
            echo "partresize: rebooting into normal system"; reboot -f;;
    esac
done
"""
        d = f"/etc/initramfs-tools/scripts/local-premount/partresize-{op_id}"
        written.append(self._write(d, premount, 0o755))

        # A hook copies our script + the required binaries into the image.
        copies = "\n".join(f'copy_exec /sbin/{b} /sbin 2>/dev/null || '
                           f'copy_exec /usr/sbin/{b} /sbin 2>/dev/null || '
                           f'copy_exec $(command -v {b} 2>/dev/null) /bin 2>/dev/null || true'
                           for b in binaries)
        hook = f"""#!/bin/sh
PREREQ=""
prereqs() {{ echo "$PREREQ"; }}
case "$1" in prereqs) prereqs; exit 0;; esac
. /usr/share/initramfs-tools/hook-functions
mkdir -p "${{DESTDIR}}/partresize"
cp /etc/partresize/maint-{op_id}.sh "${{DESTDIR}}/partresize/maint.sh"
chmod 755 "${{DESTDIR}}/partresize/maint.sh"
{copies}
"""
        h = f"/etc/initramfs-tools/hooks/partresize-{op_id}"
        written.append(self._write(h, hook, 0o755))
        written.append(self._write(f"/etc/partresize/maint-{op_id}.sh", script, 0o755))

        image = self.image_path(op_id)
        kver = _kernel_version()
        # Build a *dedicated* image, leaving the system initramfs untouched.
        run(
            ["update-initramfs", "-c", "-k", kver] if False else
            ["mkinitramfs", "-o", self._p(image), kver],
            dry_run=self.dry_run, check=False,
        )
        return PrepareResult(image, kver, f"{MARKER}={op_id}", written)

    def cleanup(self, op_id):
        super().cleanup(op_id)
        for path in (
            f"/etc/initramfs-tools/scripts/local-premount/partresize-{op_id}",
            f"/etc/initramfs-tools/hooks/partresize-{op_id}",
            f"/etc/partresize/maint-{op_id}.sh",
        ):
            full = self._p(path)
            if os.path.exists(full):
                os.remove(full)


class Dracut(InitramfsBuilder):
    """Fedora/RHEL/SUSE dracut."""

    name = "dracut"

    @classmethod
    def available(cls, prefix: str = "/") -> bool:
        return have("dracut")

    def build(self, script, binaries, op_id):
        written: list[str] = []
        modname = f"99partresize-{op_id}"
        moddir = f"/usr/lib/dracut/modules.d/{modname}"

        # module-setup.sh declares what to install and installs the hook.
        install_bins = " ".join(binaries)
        module_setup = f"""#!/bin/bash
check() {{ return 0; }}
depends() {{ echo "dm crypt lvm"; return 0; }}
install() {{
    inst_multiple {install_bins}
    inst_simple "$moddir/maint.sh" /partresize/maint.sh
    inst_hook pre-mount 99 "$moddir/run.sh"
}}
"""
        run_hook = f"""#!/bin/sh
for x in $(cat /proc/cmdline); do
    case "$x" in
        {MARKER}={op_id}) /partresize/maint.sh; sync; reboot -f;;
    esac
done
"""
        written.append(self._write(f"{moddir}/module-setup.sh", module_setup, 0o755))
        written.append(self._write(f"{moddir}/run.sh", run_hook, 0o755))
        written.append(self._write(f"{moddir}/maint.sh", script, 0o755))

        image = self.image_path(op_id)
        kver = _kernel_version()
        run(
            ["dracut", "--force", "--add", f"partresize-{op_id}",
             self._p(image), kver],
            dry_run=self.dry_run, check=False,
        )
        return PrepareResult(image, kver, f"{MARKER}={op_id}", written)

    def cleanup(self, op_id):
        super().cleanup(op_id)
        import shutil
        moddir = self._p(f"/usr/lib/dracut/modules.d/99partresize-{op_id}")
        if os.path.isdir(moddir):
            shutil.rmtree(moddir)


class Mkinitcpio(InitramfsBuilder):
    """Arch mkinitcpio."""

    name = "mkinitcpio"

    @classmethod
    def available(cls, prefix: str = "/") -> bool:
        return have("mkinitcpio")

    def build(self, script, binaries, op_id):
        written: list[str] = []
        hookname = f"partresize-{op_id}"
        binadd = "\n".join(f'    add_binary {b}' for b in binaries)
        install_file = f"""#!/bin/bash
build() {{
{binadd}
    add_file /etc/partresize/maint-{op_id}.sh /partresize/maint.sh 755
    add_runscript
}}
help() {{ echo "partresize maintenance resize hook"; }}
"""
        runtime = f"""#!/usr/bin/ash
run_hook() {{
    for x in $(cat /proc/cmdline); do
        case "$x" in
            {MARKER}={op_id}) /partresize/maint.sh; sync; reboot -f;;
        esac
    done
}}
"""
        written.append(self._write(f"/etc/initcpio/install/{hookname}", install_file, 0o644))
        written.append(self._write(f"/etc/initcpio/hooks/{hookname}", runtime, 0o644))
        written.append(self._write(f"/etc/partresize/maint-{op_id}.sh", script, 0o755))

        image = self.image_path(op_id)
        kver = _kernel_version()
        run(
            ["mkinitcpio", "-A", hookname, "-g", self._p(image)],
            dry_run=self.dry_run, check=False,
        )
        return PrepareResult(image, kver, f"{MARKER}={op_id}", written)

    def cleanup(self, op_id):
        super().cleanup(op_id)
        for path in (
            f"/etc/initcpio/install/partresize-{op_id}",
            f"/etc/initcpio/hooks/partresize-{op_id}",
            f"/etc/partresize/maint-{op_id}.sh",
        ):
            full = self._p(path)
            if os.path.exists(full):
                os.remove(full)


_BUILDERS = [InitramfsTools, Dracut, Mkinitcpio]


def detect_initramfs_builder(prefix: str = "/", dry_run: bool = False):
    for cls in _BUILDERS:
        try:
            if cls.available(prefix):
                return cls(prefix=prefix, dry_run=dry_run)
        except Exception:  # pragma: no cover
            continue
    raise BootEnvironmentError(
        "no supported initramfs generator found "
        "(need initramfs-tools, dracut or mkinitcpio)"
    )
