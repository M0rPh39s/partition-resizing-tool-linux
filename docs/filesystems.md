# Filesystem capability matrix

`partresize` keeps an explicit, conservative matrix of what each filesystem can
do. It **never** claims an operation a filesystem cannot genuinely perform.

| Filesystem | Online grow | Online shrink | Offline grow | Offline shrink | Notes |
|---|:---:|:---:|:---:|:---:|---|
| ext4 | ✅ | ❌ | ✅ | ✅ | Grows mounted; shrink **requires unmount**. |
| ext3 / ext2 | ✅ | ❌ | ✅ | ✅ | Same as ext4. |
| XFS | ✅ | ❌ | ❌ | ❌ | **Grow only**, and only while mounted. Cannot shrink. |
| Btrfs | ✅ | ✅ | ✅ | ✅ | Grow and shrink online via `btrfs filesystem resize`. |
| f2fs | ✅ | ❌ | ✅ | ❌ | Cannot shrink. |
| NTFS | ❌ | ❌ | ✅ | ✅ | Offline via `ntfsresize` (best effort; scaffolded). |
| FAT / exFAT | ❌ | ❌ | ❌ | ❌ | Detected, not resized by this tool. |
| swap | ❌ | ❌ | ✅ | ✅ | Not resized in place: `swapoff` → `mkswap` → `swapon`. |

Legend: ✅ supported · ❌ not supported.

## Why these differ

- **ext4 shrink is offline.** `resize2fs` refuses to shrink a mounted
  filesystem; the kernel also will not shrink an in-use partition. So any shrink
  that reclaims partition space is an **offline** operation, even when the
  filesystem could technically shrink online.
- **XFS never shrinks.** The on-disk format has no shrink operation. Requesting
  a smaller XFS is refused with a clear message, not attempted.
- **Btrfs resizes online** but reclaiming the freed *partition* space still
  needs the partition shrunk offline; the tool schedules that.

## Requested operation → what happens

| You run | ext4 | XFS | Btrfs |
|---|---|---|---|
| `resize X --grow …` (space available) | online grow | online grow | online grow |
| `resize X --size <bigger>` | online grow | online grow | online grow |
| `resize X --size <smaller>` (mounted, no flag) | **refused** with `--offline`/`--schedule` guidance | **refused** (XFS can't shrink) | **refused** with guidance |
| `resize X --size <smaller> --offline` | unmount→shrink→remount | refused | unmount→shrink→remount |
| `resize / --size <smaller> --schedule` | reboot maintenance | refused | reboot maintenance |

## Block-device layers

- **LVM** — the LV is grown/shrunk with `lvextend`/`lvreduce`; the PV with
  `pvresize`. On shrink, the LV is reduced to fit the filesystem *before* the
  partition is touched.
- **LUKS** — the mapping is grown/shrunk with `cryptsetup resize`. On shrink it
  is resized after the filesystem/LV inside it, before the partition.

## Intentionally unsupported (today)

- FAT32 / exFAT resizing.
- RAID (`md`) reshaping — detected as a dependency and refused rather than
  mishandled.
- Partition **movement** (relocating a partition's start to make room). The
  tool detects when a grow would require moving a neighbouring partition and
  reports it instead of overwriting data.
- Online NTFS resizing.
