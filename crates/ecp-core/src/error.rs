use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EcpError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("graph deserialization failed: {0}")]
    Rkyv(String),

    #[error("graph.bin not found at {path:?} — run `ecp analyze` first")]
    GraphNotFound { path: PathBuf },

    #[error("symbol UID '{uid}' not found in graph")]
    SymbolNotFound { uid: String },

    #[error("symbol name '{name}' is ambiguous ({count} candidates){}", candidates.as_ref().map(|c| format!(" — add --file or --kind:\ncandidates[filePath,kind,line]:\n{c}")).unwrap_or_else(|| " — pass --uid".into()))]
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
