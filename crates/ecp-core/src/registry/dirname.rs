use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceType {
    Branch,
    Tag,
    Pr,
    Commit,
}

impl SourceType {
    fn as_str(self) -> &'static str {
        match self {
            SourceType::Branch => "branch",
            SourceType::Tag => "tag",
            SourceType::Pr => "pr",
            SourceType::Commit => "commit",
        }
    }
}

/// Parsed generation suffix from a commit dir name. The producer
/// (`orchestrator::publish_dir_for`) writes
/// `<base>.gen.<timestamp_ms>.<pid>.<counter>` — a deterministic 3-tuple total
/// order. Same-SHA tie-breakers MUST use this tuple, never raw filesystem
/// mtime, because the mtime resolution of ext4 / APFS can be coarser than the
/// publish cadence (see FU-2026-05-23-045).
///
/// `Generation` is ordered: `None < Some(_)`, so a base dir (no gen suffix)
/// always loses to a generation dir for the same SHA. Among generations,
/// `(timestamp_ms, pid, counter)` lex order wins — the producer guarantees a
/// fresh counter+timestamp per build.
///
/// `pid` and `counter` default to `0` when the suffix carries fewer than three
/// dot-separated numbers (older builders emitted just `.gen.<timestamp>`), so
/// pre-multi-process generations still compare correctly under lex order:
/// `(t, 0, 0) < (t, pid>0, counter)` for any same-timestamp newer build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Generation {
    pub timestamp_ms: u64,
    pub pid: u32,
    pub counter: u32,
}

impl Generation {
    /// `.gen.<timestamp_ms>.<pid>.<counter>` — the on-disk suffix shape.
    /// Single source of truth for the wire format; both
    /// [`CommitDirName::format`] and `orchestrator::publish_dir_for`
    /// concatenate this onto the base dir name so a future format change
    /// (e.g. a fourth field) touches one site, not three.
    pub fn format_suffix(&self) -> String {
        format!(".gen.{}.{}.{}", self.timestamp_ms, self.pid, self.counter)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitDirName {
    pub source_type: SourceType,
    pub source_id: Option<String>,
    pub sha: [u8; 20],
    /// `Some` iff the dir name carries a `.gen.<…>` suffix the producer can
    /// have written. `None` for base / first-publish dirs.
    pub generation: Option<Generation>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("dir name missing __ separator")]
    NoSha,
    #[error("sha segment not 40-hex")]
    InvalidSha,
    #[error("prefix missing source_type")]
    NoTypeId,
    #[error("unknown source_type: {0}")]
    UnknownSourceType(String),
}

impl CommitDirName {
    pub fn parse(name: &str) -> Result<Self, ParseError> {
        let (prefix, sha_segment) = name.rsplit_once("__").ok_or(ParseError::NoSha)?;
        let (sha_str, generation) = match sha_segment.split_once(".gen.") {
            Some((sha, gen_suffix)) => (sha, parse_generation_suffix(gen_suffix)),
            None => (sha_segment, None),
        };
        if sha_str.len() != 40 || !sha_str.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(ParseError::InvalidSha);
        }
        let mut sha = [0u8; 20];
        hex::decode_to_slice(sha_str, &mut sha).map_err(|_| ParseError::InvalidSha)?;

        if prefix == "commit" {
            return Ok(Self {
                source_type: SourceType::Commit,
                source_id: None,
                sha,
                generation,
            });
        }

        let (type_str, id_str) = prefix.split_once('_').ok_or(ParseError::NoTypeId)?;
        let source_type = match type_str {
            "branch" => SourceType::Branch,
            "tag" => SourceType::Tag,
            "pr" => SourceType::Pr,
            other => return Err(ParseError::UnknownSourceType(other.into())),
        };
        Ok(Self {
            source_type,
            source_id: Some(id_str.into()),
            sha,
            generation,
        })
    }

    pub fn format(&self) -> String {
        let sha_hex = self.sha_hex();
        let base = match (&self.source_type, &self.source_id) {
            (SourceType::Commit, _) => format!("commit__{sha_hex}"),
            (t, Some(id)) => format!("{}_{id}__{sha_hex}", t.as_str()),
            (t, None) => {
                debug_assert_eq!(*t, SourceType::Commit, "only Commit may have no source_id");
                format!("{}__{sha_hex}", t.as_str())
            }
        };
        match self.generation {
            None => base,
            Some(g) => format!("{base}{}", g.format_suffix()),
        }
    }

    pub fn sha_hex(&self) -> String {
        hex::encode(self.sha)
    }
}

/// Parse the part after `.gen.` into a `Generation`. Tolerant of historical
/// formats:
/// - `1234567890123.4567.42` → `(1234567890123, 4567, 42)` (current 3-tuple)
/// - `1234567890123.4567`    → `(1234567890123, 4567, 0)` (no counter — early prototype)
/// - `1234567890123`         → `(1234567890123, 0, 0)` (older single-int format)
/// - `not-a-number...`       → `None` (unrecognised; tie-breaker treats as base)
///
/// `None` is the safe fallback because the call site treats `None < Some(_)`,
/// so an unparseable suffix loses every same-SHA tie. Worst case: a freshly
/// published dir is shadowed by a stale base dir, which the next reindex
/// fixes. That's strictly better than promoting a corrupt suffix into the
/// total order.
fn parse_generation_suffix(suffix: &str) -> Option<Generation> {
    let mut parts = suffix.split('.');
    let timestamp_ms: u64 = parts.next()?.parse().ok()?;
    let pid: u32 = parts.next().map(|s| s.parse().ok()).unwrap_or(Some(0))?;
    let counter: u32 = parts.next().map(|s| s.parse().ok()).unwrap_or(Some(0))?;
    Some(Generation {
        timestamp_ms,
        pid,
        counter,
    })
}
