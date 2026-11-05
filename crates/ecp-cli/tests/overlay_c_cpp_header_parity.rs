//! C and C++ names declared in a header, out of a dirty file: the session
//! overlay binds what a full reindex of the edited tree binds, confidence
//! included (the index's rule is `ecp-analyzer` `resolution/c_family.rs`,
//! the overlay's `session/view/resolution.rs` `c_family_field`).
//!
//! | case                                   | the reindex has                 |
//! |----------------------------------------|---------------------------------|
//! | C caller, struct / typedef in a `.h`   | `Accesses` to the header types  |
//! | C++ caller, prototype + definition     | `Calls` / `References` to the definition |
//! | C caller, `.h` prototype + `.cpp` definition | `Calls` to the definition |
//! | C caller, function in a C++ namespace  | none                            |
//! | C++ caller, two definitions + prototype | none                           |
//!
//! Where the header or the definition is dirty too, the prototype (or the
//! definition) is an overlay symbol, so the overlay reads its flags from
//! the fresh parse instead of the base graph.

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
