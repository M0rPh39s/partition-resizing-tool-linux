"""Thin subprocess wrappers with logging and dry-run support."""

from __future__ import annotations

import logging
import shutil
import subprocess
from collections.abc import Sequence
from dataclasses import dataclass

from .errors import DependencyError, ExecutionError

log = logging.getLogger("partresize.run")


@dataclass
class CommandResult:
    args: list[str]
    returncode: int
    stdout: str
    stderr: str

    @property
    def ok(self) -> bool:
        return self.returncode == 0


def have(tool: str) -> bool:
    """Return True if ``tool`` is on PATH."""
    return shutil.which(tool) is not None


def require(*tools: str) -> None:
    """Raise :class:`DependencyError` if any of ``tools`` is missing."""
    missing = [t for t in tools if not have(t)]
    if missing:
        raise DependencyError(
            "required tool(s) not found: " + ", ".join(missing)
        )


def run(
    args: Sequence[str],
    *,
    check: bool = True,
    input_text: str | None = None,
    dry_run: bool = False,
    timeout: float | None = None,
) -> CommandResult:
    """Run a command.

    When ``dry_run`` is set the command is only logged and a synthetic
    successful result is returned; nothing is executed.
    """
    argv = [str(a) for a in args]
    if dry_run:
        log.info("DRY-RUN: %s", " ".join(argv))
        return CommandResult(argv, 0, "", "")
    log.debug("exec: %s", " ".join(argv))
    try:
        proc = subprocess.run(
            argv,
            input=input_text,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
    except FileNotFoundError as exc:
        raise DependencyError(f"command not found: {argv[0]}") from exc
    result = CommandResult(argv, proc.returncode, proc.stdout, proc.stderr)
    if check and not result.ok:
        raise ExecutionError(
            f"command failed ({result.returncode}): {' '.join(argv)}\n"
            f"stdout: {result.stdout.strip()}\n"
            f"stderr: {result.stderr.strip()}"
        )
    return result
