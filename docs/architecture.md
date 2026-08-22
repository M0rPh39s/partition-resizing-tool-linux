# Architecture

`partresize` is organised around a **storage-stack model** and a strict
separation of **discovery → planning → execution → verification**. Nothing
destructive happens until a fully-resolved, validated plan exists.

## Data flow

```text
            target (mount point or device)
                       │
                 ┌─────▼─────┐
                 │ discovery │  lsblk / findmnt / blkid  →  Topology
                 └─────┬─────┘
                       │  Topology (Disk→Part→LUKS→PV→VG→LV→FS→Mount)
                 ┌─────▼─────┐
                 │  planner  │  + capability matrix + safety checks
                 └─────┬─────┘
                       │  ResizePlan { direction, ops[], online,
                       │               requires_reboot, risk, blockers }
        ┌──────────────┼───────────────┐
        │              │               │
   plan (print)   dry-run (log)   executor (mutate)
                                       │
                                 verification  (fs ≤ device invariant)
```

## Modules (`src/`)

| Module | Responsibility |
|---|---|
| `cli/` | Argument parsing, dispatch, and all human output/rendering. |
| `errors/` | `Error` enum with administrator-friendly `Display` messages. |
| `discovery/` | Build a `Topology` from `lsblk -J` + `findmnt`/`blkid`. |
| `storage/` | The `Layer`/`LayerKind`/`Topology` data model. |
| `filesystem/` | Capability matrix + per-FS drivers (`ext`, `xfs`, `btrfs`). |
| `partition/` | `sfdisk`-based table read, geometry, resize, and backups. |
| `lvm/` | PV/VG/LV grow/shrink and metadata backup. |
| `crypt/` | LUKS grow/shrink via `cryptsetup`. |
| `planner/` | Pure analysis → an ordered `ResizePlan`. Never touches devices. |
| `executor/` | Executes a validated plan; records state; takes backups. |
| `safety/` | Size sanity, privilege detection, and the test-device guard. |
| `recovery/` | Persistent state machine + backup directories. |
| `verification/` | Post-resize invariant checks and final size reporting. |
| `schedule/` | Offline execution + reboot-assisted initramfs workflow. |
| `util/` | Command runner and size parsing/formatting. |

## The plan model

```rust
struct ResizePlan {
    target: String,
    topology: Topology,
    direction: Direction,        // Grow | Shrink | None
    current_fs_size: u64,
    target_fs_size: u64,
    used_bytes: u64,
    online: bool,
    requires_reboot: bool,
    destructive: bool,
    risk_level: RiskLevel,       // Low | Medium | High
    operations: Vec<Operation>,  // ordered, explicit
    warnings: Vec<String>,
    blockers: Vec<String>,       // non-empty ⇒ infeasible
}
```

Each `Operation` has a `kind` (`Fsck`, `FsShrink`, `FsGrow`, `BlockdevShrink`,
`BlockdevGrow`, `PartitionShrink`, `PartitionGrow`, `Verify`), the device it
acts on, and an optional target size. The **order of the vector is the order of
execution**, and the planner guarantees the invariant orderings below.

## Ordering invariants

**Shrink** (reclaim space): the filesystem is made smaller *before* anything
that contains it.

```text
fsck → shrink filesystem → verify → reduce LV → shrink PV → shrink LUKS → shrink partition → verify
```

**Grow** (consume space): every container is enlarged *before* its contents.

```text
grow partition → grow LUKS → grow PV → extend LV → grow filesystem → verify
```

The fundamental invariant enforced at every step and re-checked afterwards:

```text
filesystem_size ≤ containing_block_device_size
```

## Execution modes

1. **Online** — the filesystem and stack support the operation while mounted
   (e.g. ext4/btrfs/xfs grow). Done immediately.
2. **Offline, immediate** (`--offline`, non-root) — unmount, resize, remount.
3. **Offline, scheduled** (`--schedule`, any target incl. `/`) — build a
   one-time initramfs maintenance image + a self-reverting bootloader entry,
   reboot into it, resize while the root filesystem is unmounted, verify, clean
   up, reboot normally. Distribution-independent: initramfs-tools / dracut /
   mkinitcpio and GRUB / systemd-boot are detected at run time.

## State machine (`recovery/`)

State is persisted to `/var/lib/partresize/state.json` (override with
`PARTRESIZE_STATE_DIR`) so an operation spanning a reboot — or interrupted by
power loss — is recoverable:

```text
PREPARED → BOOTING_MAINTENANCE → CHECKING_FILESYSTEM
        → SHRINKING_FILESYSTEM / GROWING_FILESYSTEM
        → RESIZING_BLOCK_DEVICE → RESIZING_PARTITION
        → VERIFYING → COMPLETED
   (any) → FAILED | RECOVERY_REQUIRED
```

Because the filesystem is always shrunk before the partition, an interruption
during the partition step leaves the filesystem *smaller* than the partition —
a safe state — which `partresize recover` reports.
