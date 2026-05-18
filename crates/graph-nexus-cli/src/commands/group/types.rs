//! On-disk types for the per-group contract registry. rkyv-archived
//! for zero-copy reads via mmap.

use rkyv::{Archive, Deserialize, Serialize};

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(compare(PartialEq), derive(Debug))]
pub enum ContractType {
    Http,
    Grpc,
    Thrift,
    Topic,
    Lib,
    Custom,
    Include,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(compare(PartialEq), derive(Debug))]
pub enum ContractRole {
    Provider,
    Consumer,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(compare(PartialEq), derive(Debug))]
pub enum MatchType {
    Exact,
    Manifest,
    Wildcard,
    Bm25,
    Embedding,
}

impl std::fmt::Display for MatchType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            MatchType::Exact => "exact",
            MatchType::Manifest => "manifest",
            MatchType::Wildcard => "wildcard",
            MatchType::Bm25 => "bm25",
            MatchType::Embedding => "embedding",
        })
    }
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(compare(PartialEq), derive(Debug))]
pub struct SymbolRef {
    pub file_path: String,
    pub name: String,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(compare(PartialEq), derive(Debug))]
pub struct ExtractedContract {
    pub contract_id: String,
    pub contract_type: ContractType,
    pub role: ContractRole,
    pub symbol_uid: String,
    pub symbol_ref: SymbolRef,
    pub confidence: f32,
    pub service: Option<String>,
    pub meta: Vec<(String, String)>,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(compare(PartialEq), derive(Debug))]
pub struct StoredContract {
    pub repo: String,
    pub inner: ExtractedContract,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(compare(PartialEq), derive(Debug))]
pub struct CrossLinkEndpoint {
    pub repo: String,
    pub service: Option<String>,
    pub symbol_uid: String,
    pub symbol_ref: SymbolRef,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(compare(PartialEq), derive(Debug))]
pub struct CrossLink {
    pub from: CrossLinkEndpoint,
    pub to: CrossLinkEndpoint,
    pub contract_type: ContractType,
    pub contract_id: String,
    pub match_type: MatchType,
    pub confidence: f32,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(compare(PartialEq), derive(Debug))]
pub struct ContractRegistry {
    pub version: u32,
    pub contracts: Vec<StoredContract>,
    pub cross_links: Vec<CrossLink>,
    pub unmatched: Vec<StoredContract>,
}
