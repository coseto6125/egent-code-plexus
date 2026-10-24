"""Tests for the dirty-file scenario's file handling and exit code.

They call the real `_bench_dirty`, `_restore_file` and `_dirty_exit_code`;
only `_run` (the `ecp find` subprocess) is replaced by an in-memory stand-in.
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import benchmark_ecp as bench  # noqa: E402

ORIGINAL = b"export const a = 1;\n"
APPEND = "\nexport function bench_probe() {}\n"
TIMES_NS = (1_000_000_000, 2_000_000_000)


def _fake_run(on_call=None, rc_after=None):
    """Stand-in for `_run`: counts calls, runs `on_call(n)`, can fail after N calls."""
    calls = {"n": 0}

    def fake(cmd, cwd, env=None):
        calls["n"] += 1
        if on_call:
            on_call(calls["n"])
        rc = 1 if rc_after is not None and calls["n"] > rc_after else 0
        return 0.01, rc, "", "boom" if rc else ""

    return fake


def _target(tmp_path: Path) -> Path:
    t = tmp_path / "a.ts"
    t.write_bytes(ORIGINAL)
    return t


def test_restore_file_unchanged_since_script_wrote_restores_original(tmp_path):
    t = _target(tmp_path)
    t.write_bytes(b"edited")
    assert bench._restore_file(t, ORIGINAL, b"edited", TIMES_NS)
    assert t.read_bytes() == ORIGINAL
    assert t.stat().st_mtime_ns == TIMES_NS[1]


def test_restore_file_user_saved_meanwhile_leaves_file_and_warns_with_path(tmp_path, capsys):
    t = _target(tmp_path)
    t.write_bytes(b"user edit")
    assert not bench._restore_file(t, ORIGINAL, b"edited", TIMES_NS)
    assert t.read_bytes() == b"user edit"
    assert str(t) in capsys.readouterr().err


def test_restore_file_deleted_by_user_stays_deleted_and_warns(tmp_path, capsys):
    t = _target(tmp_path)
    t.unlink()
    assert not bench._restore_file(t, ORIGINAL, b"edited", TIMES_NS)
    assert not t.exists()
    assert str(t) in capsys.readouterr().err


def test_bench_dirty_clean_run_restores_original_bytes(tmp_path, monkeypatch):
    t = _target(tmp_path)
    monkeypatch.setattr(bench, "_run", _fake_run())
    samples = bench._bench_dirty(Path("ecp"), tmp_path, "a.ts", "q", None, APPEND, 2)
    assert t.read_bytes() == ORIGINAL
    assert all(len(s.runs) == 2 and s.err is None for s in samples)


def test_bench_dirty_user_save_during_run_survives_and_run_reports_error(
    tmp_path, monkeypatch, capsys
):
    t = _target(tmp_path)
    # Call 2 is the first query after the script's edit; the user saves then.
    monkeypatch.setattr(
        bench, "_run", _fake_run(lambda n: t.write_bytes(b"user edit") if n == 2 else None)
    )
    samples = bench._bench_dirty(Path("ecp"), tmp_path, "a.ts", "q", None, APPEND, 2)
    assert t.read_bytes() == b"user edit"
    assert str(t) in capsys.readouterr().err
    assert bench._dirty_exit_code(samples, {"protocol": 1}) != 0


def test_bench_dirty_file_deleted_during_run_is_not_recreated(tmp_path, monkeypatch, capsys):
    t = _target(tmp_path)
    monkeypatch.setattr(bench, "_run", _fake_run(lambda n: t.unlink() if n == 2 else None))
    bench._bench_dirty(Path("ecp"), tmp_path, "a.ts", "q", None, APPEND, 1)
    assert not t.exists()
    assert str(t) in capsys.readouterr().err


def test_dirty_exit_code_failure_in_later_round_is_nonzero(tmp_path, monkeypatch):
    _target(tmp_path)
    # Round 0 (warm-up) and round 1 take 4 calls each; round 2 fails on its 2nd call.
    monkeypatch.setattr(bench, "_run", _fake_run(rc_after=9))
    samples = bench._bench_dirty(Path("ecp"), tmp_path, "a.ts", "q", None, APPEND, 3)
    assert all(s.runs for s in samples), "every phase kept the runs of earlier rounds"
    assert any(s.err for s in samples)
    assert bench._dirty_exit_code(samples, {"protocol": 1}) != 0


def test_dirty_exit_code_all_rounds_ok_is_zero(tmp_path, monkeypatch):
    _target(tmp_path)
    monkeypatch.setattr(bench, "_run", _fake_run())
    samples = bench._bench_dirty(Path("ecp"), tmp_path, "a.ts", "q", None, APPEND, 2)
    assert bench._dirty_exit_code(samples, {"protocol": 1}) == 0


def test_dirty_exit_code_without_protocol_is_nonzero():
    s = bench.Sample(name="x", cmd=[])
    s.runs.append(0.1)
    assert bench._dirty_exit_code([s], None) != 0
