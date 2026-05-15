//! Shared formatting helpers for archived graph enums. Previously each
//! command carried its own copy of `kind_to_str` / `rel_to_str`.

use graph_nexus_core::graph::{ArchivedNodeKind, ArchivedRelType};

pub fn kind_to_str(kind: &ArchivedNodeKind) -> &'static str {
    match kind {
        ArchivedNodeKind::File => "File",
        ArchivedNodeKind::Function => "Function",
        ArchivedNodeKind::Class => "Class",
        ArchivedNodeKind::Method => "Method",
        ArchivedNodeKind::Interface => "Interface",
        ArchivedNodeKind::Constructor => "Constructor",
        ArchivedNodeKind::Property => "Property",
        ArchivedNodeKind::Variable => "Variable",
        ArchivedNodeKind::Const => "Const",
        ArchivedNodeKind::Import => "Import",
        ArchivedNodeKind::Route => "Route",
        ArchivedNodeKind::Process => "Process",
        ArchivedNodeKind::Document => "Document",
        ArchivedNodeKind::Section => "Section",
        ArchivedNodeKind::EntryPoint => "EntryPoint",
    }
}

pub fn rel_to_str(rel: &ArchivedRelType) -> &'static str {
    match rel {
        ArchivedRelType::Defines => "defines",
        ArchivedRelType::Imports => "imports",
        ArchivedRelType::Calls => "calls",
        ArchivedRelType::Extends => "extends",
        ArchivedRelType::Implements => "implements",
        ArchivedRelType::HasMethod => "has_method",
        ArchivedRelType::HasProperty => "has_property",
        ArchivedRelType::Accesses => "accesses",
        ArchivedRelType::HandlesRoute => "handles_route",
        ArchivedRelType::StepInProcess => "step_in_process",
        ArchivedRelType::References => "references",
        ArchivedRelType::Fetches => "fetches",
    }
}
