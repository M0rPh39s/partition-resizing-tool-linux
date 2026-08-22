# Safety model

`partresize` treats data integrity as the top priority. Every guard below is
enforced before or during execution.

## The cardinal rule

> Never corrupt data to satisfy a resize request.

If an operation cannot be done safely, the tool **refuses** it or converts it
into a properly ordered offline workflow — it never forces an unsupported
operation.

## Invariants

1. `filesystem_size ≤ containing_block_device_size` — checked before growing
   the filesystem and re-checked after every operation (`verification`).
2. **Never shrink the partition before the filesystem.** The planner emits
   filesystem-shrink first, always; the executor runs the plan in order.
3. **Never make a partition smaller than the filesystem it holds.** Partition
   targets are aligned up and clamped to at least the filesystem/container size.
4. **Never grow a filesystem beyond its device**, or a partition beyond the
   disk.

## Preflight checks

- **Root privilege detection** — destructive operations require root.
- **Topology discovery** — the full stack is resolved before planning.
- **Mount detection** — a mounted target that needs an offline op is never
  silently unmounted; the tool refuses and explains, or acts only under an
  explicit `--offline` / `--schedule`.
- **Capability check** — the filesystem must actually support the operation.
- **Free-space check** — a shrink target must exceed used data plus a safety
  margin (10% of used, at least 256 MiB) and the filesystem's own reported
  minimum size.
- **Alignment** — partitions are aligned to 1 MiB; LV/PV targets to LVM extents.
- **Ambiguity refusal** — the tool operates on the resolved filesystem's device;
  it will not resize a whole disk because you named `/dev/nvme0n1` where you
  meant `/dev/nvme0n1p3` (the topology makes the real target explicit, shown
  before any change).

## Confirmation & modes

- Destructive operations print a **WARNING block** (device, filesystem, mount
  point, current/new size, used data) and require `y` unless `--yes` is given.
- `--dry-run` prints the exact operations and commands and changes nothing.
- `--yes` is only honoured when explicitly passed (non-interactive use).

## Backups

Before a destructive shrink the tool saves, under
`/var/lib/partresize/backups/<op-id>/`:

- the **partition table** (`sfdisk -d` dump — restorable with
  `partresize restore-table`),
- **LVM metadata** (`vgcfgbackup`) when LVM is present.

These are genuine, restorable snapshots — not a placeholder. They protect
against *metadata* mistakes; they are **not** a substitute for a data backup and
cannot recover from hardware failure or power loss mid-write.

## Recovery

A persistent state machine records progress across a reboot or crash. Because
the filesystem is always shrunk before the partition, an interruption during the
partition step leaves the filesystem *smaller than* the partition — a safe,
mountable state. `partresize recover` inspects the saved state and reports
whether the layout is intact and what remains to do.

## The test-mode guard

When `PARTRESIZE_TEST_MODE=1` is set, every destructive operation must target a
loop or device-mapper test device; any attempt to touch `/dev/sda`, `/dev/vda`,
`/dev/nvme*`, `/dev/mmcblk*`, etc. is refused immediately. The integration test
suite runs with this armed so it can never damage the host.

## Limits — read this

Partition and filesystem resizing is inherently risky. `partresize` reduces the
risk with ordering guarantees, checks, and backups, but it **cannot guarantee**
recovery from:

- hardware failure,
- unexpected power loss during a write,
- a filesystem or tool bug.

**Always back up important data before resizing.**
