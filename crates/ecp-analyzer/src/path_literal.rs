//! Language-neutral predicate + sink classifier for `PathLiteral` extraction.
//!
//! Per-lang parsers run `string_literal` captures from their `queries.scm`,
//! strip surrounding quotes to get the raw value, then call:
//!
//!   1. [`is_path_shaped`] to filter out non-path strings (display messages,
//!      format strings with `\n` escapes, URLs, etc.).
//!   2. [`classify_sink`] with the enclosing `call_expression` callee name
//!      (resolved via the existing `extract_<lang>_calls` helpers) to label
//!      what the program does with the literal.
//!
//! Both functions take only `&str` so per-language drift is impossible.
//! Language-specific escape-sequence rules are unified here: the predicate
//! treats `\\` as a path separator pair and known C-style escapes
//! (`\n \t \r \0 \' \" \x \u`) as format escapes. This covers Rust, Python,
//! JS/TS, Java, Kotlin, C#, C/C++, Go, Swift, Dart, Ruby, PHP — the 14
//! mainstream targets in this workspace. Languages that allow additional
//! escape forms (e.g. PHP `\$`, Ruby `\#`) fall through to the trailing
//! "anything else" branch which conservatively treats them as path
//! separators; a downstream raw-vs-non-raw distinction would tighten this
//! at the cost of per-lang divergence, deferred to P2.

/// Decide whether a string literal value (already stripped of surrounding
/// quotes / raw-string sigils) is shaped like a filesystem path or a known
/// config / data file. Returns `false` for empty, whitespace-only, URL-like
/// strings, and format strings whose only "separator" is a standard escape
/// sequence (`\n` etc.).
pub fn is_path_shaped(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    if s.chars().all(|c| c.is_ascii_whitespace()) {
        return false;
    }
    if s.starts_with("http://") || s.starts_with("https://") || s.starts_with("ws://") {
        return false;
    }
    if has_path_separator(s) {
        return true;
    }
    PATH_SUFFIXES.iter().any(|sfx| s.ends_with(sfx))
}

/// Walks the literal looking for a path separator. `'/'` is unambiguous.
/// `'\\'` (one source byte) is a path separator iff it is not followed by a
/// standard C-style escape continuation. The `\\\\` pair (two source bytes
/// = one literal `\` at runtime) is treated as a path separator.
fn has_path_separator(s: &str) -> bool {
    if s.contains('/') {
        return true;
    }
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            i += 1;
            continue;
        }
        match bytes.get(i + 1) {
            Some(b'n' | b't' | b'r' | b'0' | b'\'' | b'"' | b'x' | b'u') => i += 2,
            Some(_) | None => return true,
        }
    }
    false
}

/// Suffixes that mark a string as a likely config / data path even without
/// any separator. Restricted to formats that imply "this names a file the
/// program reads or writes": leaving out source-code suffixes (`.rs` etc.)
/// keeps fixture filenames out of the result set.
const PATH_SUFFIXES: &[&str] = &[
    ".json", ".jsonl", ".toml", ".lock", ".yaml", ".yml", ".log", ".rkyv", ".bin", ".sqlite",
    ".db", ".sh", ".bat", ".ps1", ".env", ".cfg", ".conf", ".ini", ".csv", ".tsv", ".xml", ".sql",
    ".md", ".txt", ".html", ".pem", ".key", ".crt", ".proto",
];

/// Coarse classification of what the embedding callsite does with the
/// path literal. Encoded into `Edge.reason` as `sink:<kind>` for cypher
/// queries that distinguish readers from writers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkKind {
    Read,
    Write,
    OpenRead,
    OpenWrite,
    Join,
    ExtChange,
    Free,
}

/// Confidence of the sink classification. `High` = the callee name uniquely
/// identifies the operation (e.g. `read_to_string`, `with_extension`).
/// `Medium` = the name is overloaded across types or could mean either read
/// or write (e.g. `open`, `join`, `read`, `write`). The PR consumer can
/// filter on confidence without re-parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkConfidence {
    High,
    Medium,
}

impl SinkKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::OpenRead => "open-read",
            Self::OpenWrite => "open-write",
            Self::Join => "join",
            Self::ExtChange => "ext-change",
            Self::Free => "free",
        }
    }
}

impl SinkConfidence {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
        }
    }
}

/// Render the `Edge.reason` payload for a `UsesPathLiteral` edge.
pub fn sink_reason(kind: SinkKind, conf: SinkConfidence) -> String {
    format!("sink:{}|confidence:{}", kind.as_str(), conf.as_str())
}

