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

    /// `file_given` / `kind_given` / `owner_form` / `line_form` say which
    /// narrowing the caller already applied or cannot use, so the message names
    /// only flags that can still separate the candidates.
    #[error("symbol name '{name}' is ambiguous ({count} candidates){}", ambiguity_tail(name, candidates.as_deref(), *file_given, *kind_given, *owner_form, *line_form))]
    AmbiguousSymbol {
        name: String,
        count: usize,
        candidates: Option<String>,
        file_given: bool,
        kind_given: bool,
        /// An `Owner.name` form would separate the candidates: some have an
        /// owner and the target did not name one.
        owner_form: bool,
        /// A `name@line` form would separate candidates that no flag and no
        /// owner tells apart: two overloads, or a redefinition, in one file.
        line_form: bool,
    },

    #[error("git diff failed: {reason}")]
    GitDiff { reason: String },

    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("serialization error: {0}")]
    Serialization(String),

    /// A command failure whose message already names its cause. Most sites are
    /// refusals, subprocess and load failures, not encoding; the output
    /// encoders prefix their own subject (`json serialize:`, `toon encode:`).
    /// A fixed prefix here would mislabel every other site.
    #[error("{0}")]
    Output(String),
}

fn ambiguity_tail(
    name: &str,
    candidates: Option<&str>,
    file_given: bool,
    kind_given: bool,
    owner_form: bool,
    line_form: bool,
) -> String {
    let mut options: Vec<&str> = Vec::new();
    if !file_given {
        options.push("--file");
    }
    if !kind_given {
        options.push("--kind");
    }
    let mut clauses: Vec<String> = Vec::new();
    if !options.is_empty() {
        clauses.push(format!("add {}", options.join(" or ")));
    }
    if owner_form {
        clauses.push("use the `Owner.name` form".into());
    }
    if line_form {
        clauses.push(format!("write the target as `{name}@<line>`"));
    }
    let mut advice = clauses.join(", or ");
    if advice.is_empty() {
        advice = match candidates {
            Some(_) => "the given filters already narrow it; tell the candidates apart by their file and line".into(),
            None => "the given filters already narrow it".into(),
        };
    }
    match candidates {
        Some(c) => format!(" — {advice}:\ncandidates[filePath,kind,line]:\n{c}"),
        None => format!(" — {advice}"),
    }
}

pub type EcpResult<T> = Result<T, EcpError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_output_display_adds_no_prefix() {
        let msg = EcpError::Output("refusing to index /: reason".into()).to_string();
        assert_eq!(msg, "refusing to index /: reason");
    }

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
            file_given: false,
            kind_given: false,
            owner_form: true,
            line_form: false,
        }
        .to_string();
        assert!(!msg.contains("--uid"), "{msg}");
        assert!(msg.contains("--file") && msg.contains("--kind"), "{msg}");
    }

    fn ambiguous(file_given: bool, kind_given: bool, owner_form: bool) -> String {
        ambiguous_lines(file_given, kind_given, owner_form, false)
    }

    fn ambiguous_lines(
        file_given: bool,
        kind_given: bool,
        owner_form: bool,
        line_form: bool,
    ) -> String {
        EcpError::AmbiguousSymbol {
            name: "load".into(),
            count: 2,
            candidates: Some("a.rs,Method,3\nb.rs,Method,9".into()),
            file_given,
            kind_given,
            owner_form,
            line_form,
        }
        .to_string()
    }

    /// Stands for: two overloads, or a redefinition, in one file differ only
    /// by line, so the message names the `name@line` form and no flag that
    /// cannot separate them.
    #[test]
    fn test_ambiguous_symbol_message_line_form_names_name_at_line() {
        let only_line = ambiguous_lines(true, true, false, true);
        assert!(
            only_line.contains("— write the target as `load@<line>`:")
                && !only_line.contains("add --"),
            "{only_line}"
        );
        let with_file = ambiguous_lines(false, true, false, true);
        assert!(
            with_file.contains("add --file, or write the target as `load@<line>`:"),
            "{with_file}"
        );
    }

    #[test]
    fn test_ambiguous_symbol_message_names_only_absent_narrowing_flags() {
        let kind_only_left = ambiguous(true, false, false);
        assert!(
            kind_only_left.contains("add --kind:") && !kind_only_left.contains("--file"),
            "{kind_only_left}"
        );
        let file_only_left = ambiguous(false, true, false);
        assert!(
            file_only_left.contains("add --file:") && !file_only_left.contains("--kind"),
            "{file_only_left}"
        );
        let with_owner = ambiguous(true, true, true);
        assert!(
            with_owner.contains("use the `Owner.name` form")
                && with_owner.contains("a.rs,Method,3"),
            "{with_owner}"
        );
        assert!(!with_owner.contains("add --"), "{with_owner}");
    }

    #[test]
    fn test_ambiguous_symbol_message_with_every_flag_given_points_at_candidates() {
        let msg = ambiguous(true, true, false);
        assert!(
            !msg.contains("add --file") && !msg.contains("add --kind"),
            "{msg}"
        );
        assert!(
            msg.contains("file and line") && msg.contains("b.rs,Method,9"),
            "{msg}"
        );
    }
}
