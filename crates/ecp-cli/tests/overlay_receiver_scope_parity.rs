//! A member call or method value on a receiver whose type depends on the path
//! (a rebinding, a branch, a block-scoped local, a same-named class in another
//! language) out of a dirty file: the session overlay binds what a full
//! reindex of the edited tree binds, confidence included, and each reindex row
//! is also the row the item's contract asks for, so a cell cannot pass by both
//! sides being equally wrong.
//!
//! Decision A09: Python, PHP, Ruby and JavaScript are guess languages. A
//! receiver the reachable paths do not all type the same binds `m` by name as
//! the tagged guess `(0.55, "call:untyped-member")`; a method value never
//! guesses.
//!
//! Items: A02 (Python annotation in a sibling branch), A03 (Python caller,
//! TypeScript namesake), A04 (Python rebinding), A07 (Java block scope), A09
//! (PHP untyped receiver). Each item pairs its FORBIDDEN cells with a POSITIVE
//! CONTROL cell in one test, so "emit nothing" cannot pass.
//!
//! The parity helpers replace or add a file: they cannot delete one. A
//! delete cell empties the file instead (an empty file declares nothing).

use crate::parity;

use ecp_core::graph::RelType;
use parity::{edges_after_edit, parse, ReasonedEdge};

const FUNCTION_REFERENCE: &str = "function_reference";
const GUESS: (&str, f32) = ("call:untyped-member", 0.55);

