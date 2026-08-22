"""Core data model: storage topology, capabilities, plans and operations.

Everything here is a plain dataclass so it serialises cleanly to JSON for the
persistent state file that survives across the maintenance reboot.
"""

from __future__ import annotations

import enum
from dataclasses import asdict, dataclass, field
from typing import Any

from .sizes import format_size


class LayerKind(str, enum.Enum):
    DISK = "disk"
    PARTITION = "partition"
    LUKS = "luks"
    LVM_PV = "lvm-pv"
    LVM_VG = "lvm-vg"
    LVM_LV = "lvm-lv"
    FILESYSTEM = "filesystem"


@dataclass
class Layer:
    """One level of the storage stack.

    ``device`` is the kernel device path (``/dev/...``).  ``size`` is the size
    of this layer in bytes.  ``props`` carries kind-specific extras (fstype,
    vg name, luks uuid, partition number, start offset, ...).
    """

    kind: LayerKind
    device: str
    size: int = 0
    props: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        d = asdict(self)
        d["kind"] = self.kind.value
        return d

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> Layer:
        return cls(
            kind=LayerKind(d["kind"]),
            device=d["device"],
            size=int(d.get("size", 0)),
            props=dict(d.get("props", {})),
        )


@dataclass
class Topology:
    """The complete resolved stack for a mount point, top (disk) to bottom (fs).

    ``layers[0]`` is the physical disk; ``layers[-1]`` is the filesystem.
    """

    mountpoint: str
    layers: list[Layer]

    # convenience accessors -------------------------------------------------
    def _first(self, kind: LayerKind) -> Layer | None:
        for layer in self.layers:
            if layer.kind == kind:
                return layer
        return None

    @property
    def disk(self) -> Layer:
        layer = self._first(LayerKind.DISK)
        if layer is None:
            raise ValueError("topology has no disk layer")
        return layer

    @property
    def partition(self) -> Layer | None:
        return self._first(LayerKind.PARTITION)

    @property
    def filesystem(self) -> Layer:
        layer = self.layers[-1]
        if layer.kind != LayerKind.FILESYSTEM:
            raise ValueError("bottom layer is not a filesystem")
        return layer

    @property
    def luks(self) -> Layer | None:
        return self._first(LayerKind.LUKS)

    @property
    def lvm_lv(self) -> Layer | None:
        return self._first(LayerKind.LVM_LV)

    @property
    def fstype(self) -> str:
        return str(self.filesystem.props.get("fstype", ""))

    @property
    def has_lvm(self) -> bool:
        return self.lvm_lv is not None

    @property
    def has_luks(self) -> bool:
        return self.luks is not None

    def to_dict(self) -> dict[str, Any]:
        return {
            "mountpoint": self.mountpoint,
            "layers": [layer.to_dict() for layer in self.layers],
        }

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> Topology:
        return cls(
            mountpoint=d["mountpoint"],
            layers=[Layer.from_dict(x) for x in d["layers"]],
        )

    def describe(self) -> str:
        lines = []
        indent = ""
        for layer in self.layers:
            label = layer.kind.value
            extra = ""
            if layer.kind == LayerKind.FILESYSTEM:
                extra = f" ({layer.props.get('fstype', '?')})"
            lines.append(
                f"{indent}{layer.device}  [{label}{extra}]  {format_size(layer.size)}"
            )
            indent += "  "
        return "\n".join(lines)


@dataclass
class Capabilities:
    """What the filesystem/stack can do, mounted vs. offline."""

    fstype: str
    online_grow: bool
    online_shrink: bool
    offline_grow: bool
    offline_shrink: bool
    notes: list[str] = field(default_factory=list)

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


class OpKind(str, enum.Enum):
    FSCK = "fsck"
    FS_GROW = "fs-grow"
    FS_SHRINK = "fs-shrink"
    BLOCKDEV_GROW = "blockdev-grow"
    BLOCKDEV_SHRINK = "blockdev-shrink"
    PARTITION_GROW = "partition-grow"
    PARTITION_SHRINK = "partition-shrink"
    VERIFY = "verify"


@dataclass
class Operation:
    """A single atomic step of a resize plan."""

    kind: OpKind
    device: str
    target_size: int | None = None
    description: str = ""
    props: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        d = asdict(self)
        d["kind"] = self.kind.value
        return d

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> Operation:
        return cls(
            kind=OpKind(d["kind"]),
            device=d["device"],
            target_size=d.get("target_size"),
            description=d.get("description", ""),
            props=dict(d.get("props", {})),
        )


class RiskLevel(str, enum.Enum):
    LOW = "LOW"
    MEDIUM = "MEDIUM"
    HIGH = "HIGH"


@dataclass
class ResizePlan:
    """A fully resolved, ordered plan describing how a resize will be done."""

    mountpoint: str
    topology: Topology
    direction: str  # "grow" | "shrink" | "none"
    current_fs_size: int
    target_fs_size: int
    used_bytes: int
    online: bool
    requires_reboot: bool
    risk: RiskLevel
    operations: list[Operation]
    warnings: list[str] = field(default_factory=list)
    blockers: list[str] = field(default_factory=list)

    @property
    def feasible(self) -> bool:
        return not self.blockers

    def to_dict(self) -> dict[str, Any]:
        return {
            "mountpoint": self.mountpoint,
            "topology": self.topology.to_dict(),
            "direction": self.direction,
            "current_fs_size": self.current_fs_size,
            "target_fs_size": self.target_fs_size,
            "used_bytes": self.used_bytes,
            "online": self.online,
            "requires_reboot": self.requires_reboot,
            "risk": self.risk.value,
            "operations": [op.to_dict() for op in self.operations],
            "warnings": list(self.warnings),
            "blockers": list(self.blockers),
        }

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> ResizePlan:
        return cls(
            mountpoint=d["mountpoint"],
            topology=Topology.from_dict(d["topology"]),
            direction=d["direction"],
            current_fs_size=int(d["current_fs_size"]),
            target_fs_size=int(d["target_fs_size"]),
            used_bytes=int(d["used_bytes"]),
            online=bool(d["online"]),
            requires_reboot=bool(d["requires_reboot"]),
            risk=RiskLevel(d["risk"]),
            operations=[Operation.from_dict(x) for x in d["operations"]],
            warnings=list(d.get("warnings", [])),
            blockers=list(d.get("blockers", [])),
        )
