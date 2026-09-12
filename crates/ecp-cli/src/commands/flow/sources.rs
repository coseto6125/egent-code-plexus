use crate::git::safe_exec;
use ecp_analyzer::flow::{Boundary, SourceFile};
use ecp_core::EcpError;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Component, Path};
use std::process::Stdio;

const MAX_FILES: usize = 2048;
const MAX_BYTES: usize = 32 * 1024 * 1024;

pub fn supported_path(path: &str) -> bool {
    ecp_analyzer::flow::supported_path(path)
}

fn included(path: &Path) -> bool {
    !path.components().any(|c| matches!(c, Component::Normal(s) if matches!(s.to_str(), Some(".git" | ".ecp" | ".claude" | "node_modules" | "target" | ".venv" | "vendor" | "__pycache__"))))
}

pub fn relative_path(repo: &Path, path: &Path) -> Result<String, EcpError> {
    let path = if path.is_absolute() {
        path.strip_prefix(repo)
            .map_err(|_| EcpError::InvalidArgument("source path must be inside --repo".into()))?
    } else {
        path
    };
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(s) => {
                parts.push(s.to_str().ok_or_else(|| {
                    EcpError::InvalidArgument("source paths must be UTF-8".into())
                })?)
            }
            Component::CurDir => (),
            _ => {
                return Err(EcpError::InvalidArgument(
                    "source paths must not contain parent traversal".into(),
                ))
            }
        }
    }
    if parts.is_empty() {
        return Err(EcpError::InvalidArgument("source path is empty".into()));
    }
    Ok(parts.join("/"))
}

fn budget(files: usize, bytes: usize) -> Result<(), EcpError> {
    if files > MAX_FILES || bytes > MAX_BYTES {
        return Err(EcpError::InvalidArgument(format!("flow source scope exceeds {MAX_FILES} files or {MAX_BYTES} bytes; pass a narrower --repo (no partial result was analyzed)")));
    }
    Ok(())
}

/// Sources admitted to analysis plus the ones the loader could not read.
/// A skipped file is reported as a boundary, never silently dropped: an
/// unreadable consumer must not read as an absent one.
#[derive(Debug, Default)]
pub struct Loaded {
    pub files: Vec<SourceFile>,
    pub skipped: Vec<Boundary>,
}

fn skipped(path: &str, message: String) -> Boundary {
    Boundary {
        file: path.to_owned(),
        line: 1,
        column: 1,
        kind: "unreadable_source".into(),
        message,
    }
}

fn read_source(path: &Path) -> Result<Result<String, String>, EcpError> {
    let bytes = std::fs::read(path)?;
    Ok(String::from_utf8(bytes).map_err(|e| format!("not valid UTF-8: {e}")))
}