/// Returns `true` when `callee` is a HIGH-confidence ExtChange operation
/// (`with_extension`, `with_file_name`, `set_extension`, etc.). Per-lang
/// extractors use this as a sink-override: when the callee is a known
/// ext-change op, the literal value (`"json"`, `"toml"`, ...) is accepted
/// even if `is_path_shaped` rejects it as too short / too plain.
///
/// Conservative: only HIGH-confidence ext-change names; bare extension
/// strings appearing in unknown callees still get the `is_path_shaped`
/// gate so option/error tokens like `"json"` (passed to a serde format
/// picker) don't accidentally land in the path-literal index.
pub fn is_ext_change_callee(callee: Option<&str>) -> bool {
    let Some(name) = callee else {
        return false;
    };
    matches!(
        trailing_ident(name),
        "with_extension" | "with_file_name" | "set_extension" | "set_file_name"
    )
}

/// Classify a call-site sink based on the resolved callee name produced by
/// the per-language `extract_<lang>_calls` helpers. The input may include
/// a receiver prefix (`Dog.method`, `Path::new`, `fs.readFile`) — only the
/// trailing identifier is matched, which keeps the table language-neutral.
///
/// Returns `(SinkKind::Free, SinkConfidence::High)` when:
///   - `callee` is `None` (the literal isn't inside a call_expression).
///   - The callee name doesn't match any known file-op pattern.
///
/// "Free" is honest signal — the LLM consumer should treat unclassified
/// sinks as "we know this literal is a path, but don't know what's done
/// with it", which is materially better than silent omission.
pub fn classify_sink(callee: Option<&str>) -> (SinkKind, SinkConfidence) {
    let Some(name) = callee else {
        return (SinkKind::Free, SinkConfidence::High);
    };
    let bare = trailing_ident(name);

    use SinkConfidence::{High, Medium};
    use SinkKind::*;
    match bare {
        // ── HIGH-confidence reads (name uniquely identifies a read op) ──
        "read_to_string" | "read_to_end" | "readText" | "read_text" | "ReadAllText"
        | "ReadAllBytes" | "ReadAllLines" | "readFile" | "readFileSync" | "ReadFile" | "slurp"
        | "read_all" | "readAsString" | "readAsStringSync" | "readAsBytes"
        // pathlib.Path snake_case equivalents (FU-2026-05-23-023 Python chain promotion)
        | "read_bytes"
        // Swift labelled-arg constructors (FU-2026-05-23-023 Swift): the
        // enclosing_callee promotes the arg label over the type name, so
        // `String(contentsOfFile: ...)` and `Data(contentsOf: ...)` see
        // the label as the callee.
        | "contentsOfFile" | "contentsOf" => (Read, High),

        // ── HIGH-confidence writes ────────────────────────────────────
        "write_all" | "atomic_write" | "atomic_write_json" | "writeFile" | "writeFileSync"
        | "WriteFile" | "WriteAllText" | "WriteAllBytes" | "WriteAllLines"
        | "file_put_contents" | "writeAsString" | "writeAsStringSync" | "writeAsBytes"
        // Kotlin stdlib + java.io.File.writeText/writeBytes; symmetric with
        // the readText/readBytes/readLines listed in HIGH reads above.
        | "writeText" | "writeBytes" | "appendText" | "appendBytes"
        // pathlib.Path snake_case equivalents (FU-2026-05-23-023 Python chain promotion)
        | "write_text" | "write_bytes"
        // Swift labelled-arg writes (FU-2026-05-23-023 Swift):
        // `str.write(toFile: "...", atomically:, encoding:)`,
        // `Data.write(toFile: "...")`. enclosing_callee promotes the
        // `toFile:` label over the bare `write` so the LLM consumer
        // distinguishes file writes from generic stream writes.
        | "toFile" => (Write, High),

        // ── MEDIUM (overloaded with non-file IO writes) ───────────────
        "write" => (Write, Medium),
        // `read` is overloaded across Read trait, Vec::read, etc.
        "read" => (Read, Medium),
        // ReadFileSync is canonical in C# but `read` alone is ambiguous.
        "file_get_contents" => (Read, High),

        // ── Opens (mode-ambiguous when name is just `open`) ───────────
        "open" | "fopen" => (OpenRead, Medium),
        "create" | "Create" => (OpenWrite, High),

        // ── Path construction ─────────────────────────────────────────
        // ExtChange: pair with `is_ext_change_callee` (above) to enable the
        // sink-override for short non-path-shaped values like `"json"`.
        "with_file_name" | "with_extension" | "set_extension" | "set_file_name" => {
            (ExtChange, High)
        }
        "Path" | "PathBuf" | "Paths" | "URL" => (Join, High),
        "new" => (Join, Medium),
        "from" => (Join, Medium),
        "get" => (Join, Medium),
        "Combine" | "join" | "Join" | "resolve" | "appendingPathComponent" => (Join, Medium),
        "push" => (Join, Medium),

        _ => (Free, High),
    }
}

