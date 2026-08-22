# Troubleshooting

### "required utility 'X' is not installed"
Install the package that provides it: `util-linux` (lsblk, findmnt, blkid,
sfdisk, partprobe, blockdev), `e2fsprogs` (ext), `xfsprogs` (XFS),
`btrfs-progs` (Btrfs), `lvm2` (LVM), `cryptsetup` (LUKS).

### "cannot perform this operation while /… is mounted"
The filesystem needs to be offline (e.g. an ext4 shrink). Choose one:
- `--offline` — unmount now, resize, remount (non-root targets).
- `--schedule` — do it during a one-time maintenance boot (works for `/`).

### "xyz cannot be shrunk (it only supports growing)"
XFS (and some others) cannot shrink at all. Move the data to a smaller new
filesystem instead; the tool will not fake a shrink.

### "target size … leaves less than the required safety margin"
The requested size is too close to the used data. Free space first, or choose a
larger target. The margin is 10% of used data, at least 256 MiB.

### "the partition cannot be extended because /dev/…p4 occupies the space"
There is no free space immediately after the partition; growing would require
moving the next partition. `partresize` will not overwrite or move it
automatically. Free/move the neighbouring partition first (with appropriate
backups), or grow into free space elsewhere via LVM.

### "this operation requires root privileges"
Run with `sudo`.

### "device '…' is not a recognised disposable test device"
`PARTRESIZE_TEST_MODE=1` is set (as in the test suite). In that mode only
loop/dm devices may be touched. Unset it for real use.

### A scheduled resize didn't run after reboot
Check `partresize status`. The one-time boot entry is self-reverting; if the
maintenance pass did not complete, `partresize recover` reports the state.
Ensure the initramfs actually contains the required tools (the maintenance
image copies `e2fsprogs`/`lvm`/`cryptsetup`/`sfdisk` as needed).

### An operation was interrupted (power loss)
Run `partresize recover`. Because the filesystem is always shrunk before the
partition, the filesystem is never left larger than its partition, so the layout
is in a safe, mountable state. Recovery reports what remains.

### Verbose output
Add `-v` to print every external command as it runs.
