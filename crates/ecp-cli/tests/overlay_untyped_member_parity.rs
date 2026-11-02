//! A member call on an untyped receiver out of a dirty file (Rule U): the
//! session overlay binds what a full reindex binds, with the same confidence
//! and reason. In a guess language (Rust, Go, Java, Swift, Dart, C++,
//! TypeScript) the bind by name alone is 0.55 `call:untyped-member`, whichever
//! tier found the method; elsewhere it keeps the tier's confidence. A Rust
//! method-syntax call weighs methods only at the global tier (U2), so a
//! same-named free `fn` neither binds nor makes the name ambiguous, in the
//! index (`SymbolTable::global_field_candidates`) and in the overlay
//! (`global_tier`) alike.
//!
//! The graphs are hand-built from a one-line spec so one fixture serves all
//! 14 languages: the extension picks the language, and the spec names the
//! declarations (`method:retain@Dirty`, `free:retain`) and the caller
//! (`user:retain` calls `retain` on an untyped receiver; `caller:retain@Dirty`
//! does so from a method of `Dirty`). `pad` adds a function, so an edit moves
//! every declaration below it.

#[path = "common/parity.rs"]
mod parity;

use ecp_core::analyzer::types::{CallSite, LocalGraph, RawNode};
use ecp_core::graph::{NodeKind, RelType};
use parity::{calls_after_edit, edges_after_edit};

const GUESS_EXTENSIONS: [&str; 7] = ["rs", "go", "java", "swift", "dart", "cpp", "ts"];
const OTHER_EXTENSIONS: [&str; 6] = ["py", "rb", "php", "js", "kt", "c"];

fn node(name: &str, kind: NodeKind, row: u32, owner: Option<&str>, calls: Vec<String>) -> RawNode {
    RawNode {
        name: name.into(),
        kind,
        span: (row, 0, row + 2, 0),
        is_exported: false,
        heritage: vec![],
        type_annotation: None,
        decorators: vec![],
        calls,
        field_reads: Vec::new(),
        owner_class: owner.map(str::to_string),
        content_hash: 0,
    }
}

/// The graph a spec describes; each token takes the next four rows.
fn parse(path: &str, spec: &str) -> LocalGraph {
    let mut nodes = Vec::new();
    for (i, token) in spec.split_whitespace().enumerate() {
        let row = 4 * i as u32 + 1;
        let (role, rest) = token.split_once(':').unwrap_or((token, ""));
        let (name, owner) = match rest.split_once('@') {
            Some((name, owner)) => (name, Some(owner)),
            None => (rest, None),
        };
        let untyped = vec![CallSite::untyped_member(name)];
        match role {
            "pad" => nodes.push(node("pad", NodeKind::Function, row, None, vec![])),
            "free" => nodes.push(node(name, NodeKind::Function, row, None, vec![])),
            "method" => {
                let owner = owner.expect("a method has an owner");
                nodes.push(node(owner, NodeKind::Class, row, None, vec![]));
                nodes.push(node(name, NodeKind::Method, row + 1, Some(owner), vec![]));
            }
            "user" => nodes.push(node("user", NodeKind::Function, row, None, untyped)),
            "caller" => nodes.push(node("caller", NodeKind::Method, row, owner, untyped)),
            other => panic!("unknown role {other}"),
        }
    }
    LocalGraph {
        file_path: path.into(),
        content_hash: spec.bytes().fold([0; 8], |mut hash, byte| {
            hash[usize::from(byte) % 8] ^= byte;
            hash
        }),
        nodes,
        ..Default::default()
    }
}

/// `(files, edited file, its edited spec)`.
type Case = (Vec<(String, String)>, usize);

fn cases(ext: &str) -> Vec<(&'static str, Case)> {
    let file = |stem: &str, spec: &str| (format!("src/{stem}.{ext}"), spec.to_string());
    vec![
        (
            "caller edited, method in another file",
            (
                vec![
                    file("def", "method:retain@Dirty"),
                    file("use", "user:retain"),
                ],
                1,
            ),
        ),
        (
            "caller edited, method in its own file",
            (vec![file("one", "method:retain@Dirty user:retain")], 0),
        ),
        (
            "method edited, caller untouched",
            (
                vec![
                    file("def", "method:retain@Dirty"),
                    file("use", "user:retain"),
                ],
                0,
            ),
        ),
        (
            "two methods",
            (
                vec![
                    file("def", "method:retain@Dirty"),
                    file("other", "method:retain@Other"),
                    file("use", "user:retain"),
                ],
                2,
            ),
        ),
        (
            "the caller's own type",
            (
                vec![file("def", "method:retain@Dirty caller:retain@Dirty")],
                0,
            ),
        ),
        (
            "a method and a free fn of the name",
            (
                vec![
                    file("def", "method:retain@Dirty"),
                    file("free", "free:retain"),
                    file("use", "user:retain"),
                ],
                2,
            ),
        ),
    ]
}

fn selects(_: &str, name: &str) -> bool {
    name == "user" || name == "caller"
}

#[test]
fn test_build_untyped_member_call_matches_reindex_across_14_languages() {
    let mut failures = Vec::new();
    for ext in GUESS_EXTENSIONS.into_iter().chain(OTHER_EXTENSIONS) {
        let guess = GUESS_EXTENSIONS.contains(&ext);
        for (label, (files, edited)) in cases(ext) {
            let base: Vec<(&str, &str)> = files
                .iter()
                .map(|(path, spec)| (path.as_str(), spec.as_str()))
                .collect();
            let padded = format!("pad {}", base[edited].1);
            let edits = [(base[edited].0, padded.as_str())];

            let parity::Parity { reindex, overlay } =
                calls_after_edit(&parse, &base, &edits, &selects);
            if overlay != reindex {
                failures.push(format!(
                    "{ext} / {label}: overlay {overlay:#?}\nreindex {reindex:#?}"
                ));
            }
            let bound = |label: &str| label.contains("another file") || label.contains("own file");
            if guess && (bound(label) || label.contains("untouched")) {
                let want = (0.55, "call:untyped-member");
                let reasoned = edges_after_edit(&parse, &base, &edits, RelType::Calls, &selects);
                let tags = |edges: &[parity::ReasonedEdge]| -> Vec<(f32, String)> {
                    edges
                        .iter()
                        .map(|e| (e.edge.confidence, e.reason.clone()))
                        .collect()
                };
                let want = vec![(want.0, want.1.to_string())];
                for (side, edges) in [
                    ("reindex", &reasoned.reindex),
                    ("overlay", &reasoned.overlay),
                ] {
                    if tags(edges) != want {
                        failures.push(format!("{ext} / {label}: {side} {:?}", tags(edges)));
                    }
                }
            }
            if label.contains("free fn") {
                // U2: only a Rust method-syntax call drops the free fn from
                // the field; the other languages stay ambiguous.
                let bound = !reindex.is_empty();
                if bound != (ext == "rs") {
                    failures.push(format!("{ext} / {label}: reindex {reindex:#?}"));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
