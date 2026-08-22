"""State machine persistence + maintenance script generation."""

from __future__ import annotations

from partresize import planner
from partresize.maintenance import generate_script, required_binaries
from partresize.state import OperationState, State, StateStore
from tests.test_planner import make_topo

GiB = 1024 * 1024 * 1024


def _plan(monkeypatch, **kw):
    monkeypatch.setattr(planner, "_measure",
                        lambda t: (120 * GiB, 42 * GiB, 4096, 10 * GiB))
    topo = make_topo(**kw)
    return planner.build_plan(topo, direction="shrink", size=80 * GiB)


class TestState:
    def test_roundtrip(self, monkeypatch, state_dir):
        plan = _plan(monkeypatch)
        store = StateStore(state_dir)
        op = OperationState("op1", State.PREPARED, plan)
        store.save(op)
        loaded = store.load()
        assert loaded is not None
        assert loaded.op_id == "op1"
        assert loaded.state == State.PREPARED
        assert loaded.plan.target_fs_size == plan.target_fs_size

    def test_transitions_recorded(self, monkeypatch, state_dir):
        plan = _plan(monkeypatch)
        store = StateStore(state_dir)
        op = OperationState("op2", State.PREPARED, plan)
        store.save(op)
        store.transition(op, State.BOOTING_MAINTENANCE)
        store.transition(op, State.SHRINKING_FILESYSTEM)
        reloaded = store.load()
        assert reloaded.state == State.SHRINKING_FILESYSTEM
        assert len(reloaded.history) == 2

    def test_clear(self, monkeypatch, state_dir):
        plan = _plan(monkeypatch)
        store = StateStore(state_dir)
        store.save(OperationState("op3", State.PREPARED, plan))
        assert store.exists()
        store.clear()
        assert not store.exists()
        assert store.load() is None


class TestMaintenanceScript:
    def test_required_binaries_ext4(self, monkeypatch):
        plan = _plan(monkeypatch)
        bins = required_binaries(plan)
        assert "resize2fs" in bins and "e2fsck" in bins and "sfdisk" in bins

    def test_required_binaries_lvm_luks(self, monkeypatch):
        plan = _plan(monkeypatch, with_lvm=True, with_luks=True)
        bins = required_binaries(plan)
        assert "cryptsetup" in bins
        assert "lvresize" in bins

    def test_script_orders_fs_before_partition(self, monkeypatch):
        plan = _plan(monkeypatch)
        script = generate_script(plan, "op-xyz")
        assert "#!/bin/sh" in script
        assert "op-xyz" in script
        i_fs = script.index("Shrinking filesystem")
        i_part = script.index("Resizing partition")
        assert i_fs < i_part
        # must ensure the target is unmounted and record state
        assert "ensure_unmounted" in script
        assert "record_state COMPLETED" in script

    def test_script_has_resize2fs_call(self, monkeypatch):
        plan = _plan(monkeypatch)
        script = generate_script(plan, "op1")
        assert "resize2fs" in script

    def test_script_is_valid_posix_shell(self, monkeypatch, tmp_path):
        import shutil
        import subprocess
        sh = shutil.which("sh")
        if not sh:
            return
        for kw in ({}, {"with_lvm": True, "with_luks": True}):
            plan = _plan(monkeypatch, **kw)
            script = tmp_path / "m.sh"
            script.write_text(generate_script(plan, "opX"))
            res = subprocess.run([sh, "-n", str(script)],
                                 capture_output=True, text=True)
            assert res.returncode == 0, res.stderr

    def test_lvm_luks_shrink_layer_order(self, monkeypatch):
        # fs -> LV -> PV -> LUKS -> partition, innermost container first
        plan = _plan(monkeypatch, with_lvm=True, with_luks=True)
        script = generate_script(plan, "op1")
        i_fs = script.index("Shrinking filesystem")
        i_lv = script.index("Shrinking LV")
        i_luks = script.index("Shrinking LUKS")
        i_part = script.index("Resizing partition")
        assert i_fs < i_lv < i_luks < i_part
