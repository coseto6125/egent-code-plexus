//! `agent-dispatch` hook: PreToolUse(Agent|Task) tripwire that catches the
//! "dispatch an Explore agent for a structural code question" reflex and
//! redirects it to the graph.
//!
//! Static instructions (CLAUDE.md / SKILL.md "ecp-first") demonstrably leak:
//! the dispatch decision happens before the model re-reads any of them, and
//! the harness's own delegation guidance pulls the other way. A hook is the
//! only mechanism that runs AT the dispatch moment, so it is the
//! deterministic backstop: deny once with a reason naming the right ecp
//! verb; the model re-routes (or legitimately re-dispatches — see escape
//! hatches below).
//!
//! Three-layer detection, precision over recall (a missed reminder costs
//! tokens; a false block costs one retry). Validated against held-out
//! adversarial paraphrases — the first single-layer regex missed 80% of
//! them (would-break / depends-on / everywhere-instantiated phrasings).
//!
//! Escape hatches (always pass):
//! - the prompt mentions `ecp` — the dispatcher already knows about the
//!   graph (routed through it, or explained why it can't answer)
//! - non-exploration agent types (reviewers, planners, …)
//! - text-domain work (string literals, config, docs, CI yaml, fs layout)

use super::common::HookInput;
use ecp_core::EcpError;
use std::sync::OnceLock;

/// Exploration-shaped agent types worth gating. Specialized agents
/// (reviewers, code-architect, statusline…) pass through: their prompts
/// legitimately mention structure while their job is judgment, not lookup.
const GATED_AGENT_TYPES: &[&str] = &["Explore", "general-purpose", "feature-dev:code-explorer"];

/// Layer 2 — negative guard: the prompt is anchored in text-domain work
/// where grep / file reading is the right tool and ecp has nothing to add.
const NEGATIVE_GUARD: &str = r"(?i)string literal|config (key|value)|log (line|message|string)|filesystem layout|directory layout|readme|changelog|markdown|\.md\b|docs? (page|site)|vendored|env var|grep for the (string|text)|workflow|\.ya?ml|github actions|ci pipeline|runner image|dockerfile";

/// Layer 1 — strong structural signals, including the paraphrases the
/// naive table missed (the 80%-miss class).
const STRUCTURAL_SIGNALS: &str = r"(?i)who calls|callers? of|call ?sites? of|blast radius|where .{0,40}(is |gets )?(defined|declared)|find (the )?definition|map (out )?the architecture|explore (the )?(codebase|code structure|repo structure)|trace (the )?(call|data) ?flow|all (implementations|implementors|subclasses|overrides) of|dependency graph|fan-?(in|out) of|upstream (callers|dependencies)|downstream (callees|dependents)|what (would|will) break|(what|which|everything that) depends on|everywhere .{0,30}(instantiated|referenced|invoked|used)|impact analysis|receives? .{0,40} as (a |an )?(parameter|argument)|consumers of";

fn negative_guard() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(NEGATIVE_GUARD).expect("static regex"))
}

fn structural_signals() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(STRUCTURAL_SIGNALS).expect("static regex"))
}

/// Pure decision core, separated from envelope/process concerns so the
/// adversarial sample suite below exercises exactly what production runs.
/// Returns `true` when the dispatch should be denied toward ecp.
fn should_deny(agent_type: &str, prompt_and_description: &str) -> bool {
    if !agent_type.is_empty() && !GATED_AGENT_TYPES.contains(&agent_type) {
        return false;
    }
    let text = prompt_and_description.to_lowercase();
    // Layer 3: dispatcher already routed around / through ecp.
    if text.contains("ecp") {
        return false;
    }
    // Layer 2: text-domain work.
    if negative_guard().is_match(&text) {
        return false;
    }
    // Layer 1: structural signal.
    structural_signals().is_match(&text)
}

