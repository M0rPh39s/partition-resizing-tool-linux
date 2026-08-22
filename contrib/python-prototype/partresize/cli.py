"""Command line interface for partresize."""

from __future__ import annotations

import argparse
import logging
import os
import sys

from . import app
from .errors import PartresizeError
from .model import ResizePlan
from .sizes import format_size, parse_size
from .state import StateStore

PROG = "partresize"


def _setup_logging(verbose: bool) -> None:
    level = logging.DEBUG if verbose else logging.INFO
    logging.basicConfig(
        level=level, format="%(message)s", stream=sys.stderr,
    )
    # our own logs at INFO go to stderr; command output uses print().


def _confirm(prompt: str, assume_yes: bool, default_no: bool = True) -> bool:
    if assume_yes:
        return True
    suffix = "[y/N]" if default_no else "[Y/n]"
    try:
        ans = input(f"{prompt} {suffix} ").strip().lower()
    except EOFError:
        return False
    if not ans:
        return not default_no
    return ans in ("y", "yes")


# --------------------------------------------------------------------------
# rendering
# --------------------------------------------------------------------------

def _print_topology(topo) -> None:
    disk = topo.disk
    part = topo.partition
    print("Storage topology:")
    print(f"Disk:       {disk.device}")
    if part is not None:
        print(f"Partition:  {part.device}")
    if topo.has_luks:
        print(f"LUKS:       {topo.luks.device}")
    if topo.has_lvm:
        print(f"LVM LV:     {topo.lvm_lv.device}")
    print(f"Filesystem: {topo.fstype}")
    print(f"Mount:      {topo.mountpoint}")


def _print_plan(p: ResizePlan) -> None:
    topo = p.topology
    _print_topology(topo)
    part = topo.partition
    if part is not None:
        print(f"Current partition: {format_size(part.size)}")
    print(f"Current filesystem: {format_size(p.current_fs_size)}")
    print(f"Used: {format_size(p.used_bytes)}")
    print(f"Target: {format_size(p.target_fs_size)}")

    caps_line = "Online shrink" if p.direction == "shrink" else "Online grow"
    supported = "SUPPORTED" if p.online else "NOT SUPPORTED"
    print(f"{caps_line}: {supported}")

    print("Execution strategy:")
    if p.online:
        print("    Immediate online resize (no reboot)")
    else:
        print("    Temporary initramfs maintenance environment")
    print(f"Required reboot:\n    {'YES' if p.requires_reboot else 'NO'}")
    print("External media:\n    NOT REQUIRED")
    print(f"Risk:\n    {p.risk.value}")

    print("Operations:")
    for i, op in enumerate(_display_ops(p), 1):
        print(f"    {i}. {op}")

    for w in p.warnings:
        print(f"Note: {w}")
    if p.blockers:
        print("\nThis operation cannot proceed:")
        for b in p.blockers:
            print(f"    - {b}")


def _display_ops(p: ResizePlan) -> list[str]:
    return [op.description for op in p.operations]


# --------------------------------------------------------------------------
# command handlers
# --------------------------------------------------------------------------

def cmd_inspect(args) -> int:
    res = app.inspect(args.mountpoint)
    topo = res.topology
    print("Root filesystem:" if args.mountpoint == "/" else "Filesystem:")
    print(f"    {topo.filesystem.props.get('backing', topo.filesystem.device)}")
    print(f"    {topo.fstype}")
    print(f"    Current filesystem size: {format_size(res.fs_size)}")
    print(f"    Used: {format_size(res.used)}")
    print()
    print(topo.describe())
    return 0


def cmd_capabilities(args) -> int:
    topo, caps = app.capabilities(args.mountpoint)
    print(f"Filesystem: {caps.fstype}")
    print(f"    online grow:    {'YES' if caps.online_grow else 'NO'}")
    print(f"    online shrink:  {'YES' if caps.online_shrink else 'NO'}")
    print(f"    offline grow:   {'YES' if caps.offline_grow else 'NO'}")
    print(f"    offline shrink: {'YES' if caps.offline_shrink else 'NO'}")
    for n in caps.notes:
        print(f"    note: {n}")
    return 0


def _resolve_request(args) -> tuple[str, int | None, int | None]:
    size = parse_size(args.size) if getattr(args, "size", None) else None
    by = parse_size(args.by) if getattr(args, "by", None) else None
    return args.direction, size, by


def cmd_plan(args) -> int:
    direction, size, by = _resolve_request(args)
    p = app.plan(args.mountpoint, direction=direction, size=size, by=by)
    _print_plan(p)
    print("\nNo changes will be made.")
    return 0 if p.feasible else 2


