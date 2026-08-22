# Examples

All commands need root for anything destructive. Sizes accept binary
(`GiB`/`MiB`) and decimal (`GB`/`MB`) units; a bare `G`/`M` means the binary
unit.

## Inspect the stack

```sh
$ sudo partresize inspect /home
Storage topology:
/dev/nvme0n1  931.51 GiB
    └── /dev/nvme0n1p3  900.00 GiB
        └── /dev/mapper/cryptroot  900.00 GiB
            └── /dev/mapper/ubuntu--vg-home (VG ubuntu-vg)  500.00 GiB
                └── /dev/mapper/ubuntu--vg-home [ext4]  500.00 GiB
                    └── /home  (mount point)
Current filesystem size: 500.00 GiB
Used:                    173.00 GiB
Minimum shrink size:     181.00 GiB
```

## See what's possible

```sh
$ sudo partresize capabilities /home
Filesystem: ext4
Mount point: /home
    online grow:    YES
    online shrink:  NO
    offline grow:   YES
    offline shrink: YES
    note: online grow supported; shrink requires the filesystem to be unmounted
```

## Plan (never changes anything)

```sh
$ sudo partresize plan /home --size 300G
Storage topology:
    …
Current filesystem: 500.00 GiB
Used:               173.00 GiB
Target:             300.00 GiB
Online shrink: NOT SUPPORTED
Execution strategy:
    Temporary initramfs maintenance environment
Required reboot:  YES
Risk:             HIGH
Operations:
    1. Check filesystem integrity
    2. Shrink ext4 filesystem to the target size
    3. Verify the shrunk filesystem
    4. Reduce the logical volume to fit the filesystem
    5. Shrink the partition (never before the filesystem)
    6. Verify the partition and filesystem sizes
Status: SAFE TO EXECUTE OFFLINE
```

## Grow online (no reboot)

```sh
# Grow /home by 50 GiB (ext4 grows while mounted)
$ sudo partresize resize /home --grow 50G
Operation: ONLINE GROW
…
Resize completed successfully.

# Grow the root filesystem online
$ sudo partresize resize / --grow 20G
```

## Shrink a mounted, non-root filesystem

```sh
# Without a flag, a mounted ext4 shrink is refused with guidance:
$ sudo partresize resize /home --size 300G
error: cannot perform this operation while /home is mounted.
Filesystem: ext4
ext4 shrinking requires the filesystem to be offline.
Suggested solution:
    partresize resize /home --size 300G --offline   (unmount now)
    partresize resize /home --size 300G --schedule  (do it at next reboot)

# --offline unmounts, resizes offline, and remounts:
$ sudo partresize resize /home --size 300G --offline
```

## Shrink the root filesystem (reboot-assisted)

```sh
$ sudo partresize resize / --size 40G --schedule
The filesystem cannot be safely shrunk while mounted.
A temporary offline resize environment will be created using
your existing Linux installation. No USB or external OS is required.
Resize plan:
    1. Check filesystem integrity
    …
Prepare this operation for the next reboot? [y/N] y
Preparing resize environment...
Resize operation scheduled (id op-1787421126).
Boot integration: initramfs-tools + grub (kernel cmdline marker: partresize.maint=op-…).
Reboot to perform the resize. Run `partresize cancel` to abort beforehand.
```

## Dry run

```sh
$ sudo partresize resize /home --size 300G --offline --dry-run
--- DRY RUN (no changes will be made) ---
…
Operations that would execute:
[dry-run] e2fsck -f -y /dev/mapper/ubuntu--vg-home
[dry-run] resize2fs /dev/mapper/ubuntu--vg-home 314572800K
[dry-run] lvreduce -f -L 322122547200b /dev/mapper/ubuntu--vg-home
Dry run complete.
```

## Partition-table backup / restore

```sh
$ sudo partresize backup-table /dev/nvme0n1 ./nvme0n1.sfdisk
$ sudo partresize restore-table /dev/nvme0n1 ./nvme0n1.sfdisk
```

## Manage a scheduled/in-progress operation

```sh
$ sudo partresize status
$ sudo partresize cancel     # before the reboot
$ sudo partresize recover    # after an interrupted maintenance pass
```
