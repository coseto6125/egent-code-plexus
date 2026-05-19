//! Parse `git diff -U0` unified-diff output into per-file hunk ranges.
//!
//! Pure function (no I/O), unit-testable without git. Mirrors upstream
//! `parseDiffHunks` in `._source_code/.../storage/git.ts:339`.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffHunk {
    pub start_line: u32,
    pub end_line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub file_path: String,
    pub hunks: Vec<DiffHunk>,
}

/// Extract new-file line ranges from `@@ -x,y +start,count @@` headers.
/// Files appearing as `+++ /dev/null` (deletions) are silently skipped because
/// they cannot map to existing graph nodes.
pub fn parse_diff_hunks(diff_output: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut current: Option<FileDiff> = None;

    for line in diff_output.split('\n') {
        if let Some(rest) = line.strip_prefix("+++ b/") {
            if let Some(f) = current.take() {
                files.push(f);
            }
            current = Some(FileDiff {
                file_path: rest.to_string(),
                hunks: Vec::new(),
            });
        } else if line.starts_with("+++ /dev/null") {
            // deletion — drop any in-progress file
            current = None;
        } else if line.starts_with("@@") {
            if let Some(file) = current.as_mut() {
                if let Some(hunk) = parse_hunk_header(line) {
                    file.hunks.push(hunk);
                }
            }
        }
    }
    if let Some(f) = current.take() {
        files.push(f);
    }
    files
}

/// Parse `@@ -<old> +<new_start>[,<new_count>] @@` → DiffHunk.
/// Returns None if header malformed or count is 0 (pure-deletion hunk).
fn parse_hunk_header(line: &str) -> Option<DiffHunk> {
    // Find the "+" segment between the two "@@" markers.
    let after_dash = line.find('+')?;
    let segment_start = after_dash + 1;
    let segment_end = line[segment_start..].find(' ')?;
    let segment = &line[segment_start..segment_start + segment_end];

    let (start_s, count_s) = match segment.split_once(',') {
        Some((s, c)) => (s, c),
        None => (segment, "1"),
    };
    let start: u32 = start_s.parse().ok()?;
    let count: u32 = count_s.parse().ok()?;
    if count == 0 {
        return None;
    }
    Some(DiffHunk {
        start_line: start,
        end_line: start + count - 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_file_single_hunk() {
        let diff = "\
diff --git a/src/foo.rs b/src/foo.rs
index abc..def 100644
--- a/src/foo.rs
+++ b/src/foo.rs
@@ -10,3 +10,5 @@
 unchanged
-removed
+added one
+added two
";
        let r = parse_diff_hunks(diff);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].file_path, "src/foo.rs");
        assert_eq!(
            r[0].hunks,
            vec![DiffHunk {
                start_line: 10,
                end_line: 14
            }]
        );
    }

    #[test]
    fn multiple_files() {
        let diff = "\
+++ b/a.rs
@@ -1,1 +1,2 @@
+++ b/b.rs
@@ -5,0 +10,3 @@
";
        let r = parse_diff_hunks(diff);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].file_path, "a.rs");
        assert_eq!(r[1].file_path, "b.rs");
        assert_eq!(
            r[1].hunks[0],
            DiffHunk {
                start_line: 10,
                end_line: 12
            }
        );
    }

    #[test]
    fn deletion_to_dev_null_skipped() {
        let diff = "\
+++ /dev/null
@@ -1,5 +0,0 @@
+++ b/kept.rs
@@ -1,1 +1,2 @@
";
        let r = parse_diff_hunks(diff);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].file_path, "kept.rs");
    }

    #[test]
    fn pure_deletion_hunk_skipped() {
        // -U0 deletion-only hunk has count=0 on the + side
        let diff = "\
+++ b/x.rs
@@ -5,3 +5,0 @@
";
        let r = parse_diff_hunks(diff);
        assert_eq!(r.len(), 1);
        assert!(r[0].hunks.is_empty());
    }

    #[test]
    fn missing_count_defaults_to_one() {
        let diff = "\
+++ b/x.rs
@@ -1 +1 @@
";
        let r = parse_diff_hunks(diff);
        assert_eq!(
            r[0].hunks,
            vec![DiffHunk {
                start_line: 1,
                end_line: 1
            }]
        );
    }
}
