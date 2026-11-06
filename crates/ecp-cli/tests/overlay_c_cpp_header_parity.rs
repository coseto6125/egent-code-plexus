//! C and C++ names declared in a header, out of a dirty file: the session
//! overlay binds what a full reindex of the edited tree binds, confidence
//! included (both run `ecp-core` `analyzer/c_family.rs` `c_family_field`,
//! over the index's candidates and over the overlay's).
//!
//! | case                                   | the reindex has                 |
//! |----------------------------------------|---------------------------------|
//! | C caller, struct / typedef in a `.h`   | `Accesses` to the header types  |
//! | C++ caller, prototype + definition     | `Calls` / `References` to the definition |
//! | C caller, `.h` prototype + `.cpp` definition | `Calls` to the definition |
//! | C caller, function in a C++ namespace  | none                            |
//! | C++ caller, two definitions + prototype | none                           |
//! | C++ caller, prototype + overload of another arity | none                 |
//! | C caller, prototype + C++ overload of another arity | `Calls` to the prototype |
//! | C caller, prototype + vendor definition | `Calls` to the prototype       |
//! | C++ caller, prototype + `static` C definition | `Calls` to the prototype |
//! | C++ caller, prototype + `static` definition in another file | `Calls` to the prototype |
//! | C++ caller, prototype + overload of the same arity, other types | none   |
//! | C caller, vendor prototype + header definition | `Calls` to the definition |
//! | C++ caller, unnamed `void *` prototype + definition | `Calls` to the definition |
//! | C++ caller, prototype + K&R C definition | `Calls` to the definition    |
//! | C++ caller, variadic `.h` prototype + C definition | `Calls` to the definition |
//! | C++ caller, `struct` + line break in a prototype type | `Calls` to the definition |
//!
//! Where the header or the definition is dirty too, the prototype (or the
//! definition) is an overlay symbol, so the overlay reads its flags from
//! the fresh parse instead of the base graph, and its parameter count and
//! types from the fresh parse's parameters.

use crate::parity;

use ecp_core::graph::RelType;
use parity::{calls_after_edit, edges_after_edit, parse, CallEdge, EdgeParity, ReasonedEdge};

struct Case {
    label: &'static str,
    files: &'static [(&'static str, &'static str)],
    edits: &'static [(&'static str, &'static str)],
    rel: RelType,
    /// The reason the rows keep; `Calls` compares without one, since an
    /// overlay `Calls` edge carries the uncommitted-edit marker.
    reason: Option<&'static str>,
    source_file: &'static str,
    /// `(caller, callee file, callee)` the reindex emits, sorted.
    expected: &'static [(&'static str, &'static str, &'static str)],
}

const HEADER_TYPES: &str = "struct St { int x; };\ntypedef struct { int a; } Al;\n";
const C_TYPE_USER: &str = "#include \"t.h\"\n\
    struct St st_ret(void) { struct St s; return s; }\n\
    Al al_ret(void) { Al a; return a; }\n";
const CPP_PROTOTYPE: &str = "void cb_bare(int &x);\n";
const CPP_DEFINITION: &str = "#include \"types.hpp\"\nvoid cb_bare(int &x) {}\n";
const CPP_USER_BARE: &str = "#include \"types.hpp\"\nvoid run() { int v = 0; }\n";
const CPP_USER: &str = "#include \"types.hpp\"\n\
    #include <algorithm>\n\
    #include <vector>\n\
    void use_bare(std::vector<int> a) { std::for_each(a.begin(), a.end(), cb_bare); }\n\
    void run() { int v = 0; cb_bare(v); }\n";

const CASES: &[Case] = &[
    Case {
        label: "C caller, header types, caller dirty",
        files: &[("t.h", HEADER_TYPES), ("m.c", "#include \"t.h\"\n")],
        edits: &[("m.c", C_TYPE_USER)],
        rel: RelType::Accesses,
        reason: Some("type_annotation"),
        source_file: "m.c",
        expected: &[("al_ret", "t.h", "Al"), ("st_ret", "t.h", "St")],
    },
    Case {
        label: "C caller, header types, header and caller dirty",
        files: &[("t.h", ""), ("m.c", "#include \"t.h\"\n")],
        edits: &[("t.h", HEADER_TYPES), ("m.c", C_TYPE_USER)],
        rel: RelType::Accesses,
        reason: Some("type_annotation"),
        source_file: "m.c",
        expected: &[("al_ret", "t.h", "Al"), ("st_ret", "t.h", "St")],
    },
    Case {
        label: "C++ prototype + definition, caller dirty (Calls)",
        files: &[
            ("types.hpp", CPP_PROTOTYPE),
            ("cb.cpp", CPP_DEFINITION),
            ("main.cpp", CPP_USER_BARE),
        ],
        edits: &[("main.cpp", CPP_USER)],
        rel: RelType::Calls,
        reason: None,
        source_file: "main.cpp",
        expected: &[("run", "cb.cpp", "cb_bare")],
    },
    Case {
        label: "C++ prototype + definition, caller dirty (References)",
        files: &[
            ("types.hpp", CPP_PROTOTYPE),
            ("cb.cpp", CPP_DEFINITION),
            ("main.cpp", CPP_USER_BARE),
        ],
        edits: &[("main.cpp", CPP_USER)],
        rel: RelType::References,
        reason: Some("function_reference"),
        source_file: "main.cpp",
        expected: &[("use_bare", "cb.cpp", "cb_bare")],
    },
    Case {
        label: "C++ prototype + definition, header and caller dirty",
        files: &[
            ("types.hpp", ""),
            ("cb.cpp", CPP_DEFINITION),
            ("main.cpp", CPP_USER_BARE),
        ],
        edits: &[("types.hpp", CPP_PROTOTYPE), ("main.cpp", CPP_USER)],
        rel: RelType::Calls,
        reason: None,
        source_file: "main.cpp",
        expected: &[("run", "cb.cpp", "cb_bare")],
    },
    Case {
        label: "C++ prototype + definition, definition and caller dirty",
        files: &[
            ("types.hpp", CPP_PROTOTYPE),
            ("cb.cpp", "#include \"types.hpp\"\n"),
            ("main.cpp", CPP_USER_BARE),
        ],
        edits: &[("cb.cpp", CPP_DEFINITION), ("main.cpp", CPP_USER)],
        rel: RelType::Calls,
        reason: None,
        source_file: "main.cpp",
        expected: &[("run", "cb.cpp", "cb_bare")],
    },
    Case {
        label: "C caller, .h prototype + .cpp definition",
        files: &[
            ("api.h", "void lib_init(void);\n"),
            ("lib.cpp", "#include \"api.h\"\nvoid lib_init(void) {}\n"),
            ("m.c", "#include \"api.h\"\nvoid run(void) { }\n"),
        ],
        edits: &[(
            "m.c",
            "#include \"api.h\"\nvoid run(void) { lib_init(); }\n",
        )],
        rel: RelType::Calls,
        reason: None,
        source_file: "m.c",
        expected: &[("run", "lib.cpp", "lib_init")],
    },
    Case {
        label: "C caller, function in a C++ namespace",
        files: &[
            ("t.h", "namespace ns { void helper(void); }\n"),
            ("m.c", "#include \"t.h\"\nvoid run(void) { }\n"),
        ],
        edits: &[("m.c", "#include \"t.h\"\nvoid run(void) { helper(); }\n")],
        rel: RelType::Calls,
        reason: None,
        source_file: "m.c",
        expected: &[],
    },
    Case {
        label: "C++ caller, two definitions + prototype",
        files: &[
            ("types.hpp", "void dup(int &x);\n"),
            ("a.cpp", "void dup(int &x) {}\n"),
            ("b.cpp", "void dup(int &x) {}\n"),
            ("main.cpp", "#include \"types.hpp\"\nvoid run() { }\n"),
        ],
        edits: &[(
            "main.cpp",
            "#include \"types.hpp\"\nvoid run() { int v = 0; dup(v); }\n",
        )],
        rel: RelType::Calls,
        reason: None,
        source_file: "main.cpp",
        expected: &[],
    },
];