/// Returns the trailing identifier of a qualified name. Recognises `::`,
/// `.`, and `/` as separators so receiver-bound names (`Dog.method`),
/// scoped names (`fs::read_to_string`) and module paths (`os/path/join`)
/// all collapse to the bare method/function ident.
fn trailing_ident(name: &str) -> &str {
    let mut last = 0;
    for (i, c) in name.char_indices() {
        if c == '.' || c == ':' || c == '/' || c == '\\' {
            last = i + c.len_utf8();
        }
    }
    &name[last..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predicate_accepts_obvious_paths() {
        for s in [
            "session_meta.json",
            "crates/foo/Cargo.toml",
            "src/lib.rs",
            "./relative/path",
            "~/foo",
            "C:\\foo",
            "data.json",
            "config.toml",
            "schema.sql",
            "settings.yaml",
        ] {
            assert!(is_path_shaped(s), "expected path-shaped: {s:?}");
        }
    }

    #[test]
    fn predicate_rejects_non_paths() {
        for s in [
            "",
            "   ",
            "hello",
            "http://example.com/x.json",
            "https://api.com",
            "{}\n;; ---- framework queries ----\n{}",
            "did not return JSON\nstdout=",
            "line1\nline2",
        ] {
            assert!(!is_path_shaped(s), "expected non-path: {s:?}");
        }
    }

    #[test]
    fn predicate_distinguishes_escape_from_separator() {
        assert!(is_path_shaped("C:\\Users\\me"), "double-backslash = path");
        assert!(!is_path_shaped("a\\nb"), "single backslash + n = escape");
        assert!(!is_path_shaped("a\\tb"), "single backslash + t = escape");
        assert!(
            is_path_shaped("a\\xfoo.json"),
            "x escape doesn't strip suffix"
        ); // suffix wins
    }

    #[test]
    fn classify_sink_high_confidence_reads() {
        let (k, c) = classify_sink(Some("std::fs::read_to_string"));
        assert_eq!(k, SinkKind::Read);
        assert_eq!(c, SinkConfidence::High);

        let (k, c) = classify_sink(Some("readFileSync"));
        assert_eq!(k, SinkKind::Read);
        assert_eq!(c, SinkConfidence::High);
    }

    #[test]
    fn classify_sink_medium_for_overloaded_names() {
        let (k, c) = classify_sink(Some("foo.join"));
        assert_eq!(k, SinkKind::Join);
        assert_eq!(c, SinkConfidence::Medium);

        let (k, c) = classify_sink(Some("File::open"));
        assert_eq!(k, SinkKind::OpenRead);
        assert_eq!(c, SinkConfidence::Medium);
    }

    #[test]
    fn classify_sink_free_for_unknown_or_none() {
        let (k, c) = classify_sink(None);
        assert_eq!(k, SinkKind::Free);
        assert_eq!(c, SinkConfidence::High);

        let (k, c) = classify_sink(Some("unrelated_fn"));
        assert_eq!(k, SinkKind::Free);
        assert_eq!(c, SinkConfidence::High);
    }

    #[test]
    fn is_ext_change_callee_recognises_canonical_names() {
        assert!(is_ext_change_callee(Some("with_extension")));
        assert!(is_ext_change_callee(Some("with_file_name")));
        assert!(is_ext_change_callee(Some("set_extension")));
        assert!(is_ext_change_callee(Some("Path::with_extension")));
        assert!(is_ext_change_callee(Some("foo.bar.with_extension")));
        assert!(!is_ext_change_callee(None));
        assert!(!is_ext_change_callee(Some("read_to_string")));
        assert!(!is_ext_change_callee(Some("join")));
    }

    #[test]
    fn classify_sink_extchange_high_for_ext_change_names() {
        for name in [
            "with_extension",
            "with_file_name",
            "set_extension",
            "set_file_name",
        ] {
            let (k, c) = classify_sink(Some(name));
            assert_eq!(k, SinkKind::ExtChange, "kind mismatch for {name}");
            assert_eq!(c, SinkConfidence::High, "confidence mismatch for {name}");
        }
    }

    #[test]
    fn sink_reason_encoding_is_stable() {
        assert_eq!(
            sink_reason(SinkKind::Read, SinkConfidence::High),
            "sink:read|confidence:high"
        );
        assert_eq!(
            sink_reason(SinkKind::Free, SinkConfidence::High),
            "sink:free|confidence:high"
        );
        assert_eq!(
            sink_reason(SinkKind::Join, SinkConfidence::Medium),
            "sink:join|confidence:medium"
        );
    }

    #[test]
    fn trailing_ident_handles_all_separators() {
        assert_eq!(trailing_ident("foo"), "foo");
        assert_eq!(trailing_ident("std::fs::read_to_string"), "read_to_string");
        assert_eq!(trailing_ident("path.join"), "join");
        assert_eq!(trailing_ident("os/path/join"), "join");
        assert_eq!(trailing_ident("Dog.method"), "method");
    }
}
