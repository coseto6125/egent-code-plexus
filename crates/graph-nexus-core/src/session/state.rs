//! Derived view of session liveness, classifying each `<repo>/sessions/<sid>/`
//! as PureReference (clean, can short-circuit overlay merge), Augmented (has
//! dirty fragments), or Stale (cannot serve queries). Not persisted to disk —
//! always re-derived from session_meta + dirty_files + commits/.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    PureReference {
        base_sha: String,
        l2_dirname: String,
    },
    AugmentedReference {
        base_sha: String,
        l2_dirname: String,
        fragment_count: usize,
    },
    Stale {
        reason: StaleReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleReason {
    MetaUnreadable,
    DirtyFilesCorrupt,
    L2Missing,
    Orphan,
}

impl StaleReason {
    /// Short text used by `admin sessions list` STATE column.
    pub fn short(&self) -> &'static str {
        match self {
            Self::MetaUnreadable => "meta",
            Self::DirtyFilesCorrupt => "dirty_corr",
            Self::L2Missing => "l2_missing",
            Self::Orphan => "orphan",
        }
    }
}
