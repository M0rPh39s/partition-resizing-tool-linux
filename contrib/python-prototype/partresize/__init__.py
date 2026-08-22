"""partresize — resize the partition of a running Linux system without external media.

The package is organised into small, focused modules:

* :mod:`partresize.sizes`         — human-readable size parsing/formatting.
* :mod:`partresize.run`           — subprocess helpers.
* :mod:`partresize.model`         — dataclasses describing topology, plans, state.
* :mod:`partresize.topology`      — discovery of the full storage stack.
* :mod:`partresize.capabilities`  — the online/offline grow/shrink matrix.
* :mod:`partresize.planner`       — turns a request into a :class:`ResizePlan`.
* :mod:`partresize.filesystems`   — per-filesystem resize drivers.
* :mod:`partresize.partition`     — partition-table operations.
* :mod:`partresize.blockdev`      — LVM / LUKS layer operations.
* :mod:`partresize.state`         — the persistent recovery state machine.
* :mod:`partresize.boot`          — the distribution-independent BootEnvironment.
* :mod:`partresize.maintenance`   — the executor that runs offline in the initramfs.
* :mod:`partresize.cli`           — the command line interface.
"""

__version__ = "0.1.0"
