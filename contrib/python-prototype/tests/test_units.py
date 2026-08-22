"""Pure-Python unit tests (no devices, no root)."""

from __future__ import annotations

import pytest

from partresize.capabilities import capabilities_for
from partresize.errors import SafetyError
from partresize.safety import (
    assert_shrink_safe,
    assert_test_device,
)
from partresize.safety import test_mode as _test_mode
from partresize.sizes import format_size, parse_size, round_down, round_up


class TestSizes:
    @pytest.mark.parametrize("text,expected", [
        ("80G", 80 * 1024**3),
        ("80GiB", 80 * 1024**3),
        ("1000MB", 1000 * 1000**2),
        ("512", 512),
        ("1.5G", int(1.5 * 1024**3)),
        ("80g", 80 * 1024**3),
    ])
    def test_parse(self, text, expected):
        assert parse_size(text) == expected

    def test_parse_invalid(self):
        with pytest.raises(ValueError):
            parse_size("banana")
        with pytest.raises(ValueError):
            parse_size("10ZB")

    def test_format_roundish(self):
        assert format_size(80 * 1024**3) == "80.00 GiB"
        assert format_size(0) == "0 B"
        assert format_size(1024) == "1.00 KiB"

    def test_rounding(self):
        assert round_down(1000, 256) == 768
        assert round_up(1000, 256) == 1024
        assert round_up(1024, 256) == 1024


class TestCapabilities:
    def test_ext4_matrix(self):
        caps = capabilities_for("ext4")
        assert caps.online_grow is True
        assert caps.online_shrink is False  # the crucial rule
        assert caps.offline_shrink is True

    def test_xfs_cannot_shrink(self):
        caps = capabilities_for("xfs")
        assert caps.online_grow is True
        assert caps.offline_shrink is False
        assert caps.online_shrink is False

    def test_btrfs_online_shrink(self):
        caps = capabilities_for("btrfs")
        assert caps.online_shrink is True

    def test_unknown(self):
        caps = capabilities_for("reiserfs")
        assert not any([caps.online_grow, caps.offline_shrink])


class TestSafety:
    def test_test_mode_on(self):
        assert _test_mode() is True

    def test_rejects_real_disk(self):
        for dev in ("/dev/sda1", "/dev/vda2", "/dev/nvme0n1p3", "/dev/sdb"):
            with pytest.raises(SafetyError):
                assert_test_device(dev)

    def test_allows_loop(self):
        assert_test_device("/dev/loop0")
        assert_test_device("/dev/loop7p2")
        assert_test_device("/dev/mapper/partresize-test")

    def test_rejects_unknown_device(self):
        with pytest.raises(SafetyError):
            assert_test_device("/dev/mapper/some-real-vg")

    def test_shrink_margin_guard(self):
        # target below used -> reject
        with pytest.raises(SafetyError):
            assert_shrink_safe(50, 100)
        # target above used + margin -> ok (no exception raised)
        assert_shrink_safe(10**9, 10**8)

    def test_shrink_below_min(self):
        with pytest.raises(SafetyError):
            assert_shrink_safe(10**9, 10**7, min_fs_bytes=2 * 10**9)
