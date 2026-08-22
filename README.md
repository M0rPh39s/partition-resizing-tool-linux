# partresize

A safety-first Linux partition and filesystem resizing tool, written in Rust.

`partresize` resizes partitions **and** the filesystems inside them, choosing
the safest possible workflow automatically. It understands the whole storage
stack — partition table, LUKS, LVM, filesystem — and it **never reorders the
steps in a way that can corrupt data**. When an operation cannot be performed
safely on a mounted filesystem (for example, shrinking ext4), it does not force
it: it either performs a proper offline resize (unmount → resize → remount) or
schedules a reboot-assisted maintenance pass built from your own initramfs.

```text
partresize inspect /home
partresize capabilities /home
partresize plan   /home --size 300G          # never changes anything
partresize resize /home --grow 50G           # online grow when supported
partresize resize /home --size 300G --offline   # unmount, shrink, remount
partresize resize /      --size 40G --schedule   # root: do it at next reboot
```

> ⚠️ Partition resizing is inherently risky. `partresize` takes backups and
> refuses unsafe operations, but it **cannot** guarantee recovery from hardware
> failure or unexpected power loss. **Back up important data first.**

## Highlights

- **Storage-stack aware.** Discovers `Disk → Partition → LUKS → LVM PV → VG →
  LV → Filesystem → Mount` automatically via `lsblk`/`findmnt`/`blkid`.
- **Correct ordering, always.** Shrink = *filesystem → block device → partition*;
  grow = *partition → block device → filesystem*. Never the reverse.
- **Honest capability matrix.** ext4 grows online but only shrinks offline; XFS
  can only grow; Btrfs grows and shrinks online; the tool refuses what a
  filesystem genuinely cannot do.
- **Plan / execute separation.** A `plan` is an explicit, inspectable operation
  graph produced before anything destructive happens. The executor only runs a
  validated plan.
- **Safe offline workflows.** `--offline` (non-root) unmounts, resizes, remounts.
  `--schedule` builds a one-time initramfs maintenance boot for the root
  filesystem — no USB, live CD, or second OS required.
- **Backups & recovery.** Partition-table and LVM-metadata backups before
  destructive work; a persistent state machine detects and recovers interrupted
  operations.
- **Dry-run everywhere.** `--dry-run` prints the exact commands and changes
  nothing.

## Documentation

| Document | Contents |
|---|---|
| [docs/architecture.md](docs/architecture.md) | Module layout, data flow, plan/execute model |
| [docs/filesystems.md](docs/filesystems.md) | Per-filesystem capability matrix and limitations |
| [docs/safety.md](docs/safety.md) | The safety model and every guard |
| [docs/examples.md](docs/examples.md) | Worked examples for common scenarios |
| [docs/troubleshooting.md](docs/troubleshooting.md) | Common errors and fixes |
| [docs/developer.md](docs/developer.md) | Building, testing, contributing |
| [man/partresize.8](man/partresize.8) | Manual page |

## Build

```sh
cargo build --release
# binary at ./target/release/partresize
```

Only two dependencies (`serde`, `serde_json`); everything else is std.

## Install

```sh
sudo install -m 0755 target/release/partresize /usr/local/sbin/partresize
sudo install -m 0644 man/partresize.8 /usr/local/share/man/man8/partresize.8
```

`partresize` calls standard utilities at run time. Install what your stack
needs: `util-linux` (lsblk/findmnt/blkid/sfdisk/partprobe/blockdev),
`e2fsprogs` (ext), `xfsprogs` (XFS), `btrfs-progs` (Btrfs), `lvm2` (LVM),
`cryptsetup` (LUKS).

## Usage summary

```
partresize <command> [options]

  list                          List block devices
  inspect <target>              Show the storage topology
  capabilities <target>         Show what the filesystem can/can't do
  plan <target> <size-opt>      Show the resize plan (never changes anything)
  resize <target> <size-opt>    Resize a filesystem/partition
  grow <target> [--by S|--size S]
  shrink <target> --size S|--by S
  backup-table <disk> [file]    Back up a partition table
  restore-table <disk> <file>   Restore a partition table
  status | cancel | recover     Manage an in-progress/scheduled operation

Size options:  --size <S> | --grow <S> | --shrink <S> | --by <S>
Flags:         --dry-run  --offline  --schedule  -y/--yes  -v/--verbose
Sizes:         binary (KiB/MiB/GiB/TiB) and decimal (KB/MB/GB/TB); e.g. 300G, 80GiB
```

## Supported today

- **Filesystems:** ext4/ext3/ext2 (grow online, shrink offline), XFS (grow only),
  Btrfs (grow/shrink), swap (recreate). NTFS offline via `ntfsresize` is
  scaffolded; FAT/exFAT are detected but not resized.
- **Partition tables:** GPT and MBR (DOS).
- **Layers:** plain partitions, LVM (PV/VG/LV), LUKS.
- **Modes:** online grow; offline shrink/grow (immediate for non-root, scheduled
  reboot for root); dry-run; backups; recovery.

See [docs/filesystems.md](docs/filesystems.md) for the exact matrix and the list
of operations that are intentionally **unsupported**.

## Testing

```sh
cargo test            # unit tests + loopback integration tests (needs root)
```

Integration tests build disposable `disk.img` files on **loop devices** and run
the real binary against them. `PARTRESIZE_TEST_MODE=1` arms a guard that refuses
any non-loop/dm device, so the suite can never touch a real disk. See
[docs/developer.md](docs/developer.md) for the exact commands.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).

A Python prototype that pioneered the initramfs maintenance mechanism lives in
[`contrib/python-prototype/`](contrib/python-prototype/) for reference.