pub fn handle(input: &HookInput) -> Result<(), EcpError> {
    let agent_type = input
        .tool_input
        .get("subagent_type")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let prompt = input
        .tool_input
        .get("prompt")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let description = input
        .tool_input
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let text = format!("{prompt} {description}");

    if should_deny(agent_type, &text) {
        let payload = serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": "Structural code query detected — query the code graph first instead of dispatching an agent: `ecp find <name>` (definition), `ecp impact --target <name> --direction upstream` (callers / blast radius), `ecp inspect --name <name>` (full context), `ecp cypher` (arbitrary graph questions). If ecp cannot answer (repo unindexed, non-code text, or you need file CONTENT rather than structure), re-dispatch this agent and say so in the prompt — mentioning ecp bypasses this gate."
            }
        });
        println!("{payload}");
    }
    // Silent exit 0 = pass-through (Claude Code treats empty stdout as no-op).
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::should_deny;

    /// Held-out adversarial suite carried over from the shell prototype —
    /// includes the paraphrase class a single-layer regex missed 80% of.
    /// Every case here ran against the production decision fn.
    #[test]
    fn structural_queries_are_denied() {
        for (agent, prompt) in [
            (
                "Explore",
                "Find out who calls the resolve_session_id function across the repo",
            ),
            (
                "general-purpose",
                "Map out the architecture of the session module and how components connect",
            ),
            (
                "Explore",
                "What is the blast radius of changing the DirtyEntry struct",
            ),
            (
                "Explore",
                "Trace the call flow from main to the overlay writer",
            ),
            ("Explore", "Find all implementations of the Provider trait"),
            (
                "Explore",
                "Where is OverlayView defined and what are its fields",
            ),
            (
                "Explore",
                "What would break if I change the signature of load_view_inputs",
            ),
            (
                "Explore",
                "Show me everywhere DirtyEntry is instantiated in the workspace",
            ),
            (
                "general-purpose",
                "List everything that depends on the overlay module",
            ),
            (
                "Explore",
                "Which functions receive the session_dir as a parameter",
            ),
            ("Explore", "Do an impact analysis on renaming with_overlay"),
            ("Explore", "Find the consumers of the OverlayView struct"),
        ] {
            assert!(should_deny(agent, prompt), "should deny: {prompt}");
        }
    }

    #[test]
    fn legitimate_dispatches_pass() {
        for (agent, prompt) in [
            // ecp-aware dispatches
            (
                "Explore",
                "Use ecp impact to verify callers, then read the top 3 files",
            ),
            // text-domain work
            (
                "Explore",
                "Find the string literal 'l1-overlay' usages and the config key for session dir",
            ),
            (
                "Explore",
                "Survey the README and docs site for outdated installation instructions",
            ),
            (
                "Explore",
                "Everywhere the word TODO appears in markdown docs, collect them",
            ),
            ("Explore", "Check which env vars the Dockerfile depends on"),
            (
                "Explore",
                "What would break the CI if the runner image changes - check workflow yaml files",
            ),
            (
                "Explore",
                "Check the filesystem layout under ~/.ecp for orphan directories",
            ),
            (
                "Explore",
                "Read the vendored tree-sitter grammar files and summarize licenses",
            ),
            // non-structural tasks
            (
                "general-purpose",
                "Refactor the parser module and run tests",
            ),
            ("general-purpose", "Run the test suite and report failures"),
            // specialized agents pass regardless of phrasing
            (
                "feature-dev:code-reviewer",
                "Review who calls this and whether error handling is correct",
            ),
            (
                "Plan",
                "Plan the blast radius mitigation for the schema change",
            ),
        ] {
            assert!(!should_deny(agent, prompt), "should pass: {prompt}");
        }
    }

    /// Empty agent type (defensive: malformed envelope) is gated — an
    /// unidentifiable dispatch with a structural prompt still benefits
    /// from the redirect, and the escape hatches all still apply.
    #[test]
    fn empty_agent_type_is_gated() {
        assert!(should_deny("", "who calls ensure_fresh"));
        assert!(!should_deny(
            "",
            "use ecp impact for callers of ensure_fresh"
        ));
    }
}