/// `(caller, callee file, callee)` of each edge, sorted.
fn rows(edges: &[CallEdge]) -> Vec<(String, String, String)> {
    let mut rows: Vec<_> = edges
        .iter()
        .map(|e| (e.caller.clone(), e.callee_file.clone(), e.callee.clone()))
        .collect();
    rows.sort_unstable();
    rows
}

/// The mismatch of `case`, if any: a reindex that differs from the
/// expected rows (the fixture would prove nothing), or an overlay that
/// differs from the reindex in any field, confidence included.
fn mismatch(case: &Case) -> Option<String> {
    let selects = |file: &str, _: &str| file == case.source_file;
    let (got, detail) = match case.reason {
        None => {
            let parity = calls_after_edit(&parse, case.files, case.edits, &selects);
            let detail = (parity.overlay != parity.reindex).then(|| {
                format!(
                    "overlay {:#?}\nreindex {:#?}",
                    parity.overlay, parity.reindex
                )
            });
            (rows(&parity.reindex), detail)
        }
        Some(reason) => {
            let EdgeParity { reindex, overlay } =
                edges_after_edit(&parse, case.files, case.edits, case.rel, &selects);
            let only = |edges: Vec<ReasonedEdge>| -> Vec<ReasonedEdge> {
                edges.into_iter().filter(|e| e.reason == reason).collect()
            };
            let (reindex, overlay) = (only(reindex), only(overlay));
            let edges: Vec<CallEdge> = reindex.iter().map(|e| e.edge.clone()).collect();
            let detail =
                (overlay != reindex).then(|| format!("overlay {overlay:#?}\nreindex {reindex:#?}"));
            (rows(&edges), detail)
        }
    };
    let expected: Vec<(String, String, String)> = case
        .expected
        .iter()
        .map(|&(a, b, c)| (a.to_string(), b.to_string(), c.to_string()))
        .collect();
    if got != expected {
        return Some(format!(
            "{}: the reindex emits {got:?}, the fixture expects {expected:?}",
            case.label
        ));
    }
    detail.map(|detail| format!("{}: {detail}", case.label))
}

