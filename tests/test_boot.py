"""Boot-environment file generation (sandboxed under a prefix, dry-run).

These never rebuild the host initramfs or touch the host bootloader: every
external command is dry-run and every file is written under a temp prefix.
"""

from __future__ import annotations

import pytest

from partresize.boot.bootloader import ENTRY_TITLE, Grub, SystemdBoot
from partresize.boot.generators import (
    Dracut,
    InitramfsTools,
    Mkinitcpio,
)

SCRIPT = "#!/bin/sh\necho hello\n"
BINS = ["e2fsck", "resize2fs", "sfdisk"]


def _prefix(tmp_path):
    return str(tmp_path) + "/"


class TestInitramfsBuilders:
    def test_initramfs_tools(self, tmp_path):
        b = InitramfsTools(prefix=_prefix(tmp_path), dry_run=True)
        res = b.build(SCRIPT, BINS, "op1")
        assert res.cmdline_marker == "partresize.maint=op1"
        premount = tmp_path / "etc/initramfs-tools/scripts/local-premount/partresize-op1"
        hook = tmp_path / "etc/initramfs-tools/hooks/partresize-op1"
        script = tmp_path / "etc/partresize/maint-op1.sh"
        assert premount.exists() and hook.exists() and script.exists()
        # premount must gate on the marker and not the normal boot
        assert "partresize.maint=op1" in premount.read_text()
        b.cleanup("op1")
        assert not premount.exists()

    def test_dracut(self, tmp_path):
        b = Dracut(prefix=_prefix(tmp_path), dry_run=True)
        res = b.build(SCRIPT, BINS, "op2")
        mod = tmp_path / "usr/lib/dracut/modules.d/99partresize-op2/module-setup.sh"
        assert mod.exists()
        assert "pre-mount" in mod.read_text()
        assert res.cmdline_marker == "partresize.maint=op2"
        b.cleanup("op2")
        assert not mod.exists()

    def test_mkinitcpio(self, tmp_path):
        b = Mkinitcpio(prefix=_prefix(tmp_path), dry_run=True)
        b.build(SCRIPT, BINS, "op3")
        install = tmp_path / "etc/initcpio/install/partresize-op3"
        assert install.exists()
        assert "add_binary" in install.read_text()
        b.cleanup("op3")
        assert not install.exists()


class TestBootloaders:
    def test_grub_snippet_and_oneshot(self, tmp_path):
        # make it look like a grub system
        (tmp_path / "boot/grub").mkdir(parents=True)
        g = Grub(prefix=_prefix(tmp_path), dry_run=True)
        g.add_and_schedule("/boot/partresize-maint-op1.img", "6.8.0-generic",
                           "partresize.maint=op1", "op1")
        snippet = tmp_path / "etc/grub.d/09_partresize_op1"
        assert snippet.exists()
        text = snippet.read_text()
        assert ENTRY_TITLE in text
        assert "partresize.maint=op1" in text
        g.cleanup("op1")
        assert not snippet.exists()

    def test_systemd_boot_entry(self, tmp_path):
        (tmp_path / "boot/loader/entries").mkdir(parents=True)
        s = SystemdBoot(prefix=_prefix(tmp_path), dry_run=True)
        s.add_and_schedule("/boot/partresize-maint-op1.img", "6.8.0",
                           "partresize.maint=op1", "op1")
        entry = tmp_path / "boot/loader/entries/partresize-op1.conf"
        assert entry.exists()
        assert "partresize.maint=op1" in entry.read_text()
        s.cleanup("op1")
        assert not entry.exists()


class TestDetection:
    def test_initramfs_detection_or_error(self):
        from partresize.boot.generators import detect_initramfs_builder
        from partresize.errors import BootEnvironmentError
        try:
            b = detect_initramfs_builder(dry_run=True)
            assert b.name in ("initramfs-tools", "dracut", "mkinitcpio")
        except BootEnvironmentError:
            pytest.skip("no initramfs generator on this host")
