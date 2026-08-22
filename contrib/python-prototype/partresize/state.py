"""Persistent recovery state machine.

State lives in ``/var/lib/partresize/state.json`` (overridable via
``PARTRESIZE_STATE_DIR``) so that a resize which spans a reboot — or which is
interrupted by power loss inside the maintenance environment — can be detected
and recovered on the next boot.

The lifecycle::

    PREPARED
      -> BOOTING_MAINTENANCE
        -> CHECKING_FILESYSTEM
          -> SHRINKING_FILESYSTEM / GROWING_FILESYSTEM
            -> RESIZING_BLOCK_DEVICE
              -> RESIZING_PARTITION
                -> VERIFYING
                  -> COMPLETED
    (any) -> FAILED
    (any) -> RECOVERY_REQUIRED
"""

from __future__ import annotations

import enum
import json
import os
import time
from dataclasses import asdict, dataclass, field
from typing import Any

from .errors import StateError
from .model import ResizePlan


class State(str, enum.Enum):
    PREPARED = "PREPARED"
    BOOTING_MAINTENANCE = "BOOTING_MAINTENANCE"
    CHECKING_FILESYSTEM = "CHECKING_FILESYSTEM"
    SHRINKING_FILESYSTEM = "SHRINKING_FILESYSTEM"
    GROWING_FILESYSTEM = "GROWING_FILESYSTEM"
    RESIZING_BLOCK_DEVICE = "RESIZING_BLOCK_DEVICE"
    RESIZING_PARTITION = "RESIZING_PARTITION"
    VERIFYING = "VERIFYING"
    COMPLETED = "COMPLETED"
    FAILED = "FAILED"
    RECOVERY_REQUIRED = "RECOVERY_REQUIRED"


# States from which it is safe to abort/cancel cleanly (nothing destructive
# has happened yet).
ABORTABLE = {State.PREPARED, State.BOOTING_MAINTENANCE}

# States that indicate the on-disk layout may be half-resized.
DANGEROUS = {
    State.SHRINKING_FILESYSTEM,
    State.RESIZING_BLOCK_DEVICE,
    State.RESIZING_PARTITION,
}

TERMINAL = {State.COMPLETED, State.FAILED}


def state_dir() -> str:
    return os.environ.get("PARTRESIZE_STATE_DIR", "/var/lib/partresize")


def _state_path() -> str:
    return os.path.join(state_dir(), "state.json")


@dataclass
class OperationState:
    op_id: str
    state: State
    plan: ResizePlan
    created_at: float = field(default_factory=time.time)
    updated_at: float = field(default_factory=time.time)
    history: list[dict[str, Any]] = field(default_factory=list)
    error: str | None = None

    def to_dict(self) -> dict[str, Any]:
        d = asdict(self)
        d["state"] = self.state.value
        d["plan"] = self.plan.to_dict()
        return d

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> OperationState:
        return cls(
            op_id=d["op_id"],
            state=State(d["state"]),
            plan=ResizePlan.from_dict(d["plan"]),
            created_at=d.get("created_at", time.time()),
            updated_at=d.get("updated_at", time.time()),
            history=list(d.get("history", [])),
            error=d.get("error"),
        )


class StateStore:
    """File-backed store for the single in-flight operation."""

    def __init__(self, directory: str | None = None) -> None:
        self.directory = directory or state_dir()
        self.path = os.path.join(self.directory, "state.json")

    def exists(self) -> bool:
        return os.path.exists(self.path)

    def load(self) -> OperationState | None:
        if not self.exists():
            return None
        try:
            with open(self.path, encoding="utf-8") as fh:
                return OperationState.from_dict(json.load(fh))
        except (json.JSONDecodeError, KeyError, OSError) as exc:
            raise StateError(f"corrupt state file {self.path}: {exc}") from exc

    def save(self, op: OperationState) -> None:
        os.makedirs(self.directory, exist_ok=True)
        op.updated_at = time.time()
        tmp = self.path + ".tmp"
        with open(tmp, "w", encoding="utf-8") as fh:
            json.dump(op.to_dict(), fh, indent=2)
            fh.flush()
            os.fsync(fh.fileno())
        os.replace(tmp, self.path)

    def transition(self, op: OperationState, new_state: State,
                   error: str | None = None) -> OperationState:
        op.history.append(
            {"from": op.state.value, "to": new_state.value, "at": time.time()}
        )
        op.state = new_state
        if error is not None:
            op.error = error
        self.save(op)
        return op

    def clear(self) -> None:
        if self.exists():
            os.remove(self.path)
