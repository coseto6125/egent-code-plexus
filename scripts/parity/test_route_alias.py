"""Unit tests for `_pair_route_aliases` in parity_aggregate.py / review_diffs.py.

Both scripts ship their own copy of the helper (intentional — they're stand-alone
CLIs without a shared package). Tests run against both to keep them in sync.

Invoke: `python3 -m pytest scripts/parity/test_route_alias.py`
"""
from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

import parity_aggregate
import review_diffs


def _run(impl_pair, rs_rows, ref_rows):
    rs_only = set(rs_rows)
    ref_only = set(ref_rows)
    return impl_pair(rs_only, ref_only)


def test_basic_pair_per_file():
    rs = [("EntryPoint", "a.py", "route@index")]
    ref = [("Route", "a.py", "/index")]
    new_rs, new_ref, pairs = _run(parity_aggregate._pair_route_aliases, rs, ref)
    assert new_rs == set()
    assert new_ref == set()
    assert pairs == 1


def test_only_route_prefix_pairs():
    # main@main is not a route — must not pair with ref Route rows.
    rs = [("EntryPoint", "a.py", "main@main")]
    ref = [("Route", "a.py", "/index")]
    new_rs, new_ref, pairs = _run(parity_aggregate._pair_route_aliases, rs, ref)
    assert pairs == 0
    assert new_rs == set(rs)
    assert new_ref == set(ref)


def test_pair_min_per_file():
    rs = [
        ("EntryPoint", "a.py", "route@a"),
        ("EntryPoint", "a.py", "route@b"),
        ("EntryPoint", "a.py", "route@c"),
    ]
    ref = [
        ("Route", "a.py", "/x"),
        ("Route", "a.py", "/y"),
    ]
    _, _, pairs = _run(parity_aggregate._pair_route_aliases, rs, ref)
    # min(3, 2) = 2 pairs.
    assert pairs == 2


def test_no_cross_file_pairing():
    rs = [("EntryPoint", "a.py", "route@index")]
    ref = [("Route", "b.py", "/index")]
    new_rs, new_ref, pairs = _run(parity_aggregate._pair_route_aliases, rs, ref)
    assert pairs == 0
    assert new_rs == set(rs)
    assert new_ref == set(ref)


def test_nonroute_kinds_untouched():
    # EQUIV pairing is the aggregator's job; the route-alias helper only
    # touches `Route` ↔ `EntryPoint(route@*)`.
    rs = [
        ("Function", "a.py", "helper"),
        ("EntryPoint", "a.py", "route@index"),
    ]
    ref = [
        ("Method", "a.py", "helper"),
        ("Route", "a.py", "/index"),
    ]
    new_rs, new_ref, pairs = _run(parity_aggregate._pair_route_aliases, rs, ref)
    assert pairs == 1
    assert ("Function", "a.py", "helper") in new_rs
    assert ("Method", "a.py", "helper") in new_ref


def test_review_diffs_helper_returns_pair_rows():
    # review_diffs.py needs the actual paired rows (rs_row, ref_row) so it
    # can build a label entry; the aggregator only needs the count.
    rs = [("EntryPoint", "a.py", "route@index")]
    ref = [("Route", "a.py", "/index")]
    new_rs, new_ref, pairs = _run(review_diffs._pair_route_aliases, rs, ref)
    assert new_rs == set()
    assert new_ref == set()
    assert pairs == [
        (("EntryPoint", "a.py", "route@index"), ("Route", "a.py", "/index")),
    ]


def test_both_helpers_agree_on_count():
    # Sanity guard against the two copies drifting apart.
    # x.py: 2 rs vs 1 ref → 1 pair; y.py: 1 rs vs 2 ref → 1 pair; total = 2.
    rs = [
        ("EntryPoint", "x.py", "route@a"),
        ("EntryPoint", "x.py", "route@b"),
        ("EntryPoint", "y.py", "route@c"),
    ]
    ref = [
        ("Route", "x.py", "/a"),
        ("Route", "y.py", "/c"),
        ("Route", "y.py", "/d"),
    ]
    _, _, agg_n = _run(parity_aggregate._pair_route_aliases, rs, ref)
    _, _, rev_pairs = _run(review_diffs._pair_route_aliases, rs, ref)
    assert agg_n == len(rev_pairs) == 2
