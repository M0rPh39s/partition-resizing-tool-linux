# Developer guide

## Layout

```
src/
  cli/           argument parsing, dispatch, output
  errors/        Error enum (admin-friendly messages)
  discovery/     lsblk/findmnt/blkid → Topology
  storage/       Layer / Topology model
  filesystem/    capability matrix + ext/xfs/btrfs drivers
  partition/     sfdisk geometry, resize, table backup/restore
  lvm/           PV/VG/LV operations + metadata backup
  crypt/         LUKS resize
  planner/       Topology + Request → ResizePlan (pure)
  executor/      run a validated plan; state + backups
  safety/        guards (privilege, sizes, test-device)
  recovery/      state machine + backup dirs
  verification/  post-resize invariant checks
  schedule/      offline execution + initramfs/bootloader integration
  util/          command runner, size parse/format
tests/
  loopback.rs    integration tests driving the binary on loop devices
```

## Build & static analysis

```sh
cargo build                 # debug
cargo build --release       # optimised binary
cargo fmt --all             # format
cargo clippy --all-targets  # lints (the tree is clippy-clean)
```

The crate forbids `unsafe` (`[lints.rust] unsafe_code = "forbid"`).

## Tests

```sh
# Unit tests (no root needed): sizes, safety guards.
cargo test --bin partresize

# Full suite incl. loop-device integration (needs root + loop devices).
sudo -E cargo test -- --test-threads=1
```

Integration tests (`tests/loopback.rs`):

- create disposable `disk.img` files attached as **loop devices**;
- run the **real compiled binary** (`CARGO_BIN_EXE_partresize`);
- assert filesystem/partition sizes, data integrity, refusals, dry-run, and
  scheduling artefacts;
- self-skip when not root, or when a kernel feature is missing (LVM/LUKS need
  device-mapper; Btrfs/XFS need the kernel module).

Every test sets `PARTRESIZE_TEST_MODE=1`, which makes the binary **refuse any
device that is not a loop/dm test device** — so a bug cannot damage the host.
`PARTRESIZE_MIN_MARGIN` is lowered so small images can be shrunk tightly.

### Environment variables used in tests

| Variable | Purpose |
|---|---|
| `PARTRESIZE_TEST_MODE=1` | Arm the device guard (loop/dm only). |
| `PARTRESIZE_MIN_MARGIN=<bytes>` | Lower the shrink safety margin for tiny images. |
| `PARTRESIZE_STATE_DIR=<dir>` | Redirect the state/backup directory. |
| `PARTRESIZE_PREFIX=<dir>` | Sandbox all boot-integration writes under a prefix. |

## Adding a filesystem driver

1. Add a module under `src/filesystem/` implementing `probe`, `check`, and the
   relevant `grow`/`shrink` primitives.
2. Wire it into the dispatch functions in `src/filesystem/mod.rs`.
3. Add a row to `capabilities_for` — be conservative; only claim what the
   filesystem can truly do.
4. Add a loopback test (guarded by `kernel_fs("<type>")`).

## Design rules

- The **planner never mutates**. All device changes go through the executor.
- The plan's operation order **is** the execution order; keep the shrink/grow
  orderings intact.
- Prefer standard utilities over ad-hoc ioctls unless a direct call is clearly
  safer.
- Every user-facing error should tell an admin what happened and, where useful,
  what to do next.
