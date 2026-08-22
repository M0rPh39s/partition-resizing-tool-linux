"""Exception hierarchy for partresize."""

from __future__ import annotations


class PartresizeError(Exception):
    """Base class for all partresize errors."""


class DependencyError(PartresizeError):
    """A required external tool is missing."""


class TopologyError(PartresizeError):
    """The storage topology could not be discovered or is unsupported."""


class CapabilityError(PartresizeError):
    """The requested operation is not supported for this filesystem/stack."""


class PlanError(PartresizeError):
    """A resize plan could not be constructed (e.g. insufficient space)."""


class SafetyError(PartresizeError):
    """An operation was rejected because it would be unsafe."""


class ExecutionError(PartresizeError):
    """A resize step failed while executing."""


class BootEnvironmentError(PartresizeError):
    """The reboot-assisted maintenance environment could not be prepared."""


class StateError(PartresizeError):
    """The persistent operation state is missing or inconsistent."""
