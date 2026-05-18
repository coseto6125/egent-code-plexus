"""Unit tests for `_pair_route_method_prefix` in parity_aggregate.py.

gnx-rs emits Route node names as `"METHOD path"` (e.g., `"GET /users"`),
flattened from the underlying `RawRoute { method, path }`. ref-gitnexus emits
the bare path (`"/users"`). The helper strips the leading HTTP-method prefix
from rs-side names and pairs `(p, normalized_name)` against ref side. Verified
shape on `.sample_repo` 2026-05-19: 20 JavaScript routes pair this way; the
remaining 62 ref Route rows are routes-inside-test-callbacks (function-body
design drops).

Invoke: `python3 -m pytest scripts/parity/test_route_method_prefix.py`
"""
from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

import parity_aggregate as pa


def _run(rs_rows, ref_rows):
    return pa._pair_route_method_prefix(set(rs_rows), set(ref_rows))


def test_basic_get_route_pair():
    rs = [("Route", "app.js", "GET /users")]
    ref = [("Route", "app.js", "/users")]
    new_rs, new_ref, pairs = _run(rs, ref)
    assert pairs == 1
    assert new_rs == set()
    assert new_ref == set()


def test_covers_all_http_methods():
    methods = ["GET", "POST", "PUT", "DELETE", "PATCH", "OPTIONS", "HEAD", "USE", "ALL"]
    for m in methods:
        rs = [("Route", "f.js", f"{m} /x")]
        ref = [("Route", "f.js", "/x")]
        _, _, pairs = _run(rs, ref)
        assert pairs == 1, f"method prefix {m!r} did not pair"


def test_no_pair_when_path_differs():
    rs = [("Route", "f.js", "GET /a")]
    ref = [("Route", "f.js", "/b")]
    new_rs, new_ref, pairs = _run(rs, ref)
    assert pairs == 0
    assert new_rs == set(rs)
    assert new_ref == set(ref)


def test_no_pair_when_file_differs():
    rs = [("Route", "a.js", "GET /x")]
    ref = [("Route", "b.js", "/x")]
    new_rs, new_ref, pairs = _run(rs, ref)
    assert pairs == 0


def test_rs_without_method_prefix_skipped():
    # rs Route lacking a METHOD prefix is already in ref's naming convention
    # — the EQUIV path would've paired it elsewhere. Don't double-pair here.
    rs = [("Route", "f.js", "/users")]
    ref = [("Route", "f.js", "/users")]
    new_rs, new_ref, pairs = _run(rs, ref)
    assert pairs == 0
    assert new_rs == set(rs)
    assert new_ref == set(ref)


def test_one_rs_pairs_at_most_once():
    # Two ref rows at the same (p, n) shouldn't double-consume one rs row.
    rs = [("Route", "f.js", "GET /api")]
    ref = [
        ("Route", "f.js", "/api"),
        ("Route", "g.js", "/api"),
    ]
    new_rs, new_ref, pairs = _run(rs, ref)
    assert pairs == 1
    assert new_rs == set()
    assert ("Route", "g.js", "/api") in new_ref


def test_non_route_kinds_untouched():
    rs = [("Function", "f.js", "GET /users"), ("Route", "f.js", "GET /api")]
    ref = [("Function", "f.js", "/users"), ("Route", "f.js", "/api")]
    new_rs, new_ref, pairs = _run(rs, ref)
    assert pairs == 1
    assert ("Function", "f.js", "GET /users") in new_rs
    assert ("Function", "f.js", "/users") in new_ref


def test_pair_with_parameterized_path():
    rs = [("Route", "f.js", "POST /users/:id")]
    ref = [("Route", "f.js", "/users/:id")]
    _, _, pairs = _run(rs, ref)
    assert pairs == 1
