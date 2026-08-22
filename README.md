# partition-resizing-tool-linux (`partresize`)

Resize the partition that holds the **currently running** Linux system —
including the root filesystem `/` — **without a USB stick, live CD, second
operating system, or any external recovery environment.**

```
sudo partresize grow /
sudo partresize shrink / --size 80G
sudo partresize resize / --size 150G
```

The tool figures out the safest way to carry out the request:

* **Mode 1 — true online resize.** If the filesystem and storage stack support
  the operation while mounted (e.g. growing ext4/btrfs/xfs), it is performed
  immediately, no reboot.
* **Mode 2 — reboot-assisted offline resize.** If the operation cannot be done
  safely while `/` is mounted (e.g. shrinking ext4), the tool builds a
  temporary maintenance environment **from your existing installation's own
  initramfs**, schedules a one-time boot into it, performs the resize while the
  root filesystem is not mounted, verifies, cleans up, and reboots you back
  into your normal system.

It never tells you to "boot a live USB".

## How the offline resize works without external media

Every Linux system already boots through a temporary root-in-RAM: the
**initramfs**. The kernel runs it *before* mounting the real root filesystem —
the one moment when the root block device exists but root is not mounted
read-write. That is exactly the offline maintenance environment we need, and it
is assembled entirely from the user's own installed system.

1. **Build a separate maintenance initramfs** (`/boot/partresize-maint-<id>.img`)
   — the system's real initramfs is never modified, so the normal boot path
   always remains a safe fallback. The initramfs generator is auto-detected:
   `initramfs-tools` (Debian/Ubuntu), `dracut` (Fedora/RHEL/SUSE), or
   `mkinitcpio` (Arch). The generated image contains the resize script plus the
   required tools (`e2fsprogs`, `sfdisk`, `lvm`, `cryptsetup`, …).
2. **Add a one-time boot entry** with a marker on the kernel command line
   (`partresize.maint=<id>`). GRUB → `grub-reboot` (one-shot); systemd-boot →
   `bootctl set-oneshot`. The next boot enters maintenance; every boot after
   returns to normal automatically.
3. **Reboot.** A pre-mount hook sees the marker, refuses to mount the target
   root, and runs the resize in the strict order
   *fsck → shrink filesystem → shrink block-device/LV → shrink partition →
   verify* (reversed, container-first, for a grow).
4. **Persist state** in `/var/lib/partresize/state.json` — a real state machine
   (`PREPARED → BOOTING_MAINTENANCE → CHECKING_FILESYSTEM → SHRINKING_FILESYSTEM
   → RESIZING_BLOCK_DEVICE → RESIZING_PARTITION → VERIFYING → COMPLETED`, with
   `FAILED` / `RECOVERY_REQUIRED`) — so a power loss mid-operation is detected
   and recoverable on the next boot.
5. **Clean up** the temporary image and boot entry, then reboot normally.

## Commands

| Command | Purpose |
|---|---|
| `partresize inspect /` | Show the discovered storage topology and sizes. |
| `partresize capabilities /` | Show what the filesystem can/can't do online/offline. |
| `partresize plan / --size 80G` | Show the full plan. **Never changes anything.** |
| `partresize grow /` | Grow to fill available space. |
| `partresize shrink / --size 80G` | Shrink to a target size. |
| `partresize resize / --size 150G` | Grow or shrink to a target size. |
| `partresize <op> … --dry-run` | Show exactly what would run, change nothing. |
| `partresize status` | Show any in-progress operation. |
| `partresize cancel` | Cancel a scheduled (not-yet-run) operation. |
| `partresize recover` | Detect and recover an interrupted operation. |

## Storage stacks understood

The tool discovers the complete stack before planning, e.g.:

```
disk → partition → LUKS → LVM PV → VG → LV → ext4 → /
disk → partition → ext4 → /
disk → partition → btrfs → /
```

Supported today, in priority order: **ext4** (grow online / shrink offline),
**GPT & MBR** partition tables, **LVM**, **LUKS**, **Btrfs** (online grow +
shrink), **XFS** (grow only — the tool refuses to "shrink" XFS because XFS
cannot shrink).

## Safety guarantees

* The filesystem is **always shrunk before** the partition — never the reverse.
* The partition is never shrunk below the filesystem/containers it holds.
* A shrink target must clear used data plus a safety margin and the filesystem's
  own reported minimum size, or it is refused.
* Growing never overwrites a neighbouring partition; if a partition would have
  to be *moved* to grow, the tool reports it rather than doing it silently.
* Partition-table and LVM-metadata backups are taken where possible.
* Full `--dry-run` for every operation.

## Development, testing and safety in tests

The project is Python and orchestrates the standard Linux storage utilities.

```
python -m pytest            # unit + loop-device integration tests
ruff check partresize/      # lint
```

Integration tests run against **disposable `disk.img` files attached as loop
devices** — they create GPT/MBR tables, filesystems and simulated roots, and
exercise real offline shrink/grow, recovery, and interrupted-resize handling.

A hard safety guard (`PARTRESIZE_TEST_MODE=1`, set for the whole suite) makes
every destructive operation **reject any device that is not a loop / dm test
device** — `/dev/sda`, `/dev/vda`, `/dev/nvme*` and friends raise immediately.
The tests never touch the host's real disks, bootloader, initramfs, or root
filesystem. Boot-integration code writes into a temporary prefix during tests.

> Note: some paths (kernel online-resize ioctl, Btrfs/XFS/LVM/LUKS kernel
> modules) are unavailable in minimal CI sandboxes; those tests skip
> automatically and run on a real machine. The offline ext4 resize path — the
> core of Mode 2 — is fully exercised on loop devices.