#[test]
fn test_overlay_c_family_field_header_declarations_match_reindex() {
    let failures: Vec<String> = CASES.iter().filter_map(mismatch).collect();
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// `case` alone: the reindex emits its rows, and the overlay equals the
/// reindex.
fn assert_case(case: &Case) {
    if let Some(failure) = mismatch(case) {
        panic!("{failure}");
    }
}

const OVERLOAD_HEADER: &str = "void lib_init(void);\n";
const OVERLOAD_C_DEFINITION: &str = "#include \"api.h\"\nvoid lib_init(void) {}\n";
const OVERLOAD_OTHER_ARITY: &str = "void lib_init(int mode) {}\n";
const OVERLOAD_USER_BARE: &str = "#include \"api.h\"\nvoid run(void) { }\n";
const OVERLOAD_USER: &str = "#include \"api.h\"\nvoid run(void) { lib_init(); }\n";

/// Stands for: an overload of another parameter count settles no
/// prototype, so the C++ caller gets no edge to it, whether the overload
/// is clean or dirty.
#[test]
fn test_overlay_c_family_field_cpp_caller_overload_of_other_arity_matches_reindex() {
    assert_case(&Case {
        label: "C++ caller, prototype + overload of another arity, caller dirty",
        files: &[
            ("api.h", OVERLOAD_HEADER),
            ("lib.c", OVERLOAD_C_DEFINITION),
            ("other.cpp", OVERLOAD_OTHER_ARITY),
            ("main.cpp", OVERLOAD_USER_BARE),
        ],
        edits: &[("main.cpp", OVERLOAD_USER)],
        rel: RelType::Calls,
        reason: None,
        source_file: "main.cpp",
        expected: &[],
    });
    assert_case(&Case {
        label: "C++ caller, prototype + overload of another arity, overload and caller dirty",
        files: &[
            ("api.h", OVERLOAD_HEADER),
            ("lib.c", OVERLOAD_C_DEFINITION),
            ("other.cpp", ""),
            ("main.cpp", OVERLOAD_USER_BARE),
        ],
        edits: &[
            ("other.cpp", OVERLOAD_OTHER_ARITY),
            ("main.cpp", OVERLOAD_USER),
        ],
        rel: RelType::Calls,
        reason: None,
        source_file: "main.cpp",
        expected: &[],
    });
}

/// Stands for: a C caller binds the prototype its header declares, never a
/// C++ overload of another parameter count.
#[test]
fn test_overlay_c_family_field_c_caller_cpp_overload_of_other_arity_matches_reindex() {
    assert_case(&Case {
        label: "C caller, prototype + C++ overload of another arity",
        files: &[
            ("api.h", "void ext_init(void);\n"),
            ("other.cpp", "void ext_init(int mode) {}\n"),
            ("m.c", "#include \"api.h\"\nvoid crun(void) { }\n"),
        ],
        edits: &[(
            "m.c",
            "#include \"api.h\"\nvoid crun(void) { ext_init(); }\n",
        )],
        rel: RelType::Calls,
        reason: None,
        source_file: "m.c",
        expected: &[("crun", "api.h", "ext_init")],
    });
}

/// Stands for: a vendor definition settles no prototype for a non-vendor
/// caller, so the caller keeps its edge to the prototype.
#[test]
fn test_overlay_c_family_field_c_caller_vendor_definition_matches_reindex() {
    assert_case(&Case {
        label: "C caller, prototype + vendor definition",
        files: &[
            ("api.h", "void ext_fn(void);\n"),
            ("src/main.c", "#include \"../api.h\"\nvoid run(void) { }\n"),
            ("vendor/unused/impl.cpp", "void ext_fn(void) {}\n"),
        ],
        edits: &[(
            "src/main.c",
            "#include \"../api.h\"\nvoid run(void) { ext_fn(); }\n",
        )],
        rel: RelType::Calls,
        reason: None,
        source_file: "src/main.c",
        expected: &[("run", "api.h", "ext_fn")],
    });
}

const STATIC_HEADER: &str = "void sfn(void);\n";
const STATIC_C_DEFINITION: &str = "static void sfn(void) {}\n";
const STATIC_USER_BARE: &str = "#include \"api.h\"\nvoid run() { }\n";
const STATIC_USER: &str = "#include \"api.h\"\nvoid run() { sfn(); }\n";

/// Stands for: a `static` C function never crosses to a C++ caller in
/// another file, whether the definition is clean or dirty (its flags then
/// come from the fresh parse).
#[test]
fn test_overlay_c_family_field_cpp_caller_static_c_definition_matches_reindex() {
    assert_case(&Case {
        label: "C++ caller, prototype + static C definition, caller dirty",
        files: &[
            ("api.h", STATIC_HEADER),
            ("a.c", STATIC_C_DEFINITION),
            ("main.cpp", STATIC_USER_BARE),
        ],
        edits: &[("main.cpp", STATIC_USER)],
        rel: RelType::Calls,
        reason: None,
        source_file: "main.cpp",
        expected: &[("run", "api.h", "sfn")],
    });
    assert_case(&Case {
        label: "C++ caller, prototype + static C definition, definition and caller dirty",
        files: &[
            ("api.h", STATIC_HEADER),
            ("a.c", ""),
            ("main.cpp", STATIC_USER_BARE),
        ],
        edits: &[("a.c", STATIC_C_DEFINITION), ("main.cpp", STATIC_USER)],
        rel: RelType::Calls,
        reason: None,
        source_file: "main.cpp",
        expected: &[("run", "api.h", "sfn")],
    });
}

/// Stands for: a `static` definition in another file is no candidate, so
/// the prototype is the one declaration the C++ caller reaches and binds
/// (it was "no edge").
#[test]
fn test_overlay_c_family_field_cpp_caller_static_definition_in_other_file_matches_reindex() {
    assert_case(&Case {
        label: "C++ caller, prototype + static definition in another file",
        files: &[
            ("types.hpp", "void sf();\n"),
            ("a.cpp", "static void sf() {}\n"),
            ("main.cpp", "#include \"types.hpp\"\nvoid run() { }\n"),
        ],
        edits: &[("main.cpp", "#include \"types.hpp\"\nvoid run() { sf(); }\n")],
        rel: RelType::Calls,
        reason: None,
        source_file: "main.cpp",
        expected: &[("run", "types.hpp", "sf")],
    });
}

const TYPES_HEADER: &str = "void f(int);\n";
const TYPES_OTHER_OVERLOAD: &str = "void f(double) {}\n";
const TYPES_USER_BARE: &str = "#include \"api.hpp\"\nvoid run() { }\n";
const TYPES_USER: &str = "#include \"api.hpp\"\nvoid run() { f(1); }\n";

/// Stands for: an overload of the same parameter count and other types
/// settles no prototype, whether the overload is clean or dirty (its types
/// then come from the fresh parse).
#[test]
fn test_overlay_c_family_field_cpp_caller_overload_of_same_arity_other_types_matches_reindex() {
    assert_case(&Case {
        label: "C++ caller, prototype + same-arity overload, caller dirty",
        files: &[
            ("api.hpp", TYPES_HEADER),
            ("other.cpp", TYPES_OTHER_OVERLOAD),
            ("main.cpp", TYPES_USER_BARE),
        ],
        edits: &[("main.cpp", TYPES_USER)],
        rel: RelType::Calls,
        reason: None,
        source_file: "main.cpp",
        expected: &[],
    });
    assert_case(&Case {
        label: "C++ caller, prototype + same-arity overload, overload and caller dirty",
        files: &[
            ("api.hpp", TYPES_HEADER),
            ("other.cpp", ""),
            ("main.cpp", TYPES_USER_BARE),
        ],
        edits: &[
            ("other.cpp", TYPES_OTHER_OVERLOAD),
            ("main.cpp", TYPES_USER),
        ],
        rel: RelType::Calls,
        reason: None,
        source_file: "main.cpp",
        expected: &[],
    });
}

/// Stands for: a vendor prototype the caller cannot reach never pushes the
/// header definition out.
#[test]
fn test_overlay_c_family_field_c_caller_vendor_prototype_matches_reindex() {
    assert_case(&Case {
        label: "C caller, vendor prototype + header definition",
        files: &[
            ("api.h", "void f(void) {}\n"),
            ("vendor/unused/api.hpp", "void f(int);\n"),
            ("main.c", "#include \"api.h\"\nvoid run(void) { }\n"),
        ],
        edits: &[("main.c", "#include \"api.h\"\nvoid run(void) { f(); }\n")],
        rel: RelType::Calls,
        reason: None,
        source_file: "main.c",
        expected: &[("run", "api.h", "f")],
    });
}

const VOID_PTR_HEADER: &str = "void release(void *);\n";
const VOID_PTR_DEFINITION: &str = "#include \"api.h\"\nvoid release(void *p) {}\n";
const VOID_PTR_USER_BARE: &str = "#include \"api.h\"\nvoid run() { }\n";
const VOID_PTR_USER: &str = "#include \"api.h\"\nvoid run() { release(0); }\n";

/// Stands for: an unnamed `void *` prototype counts one parameter in the
/// base graph and in a dirty header alike, so its definition settles it.
#[test]
fn test_overlay_c_family_field_cpp_caller_unnamed_void_pointer_matches_reindex() {
    assert_case(&Case {
        label: "C++ caller, unnamed void * prototype + definition, caller dirty",
        files: &[
            ("api.h", VOID_PTR_HEADER),
            ("lib.cpp", VOID_PTR_DEFINITION),
            ("main.cpp", VOID_PTR_USER_BARE),
        ],
        edits: &[("main.cpp", VOID_PTR_USER)],
        rel: RelType::Calls,
        reason: None,
        source_file: "main.cpp",
        expected: &[("run", "lib.cpp", "release")],
    });
    assert_case(&Case {
        label: "C++ caller, unnamed void * prototype + definition, header and caller dirty",
        files: &[
            ("api.h", ""),
            ("lib.cpp", VOID_PTR_DEFINITION),
            ("main.cpp", VOID_PTR_USER_BARE),
        ],
        edits: &[("api.h", VOID_PTR_HEADER), ("main.cpp", VOID_PTR_USER)],
        rel: RelType::Calls,
        reason: None,
        source_file: "main.cpp",
        expected: &[("run", "lib.cpp", "release")],
    });
}

const KR_HEADER: &str = "int kr(int, int);\n";
const KR_DEFINITION: &str = "int kr(a, b) int a; int b; { return a; }\n";
const KR_USER_BARE: &str = "#include \"api.h\"\nvoid run() { }\n";
const KR_USER: &str = "#include \"api.h\"\nvoid run() { kr(1, 2); }\n";

/// Stands for: a K&R C definition settles its prototype, whether it is
/// clean or dirty.
#[test]
fn test_overlay_c_family_field_cpp_caller_kr_definition_matches_reindex() {
    assert_case(&Case {
        label: "C++ caller, prototype + K&R C definition, caller dirty",
        files: &[
            ("api.h", KR_HEADER),
            ("lib.c", KR_DEFINITION),
            ("main.cpp", KR_USER_BARE),
        ],
        edits: &[("main.cpp", KR_USER)],
        rel: RelType::Calls,
        reason: None,
        source_file: "main.cpp",
        expected: &[("run", "lib.c", "kr")],
    });
    assert_case(&Case {
        label: "C++ caller, prototype + K&R C definition, definition and caller dirty",
        files: &[
            ("api.h", KR_HEADER),
            ("lib.c", ""),
            ("main.cpp", KR_USER_BARE),
        ],
        edits: &[("lib.c", KR_DEFINITION), ("main.cpp", KR_USER)],
        rel: RelType::Calls,
        reason: None,
        source_file: "main.cpp",
        expected: &[("run", "lib.c", "kr")],
    });
}

const VARIADIC_HEADER: &str = "int lg(const char *fmt, ...);\n";
const VARIADIC_DEFINITION: &str = "int lg(const char *fmt, ...) { return 0; }\n";
const VARIADIC_USER_BARE: &str = "#include \"api.h\"\nvoid run() { }\n";
const VARIADIC_USER: &str = "#include \"api.h\"\nvoid run() { lg(\"x\"); }\n";

/// Stands for: a C-style `...` counts one parameter in a C++-parsed header,
/// in the base graph and in a dirty header alike, so its C definition
/// settles it.
#[test]
fn test_overlay_c_family_field_cpp_caller_variadic_header_matches_reindex() {
    assert_case(&Case {
        label: "C++ caller, variadic prototype + C definition, caller dirty",
        files: &[
            ("api.h", VARIADIC_HEADER),
            ("lib.c", VARIADIC_DEFINITION),
            ("main.cpp", VARIADIC_USER_BARE),
        ],
        edits: &[("main.cpp", VARIADIC_USER)],
        rel: RelType::Calls,
        reason: None,
        source_file: "main.cpp",
        expected: &[("run", "lib.c", "lg")],
    });
    assert_case(&Case {
        label: "C++ caller, variadic prototype + C definition, header and caller dirty",
        files: &[
            ("api.h", ""),
            ("lib.c", VARIADIC_DEFINITION),
            ("main.cpp", VARIADIC_USER_BARE),
        ],
        edits: &[("api.h", VARIADIC_HEADER), ("main.cpp", VARIADIC_USER)],
        rel: RelType::Calls,
        reason: None,
        source_file: "main.cpp",
        expected: &[("run", "lib.c", "lg")],
    });
}

const BROKEN_STRUCT_HEADER: &str = "struct Foo {};\nvoid f(struct\nFoo);\n";
const BROKEN_STRUCT_DEFINITION: &str = "#include \"api.hpp\"\nvoid f(Foo x) {}\n";
const BROKEN_STRUCT_USER_BARE: &str = "#include \"api.hpp\"\nvoid run() { }\n";
const BROKEN_STRUCT_USER: &str = "#include \"api.hpp\"\nvoid run() { f(Foo{}); }\n";

/// Stands for: `struct\nFoo` and `Foo` are one parameter type on both
/// sides, so the definition settles the prototype whether the header is
/// clean or dirty.
#[test]
fn test_overlay_c_family_field_cpp_caller_struct_line_break_matches_reindex() {
    assert_case(&Case {
        label: "C++ caller, struct + line break prototype + definition, caller dirty",
        files: &[
            ("api.hpp", BROKEN_STRUCT_HEADER),
            ("impl.cpp", BROKEN_STRUCT_DEFINITION),
            ("main.cpp", BROKEN_STRUCT_USER_BARE),
        ],
        edits: &[("main.cpp", BROKEN_STRUCT_USER)],
        rel: RelType::Calls,
        reason: None,
        source_file: "main.cpp",
        expected: &[("run", "impl.cpp", "f")],
    });
    assert_case(&Case {
        label: "C++ caller, struct + line break prototype + definition, header and caller dirty",
        files: &[
            ("api.hpp", "struct Foo {};\n"),
            ("impl.cpp", BROKEN_STRUCT_DEFINITION),
            ("main.cpp", BROKEN_STRUCT_USER_BARE),
        ],
        edits: &[
            ("api.hpp", BROKEN_STRUCT_HEADER),
            ("main.cpp", BROKEN_STRUCT_USER),
        ],
        rel: RelType::Calls,
        reason: None,
        source_file: "main.cpp",
        expected: &[("run", "impl.cpp", "f")],
    });
}

/// Every case runs, and at least one of them has rows: a list of
/// forbidden-edge fixtures alone would pass on an overlay and a reindex that
/// both emit nothing, so each list carries its positive control.
fn assert_cases_with_control(cases: &[Case]) {
    assert!(
        cases.iter().any(|case| !case.expected.is_empty()),
        "no positive control in the list"
    );
    let failures: Vec<String> = cases.iter().filter_map(mismatch).collect();
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

// ── A05: an unrelated class method beside the free function ─────────────

const A05_HEADER: &str = "void lib_init(void);\n";
const A05_LIB: &str = "#include \"api.h\"\nvoid lib_init(void) {}\n";
const A05_MAIN_BARE: &str = "#include \"api.h\"\nvoid run(void) { }\n";
const A05_MAIN: &str = "#include \"api.h\"\nvoid run(void) { lib_init(); }\n";
const A05_WIDGET: &str = "struct W { void lib_init(); };\nvoid W::lib_init() {}\n";

/// Stands for: a method of an unrelated class named like the free function
/// neither takes the edge nor hides the C definition, whether `widget.cpp`
/// is in the base, is added or is emptied again.
#[test]
fn test_overlay_c_family_field_cpp_caller_unrelated_class_method_matches_reindex() {
    assert_cases_with_control(&[
        Case {
            label: "widget.cpp in the base, caller dirty",
            files: &[
                ("api.h", A05_HEADER),
                ("lib.c", A05_LIB),
                ("main.cpp", A05_MAIN_BARE),
                ("widget.cpp", A05_WIDGET),
            ],
            edits: &[("main.cpp", A05_MAIN)],
            rel: RelType::Calls,
            reason: None,
            source_file: "main.cpp",
            expected: &[("run", "lib.c", "lib_init")],
        },
        Case {
            label: "widget.cpp added, caller dirty",
            files: &[
                ("api.h", A05_HEADER),
                ("lib.c", A05_LIB),
                ("main.cpp", A05_MAIN_BARE),
                ("widget.cpp", ""),
            ],
            edits: &[("widget.cpp", A05_WIDGET), ("main.cpp", A05_MAIN)],
            rel: RelType::Calls,
            reason: None,
            source_file: "main.cpp",
            expected: &[("run", "lib.c", "lib_init")],
        },
        Case {
            label: "widget.cpp emptied, caller dirty",
            files: &[
                ("api.h", A05_HEADER),
                ("lib.c", A05_LIB),
                ("main.cpp", A05_MAIN_BARE),
                ("widget.cpp", A05_WIDGET),
            ],
            edits: &[("widget.cpp", ""), ("main.cpp", A05_MAIN)],
            rel: RelType::Calls,
            reason: None,
            source_file: "main.cpp",
            expected: &[("run", "lib.c", "lib_init")],
        },
        Case {
            label: "control: a member calls its own method",
            files: &[
                ("api.h", A05_HEADER),
                ("lib.c", A05_LIB),
                (
                    "widget.cpp",
                    "struct W { void lib_init(); void go() { } };\nvoid W::lib_init() {}\n",
                ),
            ],
            edits: &[(
                "widget.cpp",
                "struct W { void lib_init(); void go() { lib_init(); } };\nvoid W::lib_init() {}\n",
            )],
            rel: RelType::Calls,
            reason: None,
            source_file: "widget.cpp",
            expected: &[("go", "widget.cpp", "lib_init")],
        },
    ]);
}

// ── A06: a `static` function of another file ────────────────────────────

const A06_MAIN_BARE: &str = "void run(void) { }\n";
const A06_MAIN: &str = "void run(void) { hid(); }\n";

/// Stands for: a `static` C function binds no caller of another file, in the
/// base graph and when the `static` keyword is toggled in a dirty file; the
/// external twin binds.
#[test]
fn test_overlay_c_family_field_c_caller_static_function_matches_reindex() {
    assert_cases_with_control(&[
        Case {
            label: "C caller, static function of another file, caller dirty",
            files: &[
                ("a.c", "static void hid(void) {}\n"),
                ("m.c", A06_MAIN_BARE),
            ],
            edits: &[("m.c", A06_MAIN)],
            rel: RelType::Calls,
            reason: None,
            source_file: "m.c",
            expected: &[],
        },
        Case {
            label: "control: C caller, external function of another file",
            files: &[("a.c", "void hid(void) {}\n"), ("m.c", A06_MAIN_BARE)],
            edits: &[("m.c", A06_MAIN)],
            rel: RelType::Calls,
            reason: None,
            source_file: "m.c",
            expected: &[("run", "a.c", "hid")],
        },
        Case {
            label: "static keyword added to the callee, caller dirty",
            files: &[("a.c", "void hid(void) {}\n"), ("m.c", A06_MAIN)],
            edits: &[("a.c", "static void hid(void) {}\n"), ("m.c", A06_MAIN)],
            rel: RelType::Calls,
            reason: None,
            source_file: "m.c",
            expected: &[],
        },
        Case {
            label: "static keyword removed from the callee, caller dirty",
            files: &[("a.c", "static void hid(void) {}\n"), ("m.c", A06_MAIN)],
            edits: &[("a.c", "void hid(void) {}\n"), ("m.c", A06_MAIN)],
            rel: RelType::Calls,
            reason: None,
            source_file: "m.c",
            expected: &[("run", "a.c", "hid")],
        },
        Case {
            label: "external namesake added beside the static one, caller dirty",
            files: &[
                ("a.c", "static void hid(void) {}\n"),
                ("b.c", ""),
                ("m.c", A06_MAIN_BARE),
            ],
            edits: &[("b.c", "void hid(void) {}\n"), ("m.c", A06_MAIN)],
            rel: RelType::Calls,
            reason: None,
            source_file: "m.c",
            expected: &[("run", "b.c", "hid")],
        },
    ]);
}

/// Stands for: a function in an anonymous namespace has internal linkage, so
/// a C++ caller of another file binds nothing, in the base graph and in the
/// overlay; the plain twin binds.
#[test]
fn test_overlay_c_family_field_cpp_caller_anonymous_namespace_function_matches_reindex() {
    assert_cases_with_control(&[
        Case {
            label: "C++ caller, anonymous-namespace function of another file",
            files: &[
                ("a.cpp", "namespace { void anon_fn() {} }\n"),
                ("main.cpp", "void run() { }\n"),
            ],
            edits: &[("main.cpp", "void run() { anon_fn(); }\n")],
            rel: RelType::Calls,
            reason: None,
            source_file: "main.cpp",
            expected: &[],
        },
        Case {
            label: "control: C++ caller, plain function of another file",
            files: &[
                ("a.cpp", "void anon_fn() {}\n"),
                ("main.cpp", "void run() { }\n"),
            ],
            edits: &[("main.cpp", "void run() { anon_fn(); }\n")],
            rel: RelType::Calls,
            reason: None,
            source_file: "main.cpp",
            expected: &[("run", "a.cpp", "anon_fn")],
        },
    ]);
}

// ── A12: K&R undeclared parameter, `f()` and `f(void)` ──────────────────

const KR_C_USER_BARE: &str = "#include \"api.h\"\nint run(void) { return 0; }\n";
const KR_C_USER: &str = "#include \"api.h\"\nint run(void) { return kr(1, 2); }\n";
const KR_LEGACY_DEFINITION: &str = "int kr(a, b) int a; int b; { return a + b; }\n";
const KR_LEGACY_USER_BARE: &str = "#include \"api.h\"\nint prun() { return 0; }\n";
const KR_LEGACY_USER: &str = "#include \"api.h\"\nint prun() { return kr(1, 2); }\n";

/// Stands for: with implicit `int` accepted, a K&R definition with an
/// undeclared parameter settles its prototype, clean or dirty.
#[test]
fn test_overlay_c_family_field_c_caller_kr_undeclared_parameter_matches_reindex() {
    const DEFINITION: &str = "int kr(a, b) int a; { return a; }\n";
    assert_cases_with_control(&[
        Case {
            label: "C caller, undeclared K&R parameter, caller dirty",
            files: &[
                ("api.h", "int kr(int, int);\n"),
                ("lib.c", DEFINITION),
                ("main.c", KR_C_USER_BARE),
            ],
            edits: &[("main.c", KR_C_USER)],
            rel: RelType::Calls,
            reason: None,
            source_file: "main.c",
            expected: &[("run", "lib.c", "kr")],
        },
        Case {
            label: "C caller, undeclared K&R parameter, definition and caller dirty",
            files: &[
                ("api.h", "int kr(int, int);\n"),
                ("lib.c", ""),
                ("main.c", KR_C_USER_BARE),
            ],
            edits: &[("lib.c", DEFINITION), ("main.c", KR_C_USER)],
            rel: RelType::Calls,
            reason: None,
            source_file: "main.c",
            expected: &[("run", "lib.c", "kr")],
        },
    ]);
}

/// Stands for: `int kr();` is the unspecified form, so the K&R definition is
/// its definition for the C++ caller, with a second legacy prototype in
/// another file or without, and when the definition is an ANSI one that a
/// dirty file rewrites as K&R.
#[test]
fn test_overlay_c_family_field_cpp_caller_legacy_header_kr_matches_reindex() {
    assert_cases_with_control(&[
        Case {
            label: "C++ caller, legacy header, K&R definition, caller dirty",
            files: &[
                ("api.h", "int kr();\n"),
                ("lib.c", KR_LEGACY_DEFINITION),
                ("main.cpp", KR_LEGACY_USER_BARE),
            ],
            edits: &[("main.cpp", KR_LEGACY_USER)],
            rel: RelType::Calls,
            reason: None,
            source_file: "main.cpp",
            expected: &[("prun", "lib.c", "kr")],
        },
        Case {
            label: "C++ caller, two legacy prototypes, caller dirty",
            files: &[
                ("api.h", "int kr();\n"),
                ("lib.c", KR_LEGACY_DEFINITION),
                ("other.c", "int kr();\n"),
                ("main.cpp", KR_LEGACY_USER_BARE),
            ],
            edits: &[("main.cpp", KR_LEGACY_USER)],
            rel: RelType::Calls,
            reason: None,
            source_file: "main.cpp",
            expected: &[("prun", "lib.c", "kr")],
        },
        Case {
            label: "C++ caller, second legacy prototype added by a dirty file",
            files: &[
                ("api.h", "int kr();\n"),
                ("lib.c", KR_LEGACY_DEFINITION),
                ("other.c", ""),
                ("main.cpp", KR_LEGACY_USER_BARE),
            ],
            edits: &[("other.c", "int kr();\n"), ("main.cpp", KR_LEGACY_USER)],
            rel: RelType::Calls,
            reason: None,
            source_file: "main.cpp",
            expected: &[("prun", "lib.c", "kr")],
        },
        Case {
            label: "C++ caller, ANSI definition rewritten as K&R, caller dirty",
            files: &[
                ("api.h", "int kr();\n"),
                ("lib.c", "int kr(int a, int b) { return a + b; }\n"),
                ("main.cpp", KR_LEGACY_USER_BARE),
            ],
            edits: &[
                ("lib.c", KR_LEGACY_DEFINITION),
                ("main.cpp", KR_LEGACY_USER),
            ],
            rel: RelType::Calls,
            reason: None,
            source_file: "main.cpp",
            expected: &[("prun", "lib.c", "kr")],
        },
    ]);
}

/// Stands for: `f()` matches a definition with parameters and `f(void)`
/// matches none, in the base graph and in a dirty header or definition.
#[test]
fn test_overlay_c_family_field_c_caller_unspecified_and_void_forms_match_reindex() {
    const HEADER: &str = "int fv(void);\nint fu();\n";
    const LIB: &str = "int fv(int x) { return x; }\nint fu(int x) { return x; }\n";
    const BARE: &str =
        "#include \"api.h\"\nint run(void) { return 0; }\nint runv(void) { return 0; }\n";
    const USER: &str =
        "#include \"api.h\"\nint run(void) { return fu(1); }\nint runv(void) { return fv(); }\n";
    assert_cases_with_control(&[
        Case {
            label: "C caller, f() and f(void), caller dirty",
            files: &[("api.h", HEADER), ("lib.c", LIB), ("main.c", BARE)],
            edits: &[("main.c", USER)],
            rel: RelType::Calls,
            reason: None,
            source_file: "main.c",
            expected: &[("run", "lib.c", "fu"), ("runv", "api.h", "fv")],
        },
        Case {
            label: "C caller, f() and f(void), header and caller dirty",
            files: &[("api.h", ""), ("lib.c", LIB), ("main.c", BARE)],
            edits: &[("api.h", HEADER), ("main.c", USER)],
            rel: RelType::Calls,
            reason: None,
            source_file: "main.c",
            expected: &[("run", "lib.c", "fu"), ("runv", "api.h", "fv")],
        },
    ]);
}

// ── A15e: a C file's own typedef is no import ───────────────────────────

/// Stands for: a dirty C file that declares a typedef emits the `Imports`
/// rows the reindex does, and none of them names the typedef; a typedef a
/// header declares still reaches the includer as a type annotation.
#[test]
fn test_overlay_c_family_field_c_self_declared_typedef_emits_no_imports_matches_reindex() {
    let typedef_main = "typedef void (*cb_t)(int *);\nvoid apply(cb_t f) {}\n";
    let EdgeParity { reindex, overlay } = edges_after_edit(
        &parse,
        &[("main.c", "void apply(void) {}\n")],
        &[("main.c", typedef_main)],
        RelType::Imports,
        &|file, _| file == "main.c",
    );
    assert!(
        reindex.iter().all(|e| e.edge.callee != "cb_t"),
        "the reindex imports a typedef its own file declares: {reindex:#?}"
    );
    assert_eq!(overlay, reindex);
    assert_case(&Case {
        label: "control: C caller, header typedef, caller dirty",
        files: &[("t.h", HEADER_TYPES), ("user.c", "#include \"t.h\"\n")],
        edits: &[(
            "user.c",
            "#include \"t.h\"\nAl al_ret(void) { Al a; return a; }\n",
        )],
        rel: RelType::Accesses,
        reason: Some("type_annotation"),
        source_file: "user.c",
        expected: &[("al_ret", "t.h", "Al")],
    });
}

// ── A15f: a qualified function value ────────────────────────────────────

const QUAL_HEADER: &str = "#pragma once\nnamespace ns {\nvoid cb_qual(int &x);\nstruct Inst { void cb_ptr(int &x); static void cb_ns(int &x); };\n}\nvoid cb_bare(int &x);\n";
const QUAL_DEFINITIONS: &str = "#include \"types.hpp\"\nvoid cb_bare(int &x) {}\nvoid ns::cb_qual(int &x) {}\nvoid ns::Inst::cb_ptr(int &x) {}\nvoid ns::Inst::cb_ns(int &x) {}\n";
const QUAL_MAIN_BARE: &str = "#include \"types.hpp\"\n#include <algorithm>\n#include <vector>\nvoid use_bare(std::vector<int> a) { }\n";
const QUAL_MAIN: &str = "#include \"types.hpp\"\n#include <algorithm>\n#include <vector>\n\
    void use_bare(std::vector<int> a) { std::for_each(a.begin(), a.end(), cb_bare); }\n\
    void use_qual(std::vector<int> a) { std::for_each(a.begin(), a.end(), &ns::cb_qual); }\n\
    void use_qual2(std::vector<int> a) { std::for_each(a.begin(), a.end(), ns::cb_qual); }\n\
    void use_ns(std::vector<int> a) { std::for_each(a.begin(), a.end(), &ns::Inst::cb_ns); }\n\
    void call_qual(std::vector<int> a) { int v = 0; ns::cb_qual(v); }\n";

/// Stands for: a qualified function value binds the definition, never the
/// header declaration, in the overlay as in the reindex, and a qualified call
/// lands on the same declaration; the definition file clean or dirty.
#[test]
fn test_overlay_c_family_field_cpp_qualified_function_value_matches_reindex() {
    let values: &[(&str, &str, &str)] = &[
        ("use_bare", "cb.cpp", "cb_bare"),
        ("use_ns", "cb.cpp", "cb_ns"),
        ("use_qual", "cb.cpp", "cb_qual"),
        ("use_qual2", "cb.cpp", "cb_qual"),
    ];
    let base = &[
        ("types.hpp", QUAL_HEADER),
        ("cb.cpp", QUAL_DEFINITIONS),
        ("main.cpp", QUAL_MAIN_BARE),
    ];
    assert_cases_with_control(&[
        Case {
            label: "qualified values, caller dirty (References)",
            files: base,
            edits: &[("main.cpp", QUAL_MAIN)],
            rel: RelType::References,
            reason: Some("function_reference"),
            source_file: "main.cpp",
            expected: values,
        },
        Case {
            label: "qualified call, caller dirty (Calls)",
            files: base,
            edits: &[("main.cpp", QUAL_MAIN)],
            rel: RelType::Calls,
            reason: None,
            source_file: "main.cpp",
            expected: &[("call_qual", "cb.cpp", "cb_qual")],
        },
        Case {
            label: "qualified values, definition and caller dirty (References)",
            files: &[
                ("types.hpp", QUAL_HEADER),
                ("cb.cpp", "#include \"types.hpp\"\n"),
                ("main.cpp", QUAL_MAIN_BARE),
            ],
            edits: &[("cb.cpp", QUAL_DEFINITIONS), ("main.cpp", QUAL_MAIN)],
            rel: RelType::References,
            reason: Some("function_reference"),
            source_file: "main.cpp",
            expected: values,
        },
    ]);
}

// ── A15g: a C++ namespace typedef or alias ──────────────────────────────

const ALIAS_HEADER: &str = "#include <utility>\nnamespace ns {\nstruct St { int x; };\nusing Al = std::pair<int,int>;\ntypedef std::pair<int,int> AlT;\n}\ntypedef int Gint;\n";
const ALIAS_C_BARE: &str = "#include \"types.hpp\"\n";
const ALIAS_C_USER: &str = "#include \"types.hpp\"\nAl al_ret(void) { Al a; return a; }\nAlT alt_ret(void) { AlT a; return a; }\nGint g_ret(void) { return 0; }\n";

/// Stands for: C binds the owner-less typedef and never a namespaced one,
/// whether the header is clean or dirty, and a namespaced alias that moves
/// out of its namespace in a dirty header starts to bind.
#[test]
fn test_overlay_c_family_field_c_caller_cpp_namespace_typedef_matches_reindex() {
    assert_cases_with_control(&[
        Case {
            label: "C caller, namespace alias, caller dirty",
            files: &[("types.hpp", ALIAS_HEADER), ("m.c", ALIAS_C_BARE)],
            edits: &[("m.c", ALIAS_C_USER)],
            rel: RelType::Accesses,
            reason: Some("type_annotation"),
            source_file: "m.c",
            expected: &[("g_ret", "types.hpp", "Gint")],
        },
        Case {
            label: "C caller, namespace alias, header and caller dirty",
            files: &[("types.hpp", ""), ("m.c", ALIAS_C_BARE)],
            edits: &[("types.hpp", ALIAS_HEADER), ("m.c", ALIAS_C_USER)],
            rel: RelType::Accesses,
            reason: Some("type_annotation"),
            source_file: "m.c",
            expected: &[("g_ret", "types.hpp", "Gint")],
        },
        Case {
            label: "C caller, alias moved out of the namespace, header and caller dirty",
            files: &[("types.hpp", ALIAS_HEADER), ("m.c", ALIAS_C_BARE)],
            edits: &[
                (
                    "types.hpp",
                    "#include <utility>\nusing Al = std::pair<int,int>;\ntypedef std::pair<int,int> AlT;\ntypedef int Gint;\n",
                ),
                ("m.c", ALIAS_C_USER),
            ],
            rel: RelType::Accesses,
            reason: Some("type_annotation"),
            source_file: "m.c",
            expected: &[
                ("al_ret", "types.hpp", "Al"),
                ("alt_ret", "types.hpp", "AlT"),
                ("g_ret", "types.hpp", "Gint"),
            ],
        },
    ]);
}

// ── A15h: `extern` on a definition with a body ──────────────────────────

const EXT_HEADER: &str = "int ext(int);\n";
const EXT_PLAIN: &str = "#include \"api.h\"\nint ext(int x) { return x; }\n";
const EXT_EXTERN: &str = "#include \"api.h\"\nextern int ext(int x) { return x; }\n";
const EXT_USER_BARE: &str = "#include \"api.h\"\nint run(void) { return 0; }\n";
const EXT_USER: &str = "#include \"api.h\"\nint run(void) { return ext(1); }\n";

/// Stands for: the `extern` keyword on a definition changes no `Calls` row,
/// in the base graph and when a dirty file toggles it; deleting the
/// definition leaves the prototype.
#[test]
fn test_overlay_c_family_field_cpp_caller_extern_keyword_matches_reindex() {
    assert_cases_with_control(&[
        Case {
            label: "extern definition in the base, caller dirty",
            files: &[
                ("api.h", EXT_HEADER),
                ("lib.c", EXT_EXTERN),
                ("main.cpp", EXT_USER_BARE),
            ],
            edits: &[("main.cpp", EXT_USER)],
            rel: RelType::Calls,
            reason: None,
            source_file: "main.cpp",
            expected: &[("run", "lib.c", "ext")],
        },
        Case {
            label: "extern added by a dirty definition, caller dirty",
            files: &[
                ("api.h", EXT_HEADER),
                ("lib.c", EXT_PLAIN),
                ("main.cpp", EXT_USER_BARE),
            ],
            edits: &[("lib.c", EXT_EXTERN), ("main.cpp", EXT_USER)],
            rel: RelType::Calls,
            reason: None,
            source_file: "main.cpp",
            expected: &[("run", "lib.c", "ext")],
        },
        Case {
            label: "extern removed by a dirty definition, caller dirty",
            files: &[
                ("api.h", EXT_HEADER),
                ("lib.c", EXT_EXTERN),
                ("main.cpp", EXT_USER_BARE),
            ],
            edits: &[("lib.c", EXT_PLAIN), ("main.cpp", EXT_USER)],
            rel: RelType::Calls,
            reason: None,
            source_file: "main.cpp",
            expected: &[("run", "lib.c", "ext")],
        },
        Case {
            label: "extern definition emptied, caller dirty",
            files: &[
                ("api.h", EXT_HEADER),
                ("lib.c", EXT_EXTERN),
                ("main.cpp", EXT_USER_BARE),
            ],
            edits: &[("lib.c", ""), ("main.cpp", EXT_USER)],
            rel: RelType::Calls,
            reason: None,
            source_file: "main.cpp",
            expected: &[("run", "api.h", "ext")],
        },
    ]);
}
