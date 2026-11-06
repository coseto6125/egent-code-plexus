"""Tests for the dirty-file scenario's file handling and exit code.

They call the real `_bench_dirty`, `_restore_file` and `_dirty_exit_code`;
only `_run` (the `ecp find` subprocess) is replaced by an in-memory stand-in.
"""

import sys

import pytest
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


def test_bench_dirty_several_files_each_edited_then_restored(tmp_path, monkeypatch):
    files = [tmp_path / f"f{i}.ts" for i in range(20)]
    for f in files:
        f.write_bytes(ORIGINAL)
    seen: list[list[bytes]] = []
    monkeypatch.setattr(
        bench, "_run", _fake_run(lambda n: seen.append([f.read_bytes() for f in files]))
    )
    bench._bench_dirty(Path("ecp"), tmp_path, [f.name for f in files], "q", None, APPEND, 1)
    # Call 2 is the first query after the edit: every file carries the edit.
    assert seen[1] == [ORIGINAL + APPEND.encode()] * 20
    assert [f.read_bytes() for f in files] == [ORIGINAL] * 20


def test_bench_dirty_same_path_twice_is_edited_once(tmp_path, monkeypatch):
    t = _target(tmp_path)
    seen: list[bytes] = []
    monkeypatch.setattr(bench, "_run", _fake_run(lambda n: seen.append(t.read_bytes())))
    bench._bench_dirty(Path("ecp"), tmp_path, ["a.ts", "a.ts"], "q", None, APPEND, 1)
    assert seen[1] == ORIGINAL + APPEND.encode()
    assert t.read_bytes() == ORIGINAL


def test_bench_dirty_two_spellings_of_one_path_are_edited_once(tmp_path, monkeypatch):
    t = _target(tmp_path)
    seen: list[bytes] = []
    monkeypatch.setattr(bench, "_run", _fake_run(lambda n: seen.append(t.read_bytes())))
    samples = bench._bench_dirty(Path("ecp"), tmp_path, ["a.ts", "./a.ts"], "q", None, APPEND, 2)
    assert seen[1] == ORIGINAL + APPEND.encode()
    assert t.read_bytes() == ORIGINAL
    assert bench._dirty_exit_code(samples, {"protocol": 1}) == 0


def test_bench_dirty_user_save_to_one_of_two_files_keeps_it_and_restores_the_other(
    tmp_path, monkeypatch, capsys
):
    a = _target(tmp_path)
    b = tmp_path / "b.ts"
    b.write_bytes(ORIGINAL)
    monkeypatch.setattr(
        bench, "_run", _fake_run(lambda n: b.write_bytes(b"user edit") if n == 2 else None)
    )
    samples = bench._bench_dirty(Path("ecp"), tmp_path, ["a.ts", "b.ts"], "q", None, APPEND, 2)
    assert a.read_bytes() == ORIGINAL
    assert b.read_bytes() == b"user edit"
    assert "b.ts" in capsys.readouterr().err
    assert bench._dirty_exit_code(samples, {"protocol": 1}) != 0


def test_bench_dirty_run_raises_mid_way_restores_every_file(tmp_path, monkeypatch):
    a = _target(tmp_path)
    b = tmp_path / "b.ts"
    b.write_bytes(ORIGINAL)

    def boom(n):
        if n == 2:
            raise RuntimeError("killed")

    monkeypatch.setattr(bench, "_run", _fake_run(boom))
    try:
        bench._bench_dirty(Path("ecp"), tmp_path, ["a.ts", "b.ts"], "q", None, APPEND, 1)
    except RuntimeError:
        pass
    assert [a.read_bytes(), b.read_bytes()] == [ORIGINAL, ORIGINAL]


def test_dirty_phase_missing_path_exits_before_first_run(tmp_path, monkeypatch, capsys):
    _target(tmp_path)
    monkeypatch.setattr(bench, "_run", _fake_run(lambda n: pytest.fail("ran a command")))
    args = bench.argparse.Namespace(
        dirty_repo=tmp_path, dirty_file=["a.ts", "gone.ts"], dirty_append=None
    )
    assert bench._dirty_phase(args) == ([], None)
    assert "gone.ts" in capsys.readouterr().err


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


def _cypher_stdout(stdout, rc=0):
    def fake(cmd, cwd, env=None):
        return 0.01, rc, stdout, ""

    return fake


@pytest.mark.parametrize(
    ("stdout", "rc", "expected"),
    [
        ('{"columns":["count(f)"],"rows":[111]}', 0, 111),
        ('{"columns":["count(f)"],"rows":[[7]]}', 0, 7),
        ('{"columns":["count(f)"],"rows":[0]}', 0, 0),
        ('{"columns":["count(f)"],"rows":[]}', 0, None),
        ("not json", 0, None),
        ("", 0, None),
        ('{"rows":[111]}', 1, None),
    ],
)
def test_indexed_file_count_cypher_output_returns_count_or_none(monkeypatch, stdout, rc, expected):
    monkeypatch.setattr(bench, "_run", _cypher_stdout(stdout, rc))
    assert bench._indexed_file_count(Path("ecp"), Path(".")) == expected


@pytest.mark.parametrize(
    ("count", "message"),
    [(None, "could not count the files"), (0, "holds 0 files")],
)
def test_main_index_file_count_unknown_or_zero_exits_before_queries(
    tmp_path, monkeypatch, capsys, count, message
):
    binary = tmp_path / "ecp"
    binary.write_bytes(b"")
    repo = tmp_path / "repo"
    repo.mkdir()
    monkeypatch.setattr(
        sys, "argv", ["bench", "--binary", str(binary), "--repo", str(repo), "--git-repo", str(repo)]
    )
    monkeypatch.setattr(bench, "_ensure_binary_fresh", lambda binary, skip: None)
    monkeypatch.setattr(bench, "_hardware", lambda: {})
    monkeypatch.setattr(bench, "_admin_drop", lambda binary, repo: None)
    monkeypatch.setattr(bench, "_run", lambda cmd, cwd, env=None: (0.01, 0, "", ""))
    monkeypatch.setattr(bench, "_indexed_file_count", lambda binary, repo: count)
    assert bench.main() == 2
    assert message in capsys.readouterr().err
