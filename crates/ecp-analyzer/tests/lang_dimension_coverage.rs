//! 14-language coverage gate (architecture plan, area 11; repo `CLAUDE.md`,
//! "Parser / core-feature changes require 14-language coverage").
//!
//! A test file in this directory names its languages and its dimension:
//! `<lang>_<dimension>.rs` (`go_frameworks.rs`), or `<dimension>_<lang>.rs`
//! when no language leads (`functionmeta_rust.rs`). Several leading or
//! trailing languages name several (`ts_js_reexport_alias.rs`,
//! `callmeta_c_cpp.rs`); `ts` and `js` stand for TypeScript and JavaScript.
//!
//! Every dimension that one of the 14 mainstream languages has must exist
//! for all 14, or the pair is listed in `lang_dimension_exemptions.txt` as
//! `<dimension> <lang> <reason>`. The list must match exactly: a missing
//! pair fails, and so does an exemption whose pair is now covered or whose
//! dimension is gone. `ECP_BLESS=1` rewrites the list from today's gaps,
//! keeping existing reasons and giving new pairs `pre-existing gap`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

/// The 14 mainstream languages, spelled as the test file names spell them.
const LANGS: [&str; 14] = [
    "typescript",
    "javascript",
    "python",
    "java",
    "kotlin",
    "csharp",
    "go",
    "rust",
    "php",
    "ruby",
    "swift",
    "c",
    "cpp",
    "dart",
];

const EXEMPTIONS: &str = "lang_dimension_exemptions.txt";
const NEW_GAP_REASON: &str = "pre-existing gap";

fn lang_of(word: &str) -> Option<&'static str> {
    match word {
        "ts" => Some("typescript"),
        "js" => Some("javascript"),
        _ => LANGS.iter().copied().find(|lang| *lang == word),
    }
}

/// The dimension and the languages a test file stem names, or `None` when
/// it names no mainstream language. Leading languages win over trailing
/// ones; at least one word stays as the dimension.
fn classify(stem: &str) -> Option<(String, Vec<&'static str>)> {
    let mut words: Vec<&str> = stem.split('_').collect();
    let mut langs = Vec::new();
    while words.len() > 1 {
        match lang_of(words[0]) {
            Some(lang) => {
                langs.push(lang);
                words.remove(0);
            }
            None => break,
        }
    }
    if langs.is_empty() {
        while words.len() > 1 {
            match lang_of(words[words.len() - 1]) {
                Some(lang) => {
                    langs.push(lang);
                    words.pop();
                }
                None => break,
            }
        }
    }
    (!langs.is_empty()).then(|| (words.join("_"), langs))
}

/// Dimension → the languages that have a test file for it.
fn coverage(stems: impl IntoIterator<Item = String>) -> BTreeMap<String, BTreeSet<&'static str>> {
    let mut cov: BTreeMap<String, BTreeSet<&'static str>> = BTreeMap::new();
    for stem in stems {
        if let Some((dimension, langs)) = classify(&stem) {
            cov.entry(dimension).or_default().extend(langs);
        }
    }
    cov
}

/// Every `(dimension, lang)` pair a covered dimension lacks.
fn gaps(cov: &BTreeMap<String, BTreeSet<&'static str>>) -> BTreeSet<(String, String)> {
    cov.iter()
        .flat_map(|(dimension, langs)| {
            LANGS
                .iter()
                .filter(move |lang| !langs.contains(*lang))
                .map(move |lang| (dimension.clone(), lang.to_string()))
        })
        .collect()
}

/// `(dimension, lang)` → reason, from the exemption file's text.
fn parse_exemptions(text: &str) -> BTreeMap<(String, String), String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let mut parts = line.splitn(3, ' ');
            let dimension = parts.next().unwrap().to_string();
            let lang = parts
                .next()
                .unwrap_or_else(|| panic!("exemption without a language: {line}"))
                .to_string();
            let reason = parts.next().map(str::trim).unwrap_or_default();
            assert!(!reason.is_empty(), "exemption without a reason: {line}");
            assert!(
                LANGS.contains(&lang.as_str()),
                "exemption names no mainstream language: {line}"
            );
            ((dimension, lang), reason.to_string())
        })
        .collect()
}

/// Missing pairs with no exemption, then exemptions that exempt nothing.
fn mismatches(
    gaps: &BTreeSet<(String, String)>,
    exempt: &BTreeMap<(String, String), String>,
) -> Vec<String> {
    let missing = gaps
        .iter()
        .filter(|pair| !exempt.contains_key(*pair))
        .map(|(dimension, lang)| format!("+ {dimension} {lang}: no test file and no exemption"));
    let stale = exempt
        .keys()
        .filter(|pair| !gaps.contains(*pair))
        .map(|(dimension, lang)| format!("- {dimension} {lang}: exempted, but nothing is missing"));
    missing.chain(stale).collect()
}

