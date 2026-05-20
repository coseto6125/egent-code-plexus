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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitDirName {
    pub source_type: SourceType,
    pub source_id: Option<String>,
    pub sha: [u8; 20],
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
        let sha_str = sha_segment
            .split_once(".gen.")
            .map_or(sha_segment, |(sha, _generation)| sha);
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
        })
    }

    pub fn format(&self) -> String {
        let sha_hex = self.sha_hex();
        match (&self.source_type, &self.source_id) {
            (SourceType::Commit, _) => format!("commit__{sha_hex}"),
            (t, Some(id)) => format!("{}_{id}__{sha_hex}", t.as_str()),
            (t, None) => {
                debug_assert_eq!(*t, SourceType::Commit, "only Commit may have no source_id");
                format!("{}__{sha_hex}", t.as_str())
            }
        }
    }

    pub fn sha_hex(&self) -> String {
        hex::encode(self.sha)
    }
}