pub fn load_sources(repo: &Path, overlay: Option<&Path>) -> Result<Loaded, EcpError> {
    let canonical_repo = dunce::canonicalize(repo)?;
    let overlay: BTreeMap<String, String> = match overlay {
        Some(path) => {
            let mut bytes = Vec::new();
            std::fs::File::open(path)?
                .take(MAX_BYTES as u64 + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() > MAX_BYTES {
                return Err(EcpError::InvalidArgument(format!(
                    "flow overlay JSON exceeds {MAX_BYTES} bytes"
                )));
            }
            serde_json::from_slice(&bytes)
                .map_err(|e| EcpError::InvalidArgument(format!("invalid source overlay: {e}")))?
        }
        None => BTreeMap::new(),
    };
    let mut sources = BTreeMap::new();
    let mut skipped_files = BTreeMap::new();
    let mut bytes = 0;
    let scope = repo.to_path_buf();
    let walker = ignore::WalkBuilder::new(repo)
        .hidden(false)
        .follow_links(false)
        .filter_entry(move |entry| {
            included(entry.path().strip_prefix(&scope).unwrap_or(entry.path()))
        })
        .build();
    for entry in walker {
        let entry = entry
            .map_err(|e| EcpError::InvalidArgument(format!("cannot read source scope: {e}")))?;
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = relative_path(repo, entry.path())?;
        if !supported_path(&path) {
            continue;
        }
        let size = entry
            .metadata()
            .map_err(|e| EcpError::InvalidArgument(e.to_string()))?
            .len();
        if size > MAX_BYTES as u64 {
            return Err(EcpError::InvalidArgument(format!(
                "source file exceeds flow byte budget: {path}"
            )));
        }
        let source = match read_source(entry.path())? {
            Ok(source) => source,
            Err(message) => {
                // Skipped sources count against the budget until replaced by an overlay.
                bytes += size as usize;
                skipped_files.insert(path.clone(), (skipped(&path, message), size as usize));
                budget(sources.len() + skipped_files.len(), bytes)?;
                continue;
            }
        };
        bytes += source.len();
        sources.insert(path, source);
        budget(sources.len() + skipped_files.len(), bytes)?;
    }
    // Ignore rules select untracked sources. Tracked sources remain admitted in
    // both snapshots, so a new ignore rule cannot fabricate a source deletion.
    for path in tracked_paths(repo)? {
        if sources.contains_key(&path)
            || skipped_files.contains_key(&path)
            || !supported_path(&path)
            || !included(Path::new(&path))
        {
            continue;
        }
        let absolute = repo.join(&path);
        let metadata = match std::fs::symlink_metadata(&absolute) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if !metadata.is_file() {
            continue;
        }
        if !dunce::canonicalize(&absolute)?.starts_with(&canonical_repo) {
            return Err(EcpError::InvalidArgument(format!(
                "tracked flow source resolves outside --repo: {path}"
            )));
        }
        if metadata.len() > MAX_BYTES as u64 {
            return Err(EcpError::InvalidArgument(format!(
                "source file exceeds flow byte budget: {path}"
            )));
        }
        let source = match read_source(&absolute)? {
            Ok(source) => source,
            Err(message) => {
                bytes += metadata.len() as usize;
                skipped_files.insert(
                    path.clone(),
                    (skipped(&path, message), metadata.len() as usize),
                );
                budget(sources.len() + skipped_files.len(), bytes)?;
                continue;
            }
        };
        bytes += source.len();
        sources.insert(path, source);
        budget(sources.len() + skipped_files.len(), bytes)?;
    }
    for (path, source) in overlay {
        let path = relative_path(repo, Path::new(&path))?;
        if !supported_path(&path) || !included(Path::new(&path)) {
            return Err(EcpError::InvalidArgument(format!(
                "unsupported or excluded overlay path: {path}"
            )));
        }
        bytes += source.len();
        if let Some((_, size)) = skipped_files.remove(&path) {
            bytes -= size;
        }
        if let Some(old) = sources.insert(path, source) {
            bytes -= old.len();
        }
        budget(sources.len() + skipped_files.len(), bytes)?;
    }
    Ok(Loaded {
        files: sources
            .into_iter()
            .map(|(path, source)| SourceFile { path, source })
            .collect(),
        skipped: skipped_files
            .into_values()
            .map(|(boundary, _)| boundary)
            .collect(),
    })
}

fn tracked_paths(repo: &Path) -> Result<Vec<String>, EcpError> {
    let probe = match safe_exec::git()
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(repo)
        .output()
    {
        Ok(probe) => probe,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    if !probe.status.success() || probe.stdout != b"true\n" {
        return Ok(Vec::new());
    }
    let tracked = safe_exec::git()
        .args(["ls-files", "--cached", "-z", "--"])
        .current_dir(repo)
        .output()?;
    if !tracked.status.success() {
        return Err(EcpError::InvalidArgument(format!(
            "cannot list tracked flow sources: {}",
            String::from_utf8_lossy(&tracked.stderr).trim()
        )));
    }
    tracked
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            std::str::from_utf8(path)
                .map(str::to_owned)
                .map_err(|error| EcpError::InvalidArgument(error.to_string()))
        })
        .collect()
}

