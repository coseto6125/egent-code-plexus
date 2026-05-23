//! Dump residual uid-collision clusters from a graph.bin.
//!
//! Usage:
//!   cargo run --release --example dump_uid_collisions -- <graph.bin> [top_n]
//!
//! Reads BlindSpot records, filters to `kind == "uid-collision"`, parses the
//! `hint` field to recover `(second_kind, second_path, second_owner, second_name)`,
//! derives lang from path extension, then collapses by
//! `(lang, second_kind, second_owner, second_name)`. Output: cluster_size DESC.
//!
//! Why cluster collapse: a single parser gap (e.g. owner_class missing on Go
//! struct fields named `File`) can fire 234 distinct BlindSpot records. The
//! raw count `uid-collision: 4092` hides the fact that those 4,092 records
//! collapse into ~20-40 cluster identities. The cluster view is the
//! actionable one for ranking root-cause fixes.

use ecp_core::graph::ArchivedZeroCopyGraph;
use memmap2::Mmap;
use std::collections::HashMap;
use std::fs::File;

fn lang_from_path(p: &str) -> &'static str {
    let ext = p.rsplit('.').next().unwrap_or("");
    match ext {
        "ts" | "tsx" => "TypeScript",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "py" => "Python",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "cs" => "CSharp",
        "go" => "Go",
        "rs" => "Rust",
        "php" => "PHP",
        "rb" => "Ruby",
        "swift" => "Swift",
        "c" => "C",
        "h" => "C++",
        "cc" | "cpp" | "cxx" | "hpp" | "hxx" | "hh" => "C++",
        "dart" => "Dart",
        "sh" | "bash" => "Bash",
        "lua" | "luau" => "Lua",
        "vue" => "Vue",
        "svelte" => "Svelte",
        "yml" | "yaml" => "YAML",
        _ => "?",
    }
}

fn parse_hint(hint: &str) -> Option<(&str, &str, &str, &str)> {
    let second = hint.split(" second=").nth(1)?;
    let mut parts = second.splitn(4, ':');
    let kind = parts.next()?;
    let path = parts.next()?;
    let owner = parts.next()?;
    let name = parts.next()?;
    Some((kind, path, owner, name))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).cloned().unwrap_or_else(|| {
        eprintln!("usage: dump_uid_collisions <graph.bin> [top_n]");
        std::process::exit(2);
    });
    let top_n: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(40);

    let f = File::open(&path).expect("open graph.bin");
    let mmap = unsafe { Mmap::map(&f).expect("mmap") };
    let g = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&mmap).expect("rkyv");

    // (lang, second_kind, second_owner, second_name) → (count, sample_path)
    let mut clusters: HashMap<(String, String, String, String), (u32, String)> = HashMap::new();
    let mut total_uid_collision: u32 = 0;
    let mut total_hint_unparsed: u32 = 0;

    for bs in g.blind_spots.iter() {
        let kind = bs.kind.resolve(&g.string_pool);
        if kind != "uid-collision" {
            continue;
        }
        total_uid_collision += 1;
        let hint = bs.hint.resolve(&g.string_pool);
        let Some((second_kind, second_path, second_owner, second_name)) = parse_hint(hint) else {
            total_hint_unparsed += 1;
            continue;
        };
        let lang = lang_from_path(second_path);
        let key = (
            lang.to_string(),
            second_kind.to_string(),
            second_owner.to_string(),
            second_name.to_string(),
        );
        clusters
            .entry(key)
            .and_modify(|(c, _)| *c += 1)
            .or_insert((1, second_path.to_string()));
    }

    let mut rows: Vec<((String, String, String, String), (u32, String))> =
        clusters.into_iter().collect();
    rows.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));

    println!("total uid-collision records  : {}", total_uid_collision);
    println!("distinct (lang,kind,own,name): {}", rows.len());
    println!("hint parse failures          : {}", total_hint_unparsed);
    println!();
    println!(
        "{:>5} {:<12} {:<14} {:<28} {:<28} {}",
        "count", "lang", "kind", "owner_class", "name", "sample_path"
    );
    println!("{}", "-".repeat(120));

    let mut cumulative: u32 = 0;
    for (i, ((lang, kind, owner, name), (count, sample))) in rows.iter().enumerate() {
        if i >= top_n {
            break;
        }
        cumulative += count;
        let owner_disp = if owner.is_empty() {
            "(none)"
        } else {
            owner.as_str()
        };
        let sample_short = if sample.len() > 50 {
            format!("...{}", &sample[sample.len() - 47..])
        } else {
            sample.clone()
        };
        println!(
            "{:>5} {:<12} {:<14} {:<28} {:<28} {}",
            count, lang, kind, owner_disp, name, sample_short
        );
    }
    println!();
    println!(
        "top {} clusters cover {} / {} ({:.1}%)",
        top_n,
        cumulative,
        total_uid_collision,
        100.0 * cumulative as f64 / total_uid_collision.max(1) as f64
    );
}