fn render(
    gaps: &BTreeSet<(String, String)>,
    exempt: &BTreeMap<(String, String), String>,
) -> String {
    let mut text = String::from(
        "# <dimension> <lang> <reason>: a test dimension that lacks this language on purpose\n\
         # or for now. Written by `ECP_BLESS=1 cargo test -p ecp-analyzer --test\n\
         # lang_dimension_coverage`; replace `pre-existing gap` with the real reason.\n",
    );
    for pair in gaps {
        let reason = exempt.get(pair).map_or(NEW_GAP_REASON, String::as_str);
        text.push_str(&format!("{} {} {reason}\n", pair.0, pair.1));
    }
    text
}

fn bless_mode() -> bool {
    std::env::var("ECP_BLESS").is_ok_and(|v| !v.is_empty() && v != "0")
}

#[test]
fn every_test_dimension_covers_the_14_languages_or_is_exempted() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let stems = fs::read_dir(&dir).unwrap().flatten().filter_map(|entry| {
        let path = entry.path();
        (path.is_file() && path.extension().is_some_and(|ext| ext == "rs"))
            .then(|| path.file_stem().unwrap().to_string_lossy().into_owned())
    });
    let gaps = gaps(&coverage(stems));
    let path = dir.join(EXEMPTIONS);
    let exempt = parse_exemptions(&fs::read_to_string(&path).unwrap_or_default());
    if bless_mode() {
        fs::write(&path, render(&gaps, &exempt)).unwrap();
        return;
    }
    let report = mismatches(&gaps, &exempt);
    assert!(
        report.is_empty(),
        "{} does not match the language gaps of the test dimensions. Add the \
         missing test files, or rerun with ECP_BLESS=1 and give each new line \
         its reason.\n{}",
        path.display(),
        report.join("\n")
    );
}

#[test]
fn classify_reads_leading_trailing_and_aliased_languages() {
    let owned =
        |dimension: &str, langs: &[&'static str]| Some((dimension.to_string(), langs.to_vec()));
    assert_eq!(classify("go_frameworks"), owned("frameworks", &["go"]));
    assert_eq!(
        classify("functionmeta_rust"),
        owned("functionmeta", &["rust"])
    );
    assert_eq!(
        classify("ts_js_reexport_alias"),
        owned("reexport_alias", &["typescript", "javascript"])
    );
    assert_eq!(classify("callmeta_c_cpp"), owned("callmeta", &["cpp", "c"]));
    assert_eq!(
        classify("cpp_receiver_types"),
        owned("receiver_types", &["cpp"])
    );
    assert_eq!(classify("owner_class_parity_14lang"), None);
    assert_eq!(classify("rust"), None, "a lone language names no dimension");
}

#[test]
fn gaps_and_stale_exemptions_both_fail() {
    let cov = coverage(
        LANGS
            .iter()
            .map(|lang| format!("{lang}_full"))
            .chain(["go_partial".to_string(), "rust_partial".to_string()]),
    );
    let gaps = gaps(&cov);
    assert_eq!(
        gaps.len(),
        12,
        "full is covered; partial lacks 12: {gaps:?}"
    );
    assert!(!gaps.iter().any(|(dimension, _)| dimension == "full"));

    let all = parse_exemptions(&render(&gaps, &BTreeMap::new()));
    assert!(mismatches(&gaps, &all).is_empty());

    let mut short = all.clone();
    short.remove(&("partial".to_string(), "dart".to_string()));
    assert_eq!(
        mismatches(&gaps, &short),
        vec!["+ partial dart: no test file and no exemption"]
    );

    let mut extra = all;
    extra.insert(("partial".to_string(), "go".to_string()), "x".to_string());
    assert_eq!(
        mismatches(&gaps, &extra),
        vec!["- partial go: exempted, but nothing is missing"]
    );
}

#[test]
fn bless_keeps_existing_reasons() {
    let gaps: BTreeSet<(String, String)> = [("d", "go"), ("d", "rust")]
        .iter()
        .map(|(d, l)| (d.to_string(), l.to_string()))
        .collect();
    let mut exempt = BTreeMap::new();
    exempt.insert(
        ("d".to_string(), "go".to_string()),
        "go has no such construct".to_string(),
    );
    let text = render(&gaps, &exempt);
    assert!(text.contains("\nd go go has no such construct\n"), "{text}");
    assert!(text.contains("\nd rust pre-existing gap\n"), "{text}");
}
