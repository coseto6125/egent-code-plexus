//! Format diff result envelope as text / json / toon.
//!
//! json/toon delegate to `crate::output::emit_to_string` so toon goes through
//! the real `etoon` encoder and json formatting matches the rest of gnx.
//! `text` keeps a custom renderer because the diff envelope's per-section
//! structure doesn't map onto the generic `results`-array text path.

use crate::commands::diff::bindings::BindingsDiff;
use crate::commands::diff::contracts::ContractsDiff;
use crate::commands::diff::routes::RoutesDiff;
use crate::output::{emit_to_string, OutputFormat};
use graph_nexus_core::GnxError;
use serde_json::Value;

pub struct DiffEnvelope<'a> {
    pub baseline_ref: &'a str,
    pub baseline_sha: &'a str,
    pub current_ref: &'a str,
    pub current_sha: &'a str,
    pub bindings: Option<&'a BindingsDiff>,
    pub routes: Option<&'a RoutesDiff>,
    pub contracts: Option<&'a ContractsDiff>,
    pub verbose: bool,
}

pub fn emit(envelope: &DiffEnvelope, format: &str) -> Result<(), GnxError> {
    let fmt = OutputFormat::parse(Some(format));
    match fmt {
        OutputFormat::Json | OutputFormat::Toon | OutputFormat::Llm => {
            let json_value = build_json(envelope);
            println!("{}", emit_to_string(&json_value, fmt)?);
        }
        OutputFormat::Text => emit_text(envelope),
    }
    Ok(())
}

fn build_json(env: &DiffEnvelope) -> Value {
    let mut sections = serde_json::Map::new();
    if let Some(b) = env.bindings {
        sections.insert(
            "bindings".into(),
            serde_json::to_value(b).unwrap_or(Value::Null),
        );
    }
    if let Some(r) = env.routes {
        sections.insert(
            "routes".into(),
            serde_json::to_value(r).unwrap_or(Value::Null),
        );
    }
    if let Some(c) = env.contracts {
        sections.insert(
            "contracts".into(),
            serde_json::to_value(c).unwrap_or(Value::Null),
        );
    }
    serde_json::json!({
        "baseline": {"ref": env.baseline_ref, "sha": env.baseline_sha},
        "current":  {"ref": env.current_ref,  "sha": env.current_sha},
        "sections": sections,
    })
}

fn emit_text(env: &DiffEnvelope) {
    let bsha = &env.baseline_sha[..env.baseline_sha.len().min(7)];
    let csha = &env.current_sha[..env.current_sha.len().min(7)];
    println!(
        "═══ Graph Δ ({} {}→{} {}) ═══",
        env.baseline_ref, bsha, env.current_ref, csha,
    );

    let limit = if env.verbose { usize::MAX } else { 10 };

    if let Some(b) = env.bindings {
        println!("\n─ Section: bindings ─");
        println!("  new_resolutions: {}", b.new_resolutions.len());
        println!("  tier_changes:    {}", b.tier_changes.len());
        println!("  target_changes:  {}", b.target_changes.len());
        println!("  removed:         {}", b.removed.len());
        for chg in b.new_resolutions.iter().take(limit) {
            println!("  [NEW]     {}::{}", chg.src_file, chg.name);
        }
        for chg in b.tier_changes.iter().take(limit) {
            let from = chg
                .before
                .as_ref()
                .and_then(|d| d.tier.as_deref())
                .unwrap_or("?");
            let to = chg
                .after
                .as_ref()
                .and_then(|d| d.tier.as_deref())
                .unwrap_or("?");
            println!(
                "  [TIER]    {}::{} ({} → {})",
                chg.src_file, chg.name, from, to
            );
        }
        for chg in b.target_changes.iter().take(limit) {
            println!("  [TARGET]  {}::{}", chg.src_file, chg.name);
        }
        for chg in b.removed.iter().take(limit) {
            println!("  [REMOVED] {}::{}", chg.src_file, chg.name);
        }
    }

    if let Some(r) = env.routes {
        println!("\n─ Section: routes ─");
        println!("  added:    {}", r.added.len());
        println!("  removed:  {}", r.removed.len());
        println!("  modified: {}", r.modified.len());
        for entry in r.added.iter().take(limit) {
            println!(
                "  [ADDED]    {} {} → {}:{}",
                entry.method, entry.path, entry.handler_file, entry.handler_line
            );
        }
        for entry in r.removed.iter().take(limit) {
            println!("  [REMOVED]  {} {}", entry.method, entry.path);
        }
        for chg in r.modified.iter().take(limit) {
            println!("  [MODIFIED] {} {}", chg.after.method, chg.after.path);
        }
    }

    if let Some(c) = env.contracts {
        println!("\n─ Section: contracts ─");
        println!("  added:    {}", c.added.len());
        println!("  removed:  {}", c.removed.len());
        println!("  modified: {}", c.modified.len());
        for entry in c.added.iter().take(limit) {
            println!("  [ADDED]    {}:{}", entry.kind, entry.identifier);
        }
        for entry in c.removed.iter().take(limit) {
            println!("  [REMOVED]  {}:{}", entry.kind, entry.identifier);
        }
        for chg in c.modified.iter().take(limit) {
            println!("  [MODIFIED] {}:{}", chg.after.kind, chg.after.identifier);
        }
    }
}
