"""Parsing and formatting of byte sizes.

All internal arithmetic is done in **bytes** (integers).  Human input accepts
both decimal (KB/MB/GB/TB, powers of 1000) and binary (KiB/MiB/GiB/TiB, powers
of 1024) suffixes.  A bare number is interpreted as bytes.  Following common
disk-tool convention, an unsuffixed ``G``/``M``/``T``/``K`` is treated as the
*binary* unit (GiB etc.), which is what users almost always mean when resizing.
"""

from __future__ import annotations

import re

_UNITS = {
    "": 1,
    "b": 1,
    "k": 1024,
    "kib": 1024,
    "kb": 1000,
    "m": 1024**2,
    "mib": 1024**2,
    "mb": 1000**2,
    "g": 1024**3,
    "gib": 1024**3,
    "gb": 1000**3,
    "t": 1024**4,
    "tib": 1024**4,
    "tb": 1000**4,
    "p": 1024**5,
    "pib": 1024**5,
    "pb": 1000**5,
}

_SIZE_RE = re.compile(r"^\s*([+-]?\d+(?:\.\d+)?)\s*([a-zA-Z]*)\s*$")


def parse_size(text: str) -> int:
    """Parse a human size string into a byte count.

    >>> parse_size("80G")
    85899345920
    >>> parse_size("80GiB")
    85899345920
    >>> parse_size("1000MB")
    1000000000
    >>> parse_size("512")
    512
    """
    if isinstance(text, (int,)):
        return int(text)
    m = _SIZE_RE.match(text)
    if not m:
        raise ValueError(f"invalid size: {text!r}")
    value = float(m.group(1))
    unit = m.group(2).lower()
    if unit not in _UNITS:
        raise ValueError(f"unknown size unit in {text!r}")
    result = value * _UNITS[unit]
    if result != int(result):
        # sub-byte precision is meaningless
        result = int(result)
    return int(result)


def format_size(num_bytes: int, *, binary: bool = True, precision: int = 2) -> str:
    """Format a byte count into a human readable string.

    >>> format_size(85899345920)
    '80.00 GiB'
    """
    if num_bytes < 0:
        return "-" + format_size(-num_bytes, binary=binary, precision=precision)
    base = 1024 if binary else 1000
    units = (
        ["B", "KiB", "MiB", "GiB", "TiB", "PiB"]
        if binary
        else ["B", "KB", "MB", "GB", "TB", "PB"]
    )
    value = float(num_bytes)
    for unit in units:
        if value < base or unit == units[-1]:
            if unit == "B":
                return f"{int(value)} {unit}"
            return f"{value:.{precision}f} {unit}"
        value /= base
    # unreachable
    return f"{num_bytes} B"


def round_down(value: int, granularity: int) -> int:
    """Round ``value`` down to a multiple of ``granularity``."""
    if granularity <= 0:
        return value
    return (value // granularity) * granularity


def round_up(value: int, granularity: int) -> int:
    """Round ``value`` up to a multiple of ``granularity``."""
    if granularity <= 0:
        return value
    return ((value + granularity - 1) // granularity) * granularity