/// What the reindex rows into the callee `callee` must be.
#[derive(Clone, Copy, Debug)]
enum Expect {
    /// No row at all.
    Nothing,
    /// One typed `call` row into this file, confidence 0.7 or above.
    Typed(&'static str),
    /// One tagged-guess row into this file.
    Guess(&'static str),
    /// `function_reference` rows into exactly these files, sorted.
    Refs(&'static [&'static str]),
    /// No row into this file (whatever else binds).
    Forbid(&'static str),
    /// Overlay equals reindex; the cell has no stated edge.
    ParityOnly,
}

struct Case {
    label: String,
    rel: RelType,
    caller: &'static str,
    callee: &'static str,
    files: Vec<(String, String)>,
    edits: Vec<(String, String)>,
    expect: Expect,
}

fn owned(files: &[(&str, &str)]) -> Vec<(String, String)> {
    files
        .iter()
        .map(|(path, src)| (path.to_string(), src.to_string()))
        .collect()
}

fn case(
    label: &str,
    rel: RelType,
    caller: &'static str,
    callee: &'static str,
    files: &[(&str, &str)],
    edits: &[(&str, &str)],
    expect: Expect,
) -> Case {
    Case {
        label: label.to_string(),
        rel,
        caller,
        callee,
        files: owned(files),
        edits: owned(edits),
        expect,
    }
}

/// `src` moved down one line: every span in the file shifts.
fn shifted(src: &str) -> String {
    format!("\n{src}")
}

type Row = (String, String, String, f32);

/// The failures of one case, one line each.
fn check(c: &Case) -> Vec<String> {
    let files: Vec<(&str, &str)> = c
        .files
        .iter()
        .map(|(p, s)| (p.as_str(), s.as_str()))
        .collect();
    let edits: Vec<(&str, &str)> = c
        .edits
        .iter()
        .map(|(p, s)| (p.as_str(), s.as_str()))
        .collect();
    let caller = c.caller;
    let parity::EdgeParity { reindex, overlay } =
        edges_after_edit(&parse, &files, &edits, c.rel, &move |_, name| {
            name == caller
        });
    let mut failures = Vec::new();
    let edge_of = |edges: &[ReasonedEdge]| edges.iter().map(|e| e.edge.clone()).collect::<Vec<_>>();
    if edge_of(&overlay) != edge_of(&reindex) {
        failures.push(format!(
            "{}: overlay {overlay:#?}\nreindex {reindex:#?}",
            c.label
        ));
    }
    let rows: Vec<Row> = reindex
        .iter()
        .filter(|e| e.edge.callee == c.callee)
        .map(|e| {
            (
                e.edge.callee_file.clone(),
                e.edge.callee.clone(),
                e.reason.clone(),
                e.edge.confidence,
            )
        })
        .collect();
    let bad = match c.expect {
        Expect::Nothing => !rows.is_empty(),
        Expect::Typed(file) => {
            !(rows.len() == 1 && rows[0].0 == file && rows[0].2 == "call" && rows[0].3 >= 0.7)
        }
        Expect::Guess(file) => {
            rows != [(
                file.to_string(),
                c.callee.to_string(),
                GUESS.0.to_string(),
                GUESS.1,
            )]
        }
        Expect::Refs(files) => {
            let got: Vec<&str> = rows
                .iter()
                .filter(|r| r.2 == FUNCTION_REFERENCE)
                .map(|r| r.0.as_str())
                .collect();
            got != files
        }
        Expect::Forbid(file) => rows.iter().any(|r| r.0 == file),
        Expect::ParityOnly => false,
    };
    if bad {
        failures.push(format!(
            "{}: expected {:?}, the reindex rows are {rows:?}",
            c.label, c.expect
        ));
    }
    failures
}

/// Every failure of `cases`; a list with no positive cell proves nothing.
fn failures_of(cases: &[Case]) -> Vec<String> {
    assert!(
        cases.iter().any(|c| matches!(
            c.expect,
            Expect::Typed(_) | Expect::Guess(_) | Expect::Refs(_)
        )),
        "a case list with no positive control proves nothing"
    );
    cases.iter().flat_map(check).collect()
}

fn assert_cases(cases: &[Case]) {
    let failures = failures_of(cases);
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

// ---- Python fixtures ----

const CLS_PY: &str = "class Cls:\n    def m(self, x):\n        return x\n";
const CLS_PY_RENAMED_METHOD: &str = "class Cls:\n    def n(self, x):\n        return x\n";
const CLS_PY_RENAMED_CLASS: &str = "class Cls2:\n    def m(self, x):\n        return x\n";
const CLS_PY_WITH_OTHER_FIRST: &str =
    "class Other:\n    def m(self, x):\n        return x\n\n\nclass Cls:\n    def m(self, x):\n        return x\n";
const OTHER_PY: &str = "class Other:\n    def m(self, x):\n        return x\n";

const SCOPE_CALLS_NO_EDGE: &str = "from cls import Cls\n\ndef run(c, loose):\n    obj = loose\n    if c:\n        obj: Cls = Cls()\n    else:\n        return obj.m(1)\n";
const SCOPE_CALLS_JOIN_CLS: &str = "from cls import Cls\n\ndef run(c, loose):\n    obj = loose\n    if c:\n        obj: Cls = Cls()\n    else:\n        obj: Cls = Cls()\n        return obj.m(1)\n";
const SCOPE_REFS_NO_EDGE: &str = "from cls import Cls\n\ndef run(c, loose, xs):\n    obj = loose\n    if c:\n        obj: Cls = Cls()\n    else:\n        return list(map(obj.m, xs))\n";
const SCOPE_POS_BRANCH: &str = "from cls import Cls\n\ndef run(c, loose):\n    obj = loose\n    if c:\n        obj: Cls = Cls()\n        return obj.m(1)\n    return 0\n";
const SCOPE_POS_JOIN: &str = "from cls import Cls\n\ndef run(c):\n    if c:\n        obj: Cls = Cls()\n    else:\n        obj: Cls = Cls()\n    return obj.m(1)\n";
const SCOPE_POS_REFS: &str = "from cls import Cls\n\ndef run(c, loose, xs):\n    obj = loose\n    if c:\n        obj: Cls = Cls()\n        return list(map(obj.m, xs))\n    return 0\n";

/// Stands for (A02, section 4): the dirty-set matrix of the annotation in a
/// sibling branch. Caller shifted, rewritten to the joined `Cls` (the edge
/// appears), callee renamed, owner order changed, import changed, and the
/// positive controls in the same list.
#[test]
fn test_overlay_python_annotation_in_sibling_branch_matches_reindex() {
    let calls = |label: &str, files: &[(&str, &str)], edits: &[(&str, &str)], expect| {
        case(label, RelType::Calls, "run", "m", files, edits, expect)
    };
    let pair = [("cls.py", CLS_PY), ("use.py", SCOPE_CALLS_NO_EDGE)];
    let shifted_use = shifted(SCOPE_CALLS_NO_EDGE);
    let shifted_pos = shifted(SCOPE_POS_BRANCH);
    let shifted_join = shifted(SCOPE_POS_JOIN);
    let cases = vec![
        calls(
            "A02 caller shifted",
            &pair,
            &[("use.py", shifted_use.as_str())],
            Expect::Guess("cls.py"),
        ),
        calls(
            "A02 else branch rewritten to Cls: the edge appears",
            &pair,
            &[("use.py", SCOPE_CALLS_JOIN_CLS)],
            Expect::Typed("cls.py"),
        ),
        calls(
            "A02 callee renamed: the target vanishes",
            &pair,
            &[("cls.py", CLS_PY_RENAMED_METHOD)],
            Expect::Nothing,
        ),
        calls(
            "A02 owner gains an earlier namesake class: ambiguous",
            &pair,
            &[("cls.py", CLS_PY_WITH_OTHER_FIRST)],
            Expect::Nothing,
        ),
        calls(
            "A02 import changed to a module that does not exist",
            &pair,
            &[(
                "use.py",
                SCOPE_CALLS_NO_EDGE
                    .replace("from cls import", "from other import")
                    .as_str(),
            )],
            Expect::ParityOnly,
        ),
        calls(
            "A02 caller with the use in the annotated branch, stale guess goes",
            &[("cls.py", CLS_PY), ("use.py", SCOPE_POS_BRANCH)],
            &[("use.py", SCOPE_CALLS_NO_EDGE)],
            Expect::Guess("cls.py"),
        ),
        calls(
            "A02 control: use in the annotated branch, shifted",
            &[("cls.py", CLS_PY), ("use.py", SCOPE_POS_BRANCH)],
            &[("use.py", shifted_pos.as_str())],
            Expect::Typed("cls.py"),
        ),
        calls(
            "A02 control: both branches annotated, shifted",
            &[("cls.py", CLS_PY), ("use.py", SCOPE_POS_JOIN)],
            &[("use.py", shifted_join.as_str())],
            Expect::Typed("cls.py"),
        ),
        calls(
            "A02 caller and callee dirty together",
            &pair,
            &[
                ("use.py", SCOPE_POS_BRANCH),
                ("cls.py", CLS_PY_RENAMED_METHOD),
            ],
            Expect::Nothing,
        ),
    ];
    assert_cases(&cases);
}

/// Stands for (A02, method value): a method value never guesses, so the
/// sibling-branch fixture has no `References` edge on either side, and its
/// annotated-branch twin keeps one.
#[test]
fn test_overlay_python_annotation_in_sibling_branch_reference_matches_reindex() {
    let refs = |label: &str, files: &[(&str, &str)], edits: &[(&str, &str)], expect| {
        case(label, RelType::References, "run", "m", files, edits, expect)
    };
    let shifted_neg = shifted(SCOPE_REFS_NO_EDGE);
    let shifted_pos = shifted(SCOPE_POS_REFS);
    let cases = vec![
        refs(
            "A02 refs caller shifted",
            &[("cls.py", CLS_PY), ("use.py", SCOPE_REFS_NO_EDGE)],
            &[("use.py", shifted_neg.as_str())],
            Expect::Nothing,
        ),
        refs(
            "A02 refs control: annotated branch, shifted",
            &[("cls.py", CLS_PY), ("use.py", SCOPE_POS_REFS)],
            &[("use.py", shifted_pos.as_str())],
            Expect::Refs(&["cls.py"]),
        ),
        refs(
            "A02 refs stale edge masked",
            &[("cls.py", CLS_PY), ("use.py", SCOPE_POS_REFS)],
            &[("use.py", SCOPE_REFS_NO_EDGE)],
            Expect::Nothing,
        ),
    ];
    assert_cases(&cases);
}

// ---- A03: a typed receiver binds its own language ----

const XL_CLS_TS: &str =
    "export class Cls {\n  pad(): void {}\n  m(x: number): number {\n    return x;\n  }\n}\n";
const XL_CLS_TS_RENAMED_METHOD: &str =
    "export class Cls {\n  pad(): void {}\n  n(x: number): number {\n    return x;\n  }\n}\n";
const XL_USE_PY: &str = "from cls import Cls\n\ndef call(obj: Cls):\n    return obj.m(1)\n\ndef call_ref(obj: Cls, xs):\n    return list(map(obj.m, xs))\n";
const XL_USE_PY_M2: &str = "from cls import Cls\n\ndef call(obj: Cls):\n    return obj.m(2)\n\ndef call_ref(obj: Cls, xs):\n    return list(map(obj.m, xs))\n";
const XL_USE_TS: &str =
    "import { Cls } from './cls';\nexport function callTs(obj: Cls) {\n  return obj.m(1);\n}\n";

/// Stands for (A03, section 4): a Python caller never binds the TypeScript
/// `Cls.m`, whichever of the four files is dirty, and the TypeScript caller
/// keeps its own file.
#[test]
fn test_overlay_python_typed_receiver_with_ts_namesake_matches_reindex() {
    let trio = [
        ("cls.py", CLS_PY),
        ("cls.ts", XL_CLS_TS),
        ("use.py", XL_USE_PY),
    ];
    let call = |label: &str, files: &[(&str, &str)], edits: &[(&str, &str)], expect| {
        case(label, RelType::Calls, "call", "m", files, edits, expect)
    };
    let call_ref = |label: &str, files: &[(&str, &str)], edits: &[(&str, &str)], expect| {
        case(
            label,
            RelType::References,
            "call_ref",
            "m",
            files,
            edits,
            expect,
        )
    };
    let shifted_use = shifted(XL_USE_PY);
    let cases = vec![
        call(
            "A03 caller shifted",
            &trio,
            &[("use.py", shifted_use.as_str())],
            Expect::Typed("cls.py"),
        ),
        call(
            "A03 caller rewritten to obj.m(2)",
            &trio,
            &[("use.py", XL_USE_PY_M2)],
            Expect::Typed("cls.py"),
        ),
        call(
            "A03 TypeScript callee renamed",
            &trio,
            &[("cls.ts", XL_CLS_TS_RENAMED_METHOD)],
            Expect::Typed("cls.py"),
        ),
        call(
            "A03 Python owner class renamed: never falls to cls.ts",
            &trio,
            &[("cls.py", CLS_PY_RENAMED_CLASS)],
            Expect::Forbid("cls.ts"),
        ),
        call(
            "A03 import of a module that does not exist",
            &trio,
            &[(
                "use.py",
                XL_USE_PY
                    .replace("from cls import", "from cls2 import")
                    .as_str(),
            )],
            Expect::Forbid("cls.ts"),
        ),
        call(
            "A03 cls.py deleted (emptied): never cls.ts",
            &trio,
            &[("cls.py", "")],
            Expect::Forbid("cls.ts"),
        ),
        call(
            "A03 cls.ts deleted (emptied): the edge is unchanged",
            &trio,
            &[("cls.ts", "")],
            Expect::Typed("cls.py"),
        ),
        call(
            "A03 caller and callee dirty together",
            &trio,
            &[
                ("use.py", XL_USE_PY_M2),
                ("cls.ts", XL_CLS_TS_RENAMED_METHOD),
            ],
            Expect::Typed("cls.py"),
        ),
        call(
            "A03 caller and delete together: cls.py emptied, caller rewritten",
            &trio,
            &[("use.py", XL_USE_PY_M2), ("cls.py", "")],
            Expect::Forbid("cls.ts"),
        ),
        call_ref(
            "A03 method value, caller shifted",
            &trio,
            &[("use.py", shifted_use.as_str())],
            Expect::Refs(&["cls.py"]),
        ),
        call_ref(
            "A03 method value, cls.py emptied: never cls.ts",
            &trio,
            &[("cls.py", "")],
            Expect::Forbid("cls.ts"),
        ),
        case(
            "A03 control: the TypeScript caller keeps cls.ts",
            RelType::Calls,
            "callTs",
            "m",
            &[
                ("cls.py", CLS_PY),
                ("cls.ts", XL_CLS_TS),
                ("use.ts", XL_USE_TS),
            ],
            &[("use.ts", shifted(XL_USE_TS).as_str())],
            Expect::Typed("cls.ts"),
        ),
    ];
    assert_cases(&cases);
}

// ---- A04: rebinding in the declaring function ----

fn with_use<'a>(base: &[(&'a str, &'a str)], src: &'a str) -> Vec<(&'a str, &'a str)> {
    let mut files = base.to_vec();
    files.push(("use.py", src));
    files
}

fn rebinding_use(body: &str) -> String {
    format!(
        "from cls import Cls\nfrom other import Other\n\n\ndef run(xs, obj: Cls, loose):\n    {body}\n"
    )
}

/// Stands for (A04, section 4): the Calls path revokes a parameter type after
/// a same-function rebinding, in the reindex and in the overlay alike. The
/// plain body is the positive control; a decoy `Other.m` makes the by-name
/// guess ambiguous, so a revoked receiver binds nothing.
#[test]
fn test_overlay_python_rebinding_in_declaring_function_matches_reindex() {
    let plain = rebinding_use("return obj.m(1)");
    let rebound = rebinding_use("obj = loose\n    return obj.m(1)");
    let looped = rebinding_use("for obj in loose:\n        obj.m(1)");
    let trio = [("cls.py", CLS_PY), ("other.py", OTHER_PY)];
    let calls = |label: &str, files: &[(&str, &str)], edits: &[(&str, &str)], expect| {
        case(label, RelType::Calls, "run", "m", files, edits, expect)
    };
    let shifted_plain = shifted(&plain);
    let cases = vec![
        calls(
            "A04 control: plain body shifted",
            &with_use(&trio, &plain),
            &[("use.py", shifted_plain.as_str())],
            Expect::Typed("cls.py"),
        ),
        calls(
            "A04 plain body rewritten to a rebinding",
            &with_use(&trio, &plain),
            &[("use.py", rebound.as_str())],
            Expect::Nothing,
        ),
        calls(
            "A04 rebinding rewritten to the plain body",
            &with_use(&trio, &rebound),
            &[("use.py", plain.as_str())],
            Expect::Typed("cls.py"),
        ),
        calls(
            "A04 for-target rebinding",
            &with_use(&trio, &plain),
            &[("use.py", looped.as_str())],
            Expect::Nothing,
        ),
        calls(
            "A04 decoy namesake added in other.py: the guess becomes ambiguous",
            &[("cls.py", CLS_PY), ("use.py", rebound.as_str())],
            &[("other.py", OTHER_PY)],
            Expect::Nothing,
        ),
        calls(
            "A04 decoy namesake removed from other.py: the revoked receiver guesses",
            &with_use(&trio, &rebound),
            &[("other.py", "")],
            Expect::Guess("cls.py"),
        ),
        calls(
            "A04 class renamed in cls.py",
            &with_use(&trio, &plain),
            &[("cls.py", CLS_PY_RENAMED_CLASS)],
            Expect::ParityOnly,
        ),
        calls(
            "A04 import removed",
            &with_use(&trio, &plain),
            &[(
                "use.py",
                plain.replace("from cls import Cls\n", "").as_str(),
            )],
            Expect::ParityOnly,
        ),
    ];
    assert_cases(&cases);
}

// ---- A07: Java block-scoped locals ----

const A_JAVA: (&str, &str) = ("A.java", "class A { void m() {} }\n");
const B_JAVA: (&str, &str) = ("B.java", "class B { void m() {} }\n");
const B_JAVA_RENAMED: (&str, &str) = ("B.java", "class B { void n() {} }\n");
const A_JAVA_WITH_B: (&str, &str) = (
    "A.java",
    "class A { void m() {} }\nclass B { void m() {} }\n",
);
const H_JAVA: &str = "class H {\n    void run() {\n        { A x = new A(); Runnable a = x::m; }\n        { B x = new B(); Runnable b = x::m; }\n    }\n}\n";
const H_JAVA_BOTH_A: &str = "class H {\n    void run() {\n        { A x = new A(); Runnable a = x::m; }\n        { A x = new A(); Runnable b = x::m; }\n    }\n}\n";
const HC_JAVA: &str = "class Hc {\n    void run() {\n        { A obj = new A(); }\n        { B obj = new B(); obj.m(); }\n    }\n}\n";
const HC_JAVA_SECOND_A: &str = "class Hc {\n    void run() {\n        { A obj = new A(); }\n        { A obj = new A(); obj.m(); }\n    }\n}\n";

/// Stands for (A07, section 4): the block-scoped locals of one name bind each
/// block's own type in the overlay as in the reindex: the `Calls` form edited
/// from `B obj` to `A obj` flips to `A.m`, the `References` form keeps one
/// edge per block type, and a renamed or clashing callee changes the row.
#[test]
fn test_overlay_java_block_scoped_same_name_locals_matches_reindex() {
    let calls = |label: &str, files: &[(&str, &str)], edits: &[(&str, &str)], expect| {
        case(label, RelType::Calls, "run", "m", files, edits, expect)
    };
    let refs = |label: &str, files: &[(&str, &str)], edits: &[(&str, &str)], expect| {
        case(label, RelType::References, "run", "m", files, edits, expect)
    };
    let hc = [A_JAVA, B_JAVA, ("Hc.java", HC_JAVA)];
    let h = [A_JAVA, B_JAVA, ("H.java", H_JAVA)];
    let shifted_hc = shifted(HC_JAVA);
    let shifted_h = shifted(H_JAVA);
    let cases = vec![
        calls(
            "A07 control: second block calls B, shifted",
            &hc,
            &[("Hc.java", shifted_hc.as_str())],
            Expect::Typed("B.java"),
        ),
        calls(
            "A07 second block edited from B to A",
            &hc,
            &[("Hc.java", HC_JAVA_SECOND_A)],
            Expect::Typed("A.java"),
        ),
        calls("A07 B.m renamed", &hc, &[B_JAVA_RENAMED], Expect::Nothing),
        calls(
            "A07 A.java gains a second class B",
            &hc,
            &[A_JAVA_WITH_B],
            Expect::ParityOnly,
        ),
        calls(
            "A07 caller and callee dirty together",
            &hc,
            &[("Hc.java", HC_JAVA_SECOND_A), B_JAVA_RENAMED],
            Expect::Typed("A.java"),
        ),
        refs(
            "A07 refs control: one edge per block type, shifted",
            &h,
            &[("H.java", shifted_h.as_str())],
            Expect::Refs(&["A.java", "B.java"]),
        ),
        refs(
            "A07 refs second block edited from B to A",
            &h,
            &[("H.java", H_JAVA_BOTH_A)],
            Expect::Refs(&["A.java"]),
        ),
    ];
    assert_cases(&cases);
}

// ---- A09: PHP untyped receiver ----

const A_PHP: (&str, &str) = ("A.php", "<?php\nclass A { function unique_cb() {} }\n");
const A_PHP_RENAMED_METHOD: (&str, &str) = ("A.php", "<?php\nclass A { function other_cb() {} }\n");
const A_PHP_RENAMED_CLASS: (&str, &str) =
    ("A.php", "<?php\nclass A2 { function unique_cb() {} }\n");
const A_PHP_SECOND_CLASS: (&str, &str) = (
    "A.php",
    "<?php\nclass A { function unique_cb() {} }\nclass A3 { function unique_cb() {} }\n",
);
const B_PHP: (&str, &str) = ("B.php", "<?php\nclass B { function other() {} }\n");
const HOLD: &str = "<?php\nfunction hold($o) { return $o->unique_cb(); }\n";
const HOLD_A: &str = "<?php\nfunction hold(A $o) { return $o->unique_cb(); }\n";
const HOLD_B: &str = "<?php\nfunction hold(B $o) { return $o->unique_cb(); }\n";
const HOLD_BRANCHES: &str = "<?php\nfunction holdJ($c) {\n    if ($c) { $o = new A(); } else { $o = new B(); }\n    return $o->unique_cb();\n}\n";
const HOLD_BRANCHES_INVERTED: &str = "<?php\nfunction holdJ($c) {\n    if (!$c) { $o = new B(); } else { $o = new A(); }\n    return $o->unique_cb();\n}\n";

/// Stands for (A09, section 4): the untyped PHP receiver binds as the tagged
/// guess in the overlay as in the reindex. A type hint `A` lifts it to the
/// typed `call`, a hint `B` removes it, a renamed callee or a second class
/// with the method removes it, and a branch conflict stays a guess in either
/// spelling of the condition.
#[test]
fn test_overlay_php_untyped_receiver_matches_reindex() {
    let calls = |label: &str,
                 caller: &'static str,
                 files: &[(&str, &str)],
                 edits: &[(&str, &str)],
                 expect| {
        case(
            label,
            RelType::Calls,
            caller,
            "unique_cb",
            files,
            edits,
            expect,
        )
    };
    let hold = [A_PHP, B_PHP, ("hold.php", HOLD)];
    let branches = [A_PHP, B_PHP, ("hold.php", HOLD_BRANCHES)];
    let shifted_hold = shifted(HOLD);
    let cases = vec![
        calls(
            "A09 untyped caller shifted",
            "hold",
            &hold,
            &[("hold.php", shifted_hold.as_str())],
            Expect::Guess("A.php"),
        ),
        calls(
            "A09 caller gains the type hint A",
            "hold",
            &hold,
            &[("hold.php", HOLD_A)],
            Expect::Typed("A.php"),
        ),
        calls(
            "A09 caller gains the type hint B",
            "hold",
            &hold,
            &[("hold.php", HOLD_B)],
            Expect::Nothing,
        ),
        calls(
            "A09 stale typed edge masked by the hint B",
            "hold",
            &[A_PHP, B_PHP, ("hold.php", HOLD_A)],
            &[("hold.php", HOLD_B)],
            Expect::Nothing,
        ),
        calls(
            "A09 callee method renamed",
            "hold",
            &hold,
            &[A_PHP_RENAMED_METHOD],
            Expect::Nothing,
        ),
        calls(
            "A09 owner class renamed",
            "hold",
            &hold,
            &[A_PHP_RENAMED_CLASS],
            Expect::Guess("A.php"),
        ),
        calls(
            "A09 second class declares the method: ambiguous",
            "hold",
            &hold,
            &[A_PHP_SECOND_CLASS],
            Expect::Nothing,
        ),
        calls(
            "A09 caller and callee dirty together",
            "hold",
            &hold,
            &[("hold.php", HOLD_A), A_PHP_RENAMED_METHOD],
            Expect::Nothing,
        ),
        calls(
            "A09 branches conflict, shifted",
            "holdJ",
            &branches,
            &[("hold.php", shifted(HOLD_BRANCHES).as_str())],
            Expect::Guess("A.php"),
        ),
        calls(
            "A09 branches conflict, condition inverted: the same verdict",
            "holdJ",
            &branches,
            &[("hold.php", HOLD_BRANCHES_INVERTED)],
            Expect::Guess("A.php"),
        ),
    ];
    assert_cases(&cases);
}
