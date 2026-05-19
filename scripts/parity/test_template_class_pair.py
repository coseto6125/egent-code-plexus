"""Unit tests for `_pair_ref_template_class_double_emit` in parity_aggregate.py.

The helper pairs ref-gitnexus's double-emit of `template<typename T> class Foo`
as both `Class` AND `Template` at the same `(p, n)` against cgn-rs's single
`Class` emission. Verified shape on `.sample_repo` 2026-05-19 (PR #152
follow-up): 19 Cpp template-class definitions that previously surfaced as
`real_ref` ref_over Template-* rows now classify as `label_diff`.

Invoke: `python3 -m pytest scripts/parity/test_template_class_pair.py`
"""
from __future__ import annotations

import sys
from collections import defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

import parity_aggregate as pa


def _build_by_pn(rows):
    by_pn = defaultdict(list)
    for k, p, n in rows:
        by_pn[(p, n)].append(k)
    return by_pn


def test_pairs_ref_template_with_rs_class():
    rs_all = [("Class", "lexer.hpp", "lexer_base")]
    ref_all = [
        ("Class", "lexer.hpp", "lexer_base"),
        ("Template", "lexer.hpp", "lexer_base"),
    ]
    rs_by_pn = _build_by_pn(rs_all)
    ref_by_pn = _build_by_pn(ref_all)
    ref_only = {("Template", "lexer.hpp", "lexer_base")}
    new_ref, pairs = pa._pair_ref_template_class_double_emit(
        ref_only, rs_by_pn, ref_by_pn
    )
    assert pairs == 1
    assert new_ref == set()


def test_does_not_pair_without_rs_type_kind():
    # If cgn-rs has nothing of the type-family at (p, n), the ref `Template`
    # row is a real missing emission — do not classify as label_diff.
    rs_all = [("Function", "tpl.hpp", "make_widget")]
    ref_all = [("Template", "tpl.hpp", "make_widget")]
    rs_by_pn = _build_by_pn(rs_all)
    ref_by_pn = _build_by_pn(ref_all)
    ref_only = {("Template", "tpl.hpp", "make_widget")}
    new_ref, pairs = pa._pair_ref_template_class_double_emit(
        ref_only, rs_by_pn, ref_by_pn
    )
    assert pairs == 0
    assert new_ref == ref_only


def test_does_not_pair_when_ref_lacks_class_double_emit():
    # ref-gitnexus must ALSO have a type-family entry at (p, n) — that's the
    # load-bearing signal it's a double-emit. A bare ref `Template` without a
    # sibling `Class` row may be a template function (where EQUIV's
    # `{Method, Function, Template, Constructor}` class already handles
    # pairing); leave it alone.
    rs_all = [("Class", "x.hpp", "Foo")]
    ref_all = [("Template", "x.hpp", "Foo")]
    rs_by_pn = _build_by_pn(rs_all)
    ref_by_pn = _build_by_pn(ref_all)
    ref_only = {("Template", "x.hpp", "Foo")}
    new_ref, pairs = pa._pair_ref_template_class_double_emit(
        ref_only, rs_by_pn, ref_by_pn
    )
    assert pairs == 0
    assert new_ref == ref_only


def test_pairs_struct_and_other_type_kinds():
    # `template<typename T> struct S` / `template<typename E> enum E` also
    # double-emit on ref-gitnexus. Cover the full _TEMPLATE_TYPE_PAIR_KINDS
    # set so future ref-gitnexus changes that emit Trait/Union for templates
    # auto-pair.
    cases = [("Struct", "s.hpp", "S"), ("Enum", "e.hpp", "E"), ("Union", "u.hpp", "U")]
    for kind, p, n in cases:
        rs_all = [(kind, p, n)]
        ref_all = [(kind, p, n), ("Template", p, n)]
        rs_by_pn = _build_by_pn(rs_all)
        ref_by_pn = _build_by_pn(ref_all)
        ref_only = {("Template", p, n)}
        new_ref, pairs = pa._pair_ref_template_class_double_emit(
            ref_only, rs_by_pn, ref_by_pn
        )
        assert pairs == 1, f"failed to pair Template ↔ {kind}"
        assert new_ref == set()


def test_does_not_pair_non_template_rows():
    # Only Template rows should be candidates. Plain Class / Variable / etc.
    # in ref_only must remain untouched even when rs has a matching kind.
    rs_all = [("Class", "x.hpp", "Bar")]
    ref_all = [("Class", "x.hpp", "Bar")]
    rs_by_pn = _build_by_pn(rs_all)
    ref_by_pn = _build_by_pn(ref_all)
    ref_only = {("Class", "x.hpp", "Bar")}  # contrived — wouldn't normally be in ref_only
    new_ref, pairs = pa._pair_ref_template_class_double_emit(
        ref_only, rs_by_pn, ref_by_pn
    )
    assert pairs == 0
    assert new_ref == ref_only
