use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EcpError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("graph deserialization failed: {0}")]
    Rkyv(String),

    #[error("graph.bin not found at {path:?} — run `ecp admin index` first")]
    GraphNotFound { path: PathBuf },

    #[error("symbol UID '{uid}' not found in graph")]
    SymbolNotFound { uid: String },

    #[error("symbol name '{name}' is ambiguous ({count} candidates){}", candidates.as_ref().map(|c| format!(" — add --file or --kind:\ncandidates[filePath,kind,line]:\n{c}")).unwrap_or_else(|| " — add --file or --kind, or use the `Owner.name` form".into()))]
    AmbiguousSymbol {
        name: String,
        count: usize,
        candidates: Option<String>,
    },

    #[error("git diff failed: {reason}")]
    GitDiff { reason: String },

    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("output encode error: {0}")]
    Output(String),
}

pub type EcpResult<T> = Result<T, EcpError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_graph_not_found_names_admin_index() {
        let msg = EcpError::GraphNotFound {
            path: "g.bin".into(),
        }
        .to_string();
        assert!(
            msg.contains("ecp admin index") && !msg.contains("ecp analyze"),
            "{msg}"
        );
    }

    #[test]
    fn test_ambiguous_symbol_without_candidates_does_not_name_missing_uid_flag() {
        let msg = EcpError::AmbiguousSymbol {
            name: "load".into(),
            count: 2,
            candidates: None,
        }
        .to_string();
        assert!(!msg.contains("--uid"), "{msg}");
        assert!(msg.contains("--file") && msg.contains("--kind"), "{msg}");
    }
}