def cmd_resize(args) -> int:
    direction, size, by = _resolve_request(args)
    print("Analyzing system...")
    p = app.plan(args.mountpoint, direction=direction, size=size, by=by)
    _render_resize_intro(p)

    if not p.feasible:
        print("\nCannot proceed:")
        for b in p.blockers:
            print(f"    {b}")
        return 2

    if p.direction == "none":
        print("Filesystem is already the requested size; nothing to do.")
        return 0

    store = StateStore()

    if args.dry_run:
        print("\n--- DRY RUN (no changes will be made) ---")
        if p.online:
            app._resize_online(p, store, dry_run=True)
        else:
            app._resize_offline(p, store, dry_run=True, prefix=args.prefix)
        print("Dry run complete.")
        return 0

    if p.online:
        if not _confirm("Perform online resize now?", args.yes, default_no=False):
            print("Aborted.")
            return 1
        app._resize_online(p, store, dry_run=False)
        print("Resize completed successfully.")
        return 0

    # offline path
    _print_offline_plan(p)
    if not _confirm("Continue?", args.yes):
        print("Aborted.")
        return 1
    print("\nPreparing resize environment...")
    op_id = app._resize_offline(p, store, dry_run=False, prefix=args.prefix)
    print("Resize operation scheduled.")
    if _confirm("Reboot now?", args.yes, default_no=False):
        from .boot import detect_boot_environment
        detect_boot_environment(prefix=args.prefix).boot()
    else:
        print(f"Operation {op_id} is scheduled; it will run on next reboot.")
        print("Run 'partresize cancel' to abort before rebooting.")
    return 0


def _render_resize_intro(p: ResizePlan) -> None:
    topo = p.topology
    label = "Root filesystem:" if topo.mountpoint == "/" else "Filesystem:"
    print(label)
    print(f"    {topo.filesystem.props.get('backing', topo.filesystem.device)}")
    print(f"    {topo.fstype}")
    print(f"    Current filesystem size: {format_size(p.current_fs_size)}")
    print(f"    Used: {format_size(p.used_bytes)}")
    print(f"    Target: {format_size(p.target_fs_size)}")


def _print_offline_plan(p: ResizePlan) -> None:
    if p.direction == "shrink":
        print("The filesystem cannot be safely shrunk while mounted.")
    else:
        print("This operation cannot be completed while the filesystem is mounted.")
    print("A temporary offline resize environment will be created using")
    print("your existing Linux installation.")
    print("No USB or external operating system is required.")
    print("Resize plan:")
    for i, op in enumerate(_display_ops(p), 1):
        print(f"    {i}. {op}")


def cmd_status(args) -> int:
    op = app.status()
    if op is None:
        print("No resize operation in progress.")
        return 0
    print(f"Operation:   {op.op_id}")
    print(f"State:       {op.state.value}")
    print(f"Direction:   {op.plan.direction}")
    print(f"Target:      {format_size(op.plan.target_fs_size)}")
    if op.error:
        print(f"Error:       {op.error}")
    return 0


def cmd_cancel(args) -> int:
    msg = app.cancel(prefix=args.prefix)
    print(msg)
    return 0


def cmd_recover(args) -> int:
    msg = app.recover(prefix=args.prefix)
    print(msg)
    return 0


# --------------------------------------------------------------------------
# argument parsing
# --------------------------------------------------------------------------

def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog=PROG,
        description="Resize the partition of a running Linux system without "
                    "external media.",
    )
    parser.add_argument("-v", "--verbose", action="store_true")
    parser.add_argument(
        "--prefix", default=os.environ.get("PARTRESIZE_PREFIX", "/"),
        help=argparse.SUPPRESS,
    )
    sub = parser.add_subparsers(dest="command", required=True)

    def add_target(sp):
        sp.add_argument("mountpoint", help="mount point or device, e.g. /")

    p_inspect = sub.add_parser("inspect", help="show the storage topology")
    add_target(p_inspect)
    p_inspect.set_defaults(func=cmd_inspect)

    p_caps = sub.add_parser("capabilities", help="show resize capabilities")
    add_target(p_caps)
    p_caps.set_defaults(func=cmd_capabilities)

    def add_resize_args(sp, direction):
        add_target(sp)
        sp.add_argument("--size", help="absolute target size, e.g. 80G")
        if direction != "shrink":
            sp.add_argument("--by", help="grow/shrink by this amount, e.g. 50G")
        else:
            sp.add_argument("--by", help="shrink by this amount, e.g. 20G")
        sp.add_argument("--dry-run", action="store_true",
                        help="show what would be done, change nothing")
        sp.add_argument("-y", "--yes", action="store_true",
                        help="do not prompt for confirmation")
        sp.set_defaults(direction=direction)

    p_plan = sub.add_parser("plan", help="show the resize plan (no changes)")
    add_resize_args(p_plan, "resize")
    p_plan.set_defaults(func=cmd_plan)

    p_grow = sub.add_parser("grow", help="grow the filesystem/partition")
    add_resize_args(p_grow, "grow")
    p_grow.set_defaults(func=cmd_resize)

    p_shrink = sub.add_parser("shrink", help="shrink the filesystem/partition")
    add_resize_args(p_shrink, "shrink")
    p_shrink.set_defaults(func=cmd_resize)

    p_resize = sub.add_parser("resize", help="grow or shrink to a target size")
    add_resize_args(p_resize, "resize")
    p_resize.set_defaults(func=cmd_resize)

    p_status = sub.add_parser("status", help="show in-progress operation state")
    p_status.set_defaults(func=cmd_status)

    p_cancel = sub.add_parser("cancel", help="cancel a scheduled operation")
    p_cancel.set_defaults(func=cmd_cancel)

    p_recover = sub.add_parser("recover", help="recover an interrupted operation")
    p_recover.set_defaults(func=cmd_recover)

    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    _setup_logging(getattr(args, "verbose", False))
    try:
        return args.func(args)
    except PartresizeError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print("\ninterrupted", file=sys.stderr)
        return 130


if __name__ == "__main__":
    sys.exit(main())