/// Read a complete baseline snapshot. Object IDs avoid quoting ambiguities for
/// paths containing whitespace, colons, or newlines in the batch protocol.
pub fn load_sources_at_ref(repo: &Path, reference: &str) -> Result<Loaded, EcpError> {
    let revision = safe_exec::git()
        .args([
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{reference}^{{tree}}"),
        ])
        .current_dir(repo)
        .output()?;
    if !revision.status.success() {
        return Err(EcpError::InvalidArgument(format!(
            "invalid flow baseline: {reference}"
        )));
    }
    let tree = String::from_utf8_lossy(&revision.stdout);
    let listing = safe_exec::git()
        .args(["ls-tree", "-rz", tree.trim()])
        .current_dir(repo)
        .output()?;
    if !listing.status.success() {
        return Err(EcpError::InvalidArgument(
            "cannot list flow baseline tree".into(),
        ));
    }
    let mut entries = Vec::new();
    for record in listing
        .stdout
        .split(|byte| *byte == 0)
        .filter(|r| !r.is_empty())
    {
        let record =
            std::str::from_utf8(record).map_err(|e| EcpError::InvalidArgument(e.to_string()))?;
        let (header, path) = record
            .split_once('\t')
            .ok_or_else(|| EcpError::InvalidArgument("invalid git tree record".into()))?;
        let fields: Vec<_> = header.split_whitespace().collect();
        // Do not read symlink blobs as source text.
        if fields.len() != 3
            || fields[1] != "blob"
            || !matches!(fields[0], "100644" | "100755")
            || !supported_path(path)
            || !included(Path::new(path))
        {
            continue;
        }
        entries.push((path.to_owned(), fields[2].to_owned()));
    }
    entries.sort();
    budget(entries.len(), 0)?;
    if entries.is_empty() {
        return Ok(Loaded::default());
    }
    let mut child = safe_exec::git()
        .args(["cat-file", "--batch"])
        .current_dir(repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut input = child.stdin.take().expect("piped stdin");
    let ids = entries.iter().map(|(_, id)| id.clone()).collect::<Vec<_>>();
    let writer = std::thread::spawn(move || -> std::io::Result<()> {
        for id in ids {
            writeln!(input, "{id}")?;
        }
        Ok(())
    });
    let mut reader = BufReader::new(child.stdout.take().expect("piped stdout"));
    let result = (|| -> Result<Loaded, EcpError> {
        let mut sources = Loaded::default();
        let mut total = 0usize;
        for (path, id) in entries {
            let mut header = String::new();
            reader.read_line(&mut header)?;
            let fields: Vec<_> = header.split_whitespace().collect();
            if fields.len() != 3 || fields[0] != id || fields[1] != "blob" {
                return Err(EcpError::InvalidArgument("invalid git batch object".into()));
            }
            let size = fields[2]
                .parse::<usize>()
                .map_err(|e| EcpError::InvalidArgument(e.to_string()))?;
            total = total
                .checked_add(size)
                .ok_or_else(|| EcpError::InvalidArgument("baseline source size overflow".into()))?;
            budget(sources.files.len() + sources.skipped.len() + 1, total)?;
            let mut bytes = vec![0; size];
            reader.read_exact(&mut bytes)?;
            let mut newline = [0];
            reader.read_exact(&mut newline)?;
            if newline != *b"\n" {
                return Err(EcpError::InvalidArgument(
                    "invalid git batch terminator".into(),
                ));
            }
            match String::from_utf8(bytes) {
                Ok(source) => sources.files.push(SourceFile { path, source }),
                Err(e) => sources
                    .skipped
                    .push(skipped(&path, format!("not valid UTF-8: {e}"))),
            }
        }
        Ok(sources)
    })();
    if result.is_err() {
        let _ = child.kill();
    }
    drop(reader);
    let status = child.wait()?;
    let written = writer
        .join()
        .map_err(|_| EcpError::InvalidArgument("git batch writer failed".into()))?;
    let sources = result?;
    written?;
    if !status.success() {
        return Err(EcpError::InvalidArgument(
            "cannot read flow baseline objects".into(),
        ));
    }
    Ok(sources)
}
