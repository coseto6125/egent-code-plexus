use crate::registry::io::atomic_write_json;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub version: u32,
    pub session_id: String,
    pub pid: Option<u32>,
    pub started_at: String,
    pub last_touched: String,
    pub base_sha: String,
    pub source_worktree: String,
    pub overlay_version: u32,
    #[serde(default)]
    pub watcher_pid: Option<u32>,
    #[serde(default)]
    pub last_drained_offset: u64,
    /// Team-visible agent name (e.g. Claude agent-team teammate name).
    /// None for solo sessions; display falls back to session_id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
}

impl SessionMeta {
    pub fn write_atomic(path: &Path, value: &Self) -> io::Result<()> {
        atomic_write_json(path, value)
    }
    pub fn read(path: &Path) -> io::Result<Self> {
        let bytes = fs::read(path)?;
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_json() -> &'static str {
        r#"{"version":1,"session_id":"s1","pid":42,"started_at":"t","last_touched":"t","base_sha":"abc","source_worktree":"","overlay_version":0}"#
    }

    #[test]
    fn test_meta_parse_without_agent_name_defaults_none() {
        let m: SessionMeta = serde_json::from_str(base_json()).unwrap();
        assert_eq!(m.agent_name, None);
    }

    #[test]
    fn test_meta_roundtrip_with_agent_name() {
        let mut m: SessionMeta = serde_json::from_str(base_json()).unwrap();
        m.agent_name = Some("rust-parser".into());
        let s = serde_json::to_string(&m).unwrap();
        let back: SessionMeta = serde_json::from_str(&s).unwrap();
        assert_eq!(back.agent_name.as_deref(), Some("rust-parser"));
    }

    #[test]
    fn test_meta_serialize_without_agent_name_omits_field() {
        let m: SessionMeta = serde_json::from_str(base_json()).unwrap();
        let s = serde_json::to_string(&m).unwrap();
        assert!(
            !s.contains("agent_name"),
            "absent name must not serialize: {s}"
        );
    }
}
