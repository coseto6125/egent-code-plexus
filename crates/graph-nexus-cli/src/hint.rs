//! Output contract helpers — formatters for empty results, errors,
//! suggestions, warnings. See spec §7 (output contract) for rules.
//!
//! All formatters return strings the caller writes to stderr (for warnings)
//! or appends after main stdout (for hints).

/// Empty result message: "No <kind> X found. <next-step suggestion>"
pub fn empty_result(query: &str, kind: &str, suggestion: &str) -> String {
    format!("No {kind} \"{query}\" found.\n→ {suggestion}")
}

/// Fuzzy match suggestion list when a name isn't found.
pub fn fuzzy_suggestions(query: &str, candidates: &[&str]) -> String {
    if candidates.is_empty() {
        return format!("No matches for \"{query}\".");
    }
    let list = candidates.join(" / ");
    format!("No symbol \"{query}\".\n→ Did you mean: {list}?")
}

/// Three-line error: "✗ <what>" / "  cause: <why>" / "  next: <how to recover>"
pub fn error_with_cause(what: &str, cause: &str, next: &str) -> String {
    format!("✗ {what}\n  cause: {cause}\n  next:  {next}")
}

/// One-line stale-index warning for stderr.
pub fn stale_warning(repo_name: &str, age: &str) -> String {
    format!("⚠ Index for \"{repo_name}\" is stale (last built {age} ago).")
}

/// Collision warning for rename pre-flight.
pub fn collision_warning(new_name: &str, existing_locations: &[String]) -> String {
    let locs = existing_locations.join("\n  - ");
    format!(
        "⚠️ COLLISION: \"{new_name}\" already exists at:\n  - {locs}\n→ Choose a different new name, or inspect: gnx inspect {new_name}"
    )
}
