//! Shared host-status enum used by every leaf handler in `gnx admin`.

/// Installation state for a tool host (MCP or Native).
///
/// MCP hosts report `Installed { detail }` where `detail` is one of
/// `"mode=spawn"` or `"mode=daemon"`.  Native hosts use `"fork: <tool> @ <path>"`.
/// Subproject leaves that haven't been implemented yet return `Missing`.
///
/// `Installed` and `Outdated` are populated by subprojects C/D/E; the
/// variants are part of the public contract even though stubs only produce
/// `Missing` today.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum HostStatus {
    /// Tool is installed and operational.
    Installed {
        /// Short descriptor, e.g. `"mode=spawn"` or `"fork: codex-cli @ /usr/local/bin"`.
        detail: String,
    },
    /// Tool was once installed but the version on disk is behind what gnx expects.
    Outdated {
        /// Human-readable explanation, e.g. `"config version 1 < required 2"`.
        reason: String,
    },
    /// Tool is not installed.
    Missing,
}

impl HostStatus {
    /// Print the status line in the canonical 3/4-state format.
    ///
    /// Output examples:
    /// ```text
    ///   Claude Code: installed (mode=spawn)
    ///   Codex CLI: outdated — config version 1 < required 2
    ///   Gemini CLI: missing
    /// ```
    pub fn print(&self, host_name: &str) {
        match self {
            HostStatus::Installed { detail } => {
                println!("  {host_name}: installed ({detail})");
            }
            HostStatus::Outdated { reason } => {
                println!("  {host_name}: outdated — {reason}");
            }
            HostStatus::Missing => {
                println!("  {host_name}: missing");
            }
        }
    }
}
