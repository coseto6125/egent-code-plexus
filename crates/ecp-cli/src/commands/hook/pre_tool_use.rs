//! PreToolUse handler: the edit-flow evidence before an edit and the peer
//! inbox drain, merged into one `additionalContext` payload.
//!
//! This file also reads shell commands for the other hooks: the search call
//! of a Bash command (`search_call`) and the directory a leading `cd` moves
//! it to (`cd_target`). The PostToolUse search delta and the per-worktree
//! heartbeat use both.

use super::common::{emit_additional_context, HookInput};
use ecp_core::EcpError;
use std::borrow::Cow;
use std::path::{Component, Path, PathBuf};

pub fn handle(input: &HookInput) -> Result<(), EcpError> {
    // Both signals (edit flow + peer drain) must be merged into a single
    // additionalContext payload — Claude Code parses one JSON object on
    // stdout, so two separate println!s would drop the second silently.
    let mut sections: Vec<String> = Vec::new();
    if let Some(flow) = super::edit_flow::context(input, false) {
        sections.push(flow);
    }
    if let Some(peer) = super::common::drain_and_render_peer_payload() {
        sections.push(peer);
    }
    if !sections.is_empty() {
        emit_additional_context("PreToolUse", &sections.join("\n\n"));
    }
    Ok(())
}

/// The directories, besides the envelope cwd, whose worktree this tool call
/// works in: a leading `cd` target of a Bash command, or the folder of the file
/// an edit tool writes. Each has its own session dir, so each needs its own
/// heartbeat.
pub(super) fn other_worktree_dirs(input: &HookInput) -> Vec<PathBuf> {
    match input.tool_name.as_str() {
        "Bash" => match bash_command(&input.tool_input).map(|cmd| cd_target(cmd, &input.cwd)) {
            Some(CdTarget::Dir(dir)) => vec![PathBuf::from(dir)],
            _ => Vec::new(),
        },
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => ["file_path", "notebook_path"]
            .iter()
            .find_map(|key| input.tool_input.get(key).and_then(|v| v.as_str()))
            .and_then(|file| Path::new(file).parent())
            .filter(|dir| dir.is_absolute())
            .map(Path::to_path_buf)
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}

pub(super) fn bash_command(tool_input: &serde_json::Value) -> Option<&str> {
    tool_input.get("command").and_then(|v| v.as_str())
}

/// The control operator that ends a shell segment.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Sep {
    /// `;` or a newline.
    Seq,
    And,
    Or,
    Pipe,
    Background,
}

/// Split a shell command at unquoted `;`, `&&`, `||`, `|`, `&` and newlines,
/// each segment paired with the operator that ends it (`None` for the last).
/// A segment stops before an unquoted `#` that starts a word: the comment's
/// text runs to the newline and holds no operator, quote or group. A
/// `${...}` expansion with no brace, quote or escape inside is one opaque
/// run.
/// `None` when a quote or `${` is left open, since bash rejects the whole
/// command, and for a heredoc, whose body the split does not model. Every
/// separator is ASCII, so the byte offsets stay on char boundaries for CJK
/// paths.
fn shell_segments(cmd: &str) -> Option<Vec<(&str, Option<Sep>)>> {
    let mut segments = Vec::new();
    let mut start = 0;
    let mut quote: Option<char> = None;
    let mut chars = cmd.char_indices().peekable();
    let mut prev = ' ';
    let mut comment_at = None;
    while let Some((i, c)) = chars.next() {
        let after_redirect = matches!(prev, '>' | '<');
        // Bash's metacharacters, not Unicode whitespace: a `#` after a CR
        // is a literal.
        let word_start = METACHARS.contains(&prev);
        prev = c;
        let sep = match (quote, c) {
            (None, '$') if chars.next_if(|&(_, n)| n == '{').is_some() => {
                loop {
                    match chars.next()?.1 {
                        '}' => break,
                        // A brace, quote or escape inside the expansion may
                        // move its end; the split does not model that.
                        '{' | '\'' | '"' | '\\' | '`' => return None,
                        _ => {}
                    }
                }
                prev = '}';
                continue;
            }
            (None, '<') if cmd[i..].starts_with("<<<") => {
                chars.nth(1);
                continue;
            }
            (None, '<') if cmd[i..].starts_with("<<") => return None,
            (None, '#') if word_start => {
                comment_at = Some(i);
                while chars.next_if(|&(_, n)| n != '\n').is_some() {}
                continue;
            }
            (Some('\''), '\'') | (Some('"'), '"') => {
                quote = None;
                continue;
            }
            (Some('"'), '\\') | (None, '\\') => {
                chars.next();
                continue;
            }
            (Some(_), _) => continue,
            (None, '\'' | '"') => {
                quote = Some(c);
                continue;
            }
            // A newline after `&&`, `||` or `|` continues that list.
            (None, '\n')
                if matches!(
                    segments.last(),
                    Some((_, Some(Sep::And | Sep::Or | Sep::Pipe)))
                ) && cmd[start..comment_at.unwrap_or(i)].trim().is_empty() =>
            {
                start = i + 1;
                comment_at = None;
                continue;
            }
            (None, ';' | '\n') => Sep::Seq,
            // `2>&1`, `<&0`, `>|file` and `&>file` are redirects, not operators.
            (None, '&' | '|') if after_redirect => continue,
            (None, '&') if chars.peek().is_some_and(|&(_, n)| n == '>') => continue,
            // `|&` pipes stderr too.
            (None, '|') if chars.next_if(|&(_, n)| n == '&').is_some() => Sep::Pipe,
            (None, '&' | '|') => match (c, chars.next_if(|&(_, n)| n == c)) {
                ('&', Some(_)) => Sep::And,
                ('&', None) => Sep::Background,
                (_, Some(_)) => Sep::Or,
                (_, None) => Sep::Pipe,
            },
            _ => continue,
        };
        segments.push((&cmd[start..comment_at.take().unwrap_or(i)], Some(sep)));
        start = chars.peek().map_or(cmd.len(), |&(next, _)| next);
    }
    segments.push((&cmd[start..comment_at.unwrap_or(cmd.len())], None));
    quote.is_none().then_some(segments)
}

/// Directory a Bash search runs in, as far as a literal `cd <dir>` before
/// the `rg` / `grep` segment decides it: sessions working across repos keep
/// the envelope cwd on another repo, so resolving from it finds no graph.
#[derive(Debug, PartialEq)]
pub(super) enum CdTarget {
    /// No cd changes the search's shell: none at all, or one in a pipeline,
    /// a subshell or a background job.
    Unchanged,
    Dir(String),
    /// A cd may or may not run (`make || cd x`, `make && cd x; rg`), or its
    /// target cannot be known without expanding it (`cd -`, `$VAR`,
    /// backticks, `~user`, options, a bare `cd`), or a builtin other than cd
    /// moves the shell (`pushd`, `source`, ...), or a quote is left open.
    /// So does a cd in a `{ }` group or compound command that closes before
    /// the search, a cd in a loop, and a cd in the search's own subshell.
    Unknown,
}

const CWD_BUILTINS: [&str; 8] = [
    "pushd", "popd", "source", ".", "eval", "builtin", "command", "exec",
];

/// Git options that move the directory `git grep` searches.
const GIT_DIR_OPTIONS: [&str; 3] = ["-C", "--git-dir", "--work-tree"];

/// Environment variables that move the directory `git grep` searches.
const GIT_DIR_ENV: [&str; 2] = ["GIT_DIR=", "GIT_WORK_TREE="];

/// The characters after which a `#` starts a comment.
const METACHARS: [char; 10] = [' ', '\t', '\n', ';', '&', '|', '(', ')', '<', '>'];

/// Reserved words and `time` that run the command after them in this shell.
const PREFIX_WORDS: [&str; 9] = [
    "!", "time", "if", "then", "elif", "else", "do", "while", "until",
];

pub(super) fn cd_target(cmd: &str, cwd: &str) -> CdTarget {
    let cdpath = std::env::var_os("CDPATH").is_some_and(|v| !v.is_empty());
    cd_target_with_home(
        cmd,
        cwd,
        std::env::var_os("HOME").as_deref().map(Path::new),
        cdpath,
    )
}

/// `cdpath`: the shell inherits a non-empty `CDPATH`, which may redirect a
/// relative `cd` to another directory.
fn cd_target_with_home(cmd: &str, cwd: &str, home: Option<&Path>, cdpath: bool) -> CdTarget {
    // One substring scan per name keeps a command that moves no directory
    // off the segment parser.
    if !cmd.contains("cd")
        && !CWD_BUILTINS.iter().any(|b| cmd.contains(b))
        && !GIT_DIR_OPTIONS
            .iter()
            .chain(&GIT_DIR_ENV)
            .any(|o| cmd.contains(o))
    {
        return CdTarget::Unchanged;
    }
    let joined = join_continuations(cmd);
    let cmd = joined.as_ref();
    let Some(segments) = shell_segments(cmd) else {
        return CdTarget::Unknown;
    };
    let cdpath = cdpath || cmd.contains("CDPATH");
    let words: Vec<Vec<String>> = segments.iter().map(|&(s, _)| shell_words(s)).collect();
    // `search_call` takes the first search word that `runs_search`
    // accepts, so the scan stops at the same word.
    let search = words
        .iter()
        .position(|w| (0..w.len()).any(|j| runs_search(w, j)));
    // `git -C dir grep` and `GIT_WORK_TREE=dir git grep` search `dir`, which
    // the cd walk does not track.
    let git_moves_grep = |k: usize| {
        let segment = &words[k];
        let Some(j) = (0..segment.len()).find(|&j| runs_search(segment, j)) else {
            return false;
        };
        segment[..j]
            .iter()
            .rposition(|w| w == "git")
            .is_some_and(|g| {
                segment[g + 1..j]
                    .iter()
                    .any(|w| GIT_DIR_OPTIONS.iter().any(|o| w.starts_with(o)))
                    || words[..=k]
                        .iter()
                        .flatten()
                        .any(|w| GIT_DIR_ENV.iter().any(|e| w.starts_with(e)))
            })
    };
    if search.is_some_and(git_moves_grep) {
        return CdTarget::Unknown;
    }
    let marks: Vec<_> = segments.iter().map(|&(s, _)| group_marks(s)).collect();
    let commands: Vec<(Vec<&str>, usize)> = words.iter().map(|w| command_words(w)).collect();
    let compounds: Vec<(Vec<bool>, usize)> = commands
        .iter()
        .map(|(cmd_words, after_prefix)| compound_marks(cmd_words, *after_prefix))
        .collect();
    let depth_after: Vec<usize> = marks
        .iter()
        .zip(&compounds)
        .scan(0usize, |depth, (&((po, pc), (bo, bc)), (opens, closes))| {
            *depth = (*depth + po + bo + opens.len()).saturating_sub(pc + bc + closes);
            Some(*depth)
        })
        .collect();
    let moves = |w: &&str| *w == "cd" || CWD_BUILTINS.contains(w);
    // A search in a function body runs wherever the function is called,
    // which the walk does not follow (`f() { rg …; }; cd x && f`).
    let defines_function = |(c, _): &(Vec<&str>, usize)| match c[..] {
        ["function", ..] | [_, "()", ..] => true,
        [first, ..] => first.len() > 2 && first.ends_with("()"),
        [] => false,
    };
    if search.is_some_and(|k| commands[..=k].iter().any(defines_function)) {
        return CdTarget::Unknown;
    }
    // A `case` pattern's `)` reads as a subshell close, so the depth below
    // is wrong inside one.
    let has_case = commands.iter().any(|(c, p)| c.get(*p) == Some(&"case"));
    if has_case
        && search.is_some_and(|k| {
            commands[..k]
                .iter()
                .any(|(c, _)| c.iter().any(|w| moves(&w.trim_start_matches('('))))
        })
    {
        return CdTarget::Unknown;
    }
    // An and-or list ending in `&` runs as a background job: a cd reaches the
    // search only when every list around the cd that ends before the search
    // ends in `;`. A `;` or `&` deeper than the cd (`cd x && (a & wait)`)
    // ends no list around it.
    let other_job = |i: usize| {
        let mut level = Some(depth_after[i]);
        for (j, &(_, sep)) in segments
            .iter()
            .enumerate()
            .take(search.unwrap_or(0))
            .skip(i)
        {
            let Some(l) = level else { break };
            if matches!(sep, None | Some(Sep::Seq | Sep::Background)) && depth_after[j] <= l {
                if sep == Some(Sep::Background) {
                    return true;
                }
                level = depth_after[j].checked_sub(1);
            }
        }
        false
    };
    // A later `.` alone is mostly a path argument (`git add .`).
    let moves_shell = |cmd_words: &[&str]| {
        cmd_words
            .first()
            .is_some_and(|w| moves(&w.trim_start_matches('(')))
            || cmd_words.iter().skip(1).any(|w| *w != "." && moves(w))
    };
    // `x=$(cd y && rg …)` opens a subshell on the word that holds the cd.
    let opens_moving_subshell = |words: &[String]| {
        words
            .iter()
            .any(|w| w.rsplit_once('(').is_some_and(|(_, c)| moves(&c)))
    };
    // A cd before `else` / `elif` belongs to the branch that is not taken.
    let switches_branch = |words: &[String]| {
        words
            .iter()
            .take_while(|w| PREFIX_WORDS.contains(&w.as_str()))
            .any(|w| w == "else" || w == "elif")
    };
    // A loop around the search runs a later cd before its next pass.
    let moves_after_search =
        search.is_some_and(|k| words[k + 1..].iter().flatten().any(|w| moves(&w.as_str())));
    let mut current: Option<PathBuf> = None;
    let mut before = Some(Sep::Seq);
    let mut subshell = 0usize;
    // A cd inside a subshell that is still open moves a search in it.
    let mut subshell_moves = false;
    // Open `{ }` groups and compound commands, `true` for a loop or a `case`.
    // A group may be a function body, a skipped operand or a pipeline's
    // subshell, so a cd inside one holds only until it closes; a loop runs
    // its cd again, and a `case` arm may not be the search's arm.
    let mut groups: Vec<bool> = Vec::new();
    let mut cd_depth = 0;
    // After `cd x ||` succeeds, its `||` operand does not run. The operand
    // ends at the next `;`, `&&` or `&` outside any group or compound it opens
    // (`a || b && c` is `(a || b) && c`).
    let mut skipping: Option<usize> = None;
    for (i, (&(segment, after), words)) in segments.iter().zip(&words).enumerate() {
        if Some(i) == search {
            if subshell > 0 && subshell_moves
                // `until grep -q x f; do cd ..; done` searches in its condition.
                || (groups.contains(&true) || compounds[i].0.contains(&true)) && moves_after_search
                || cd_depth > 0 && cd_depth >= groups.len() && switches_branch(words)
            {
                return CdTarget::Unknown;
            }
            break;
        }
        let ((paren_opens, paren_closes), (brace_opens, brace_closes)) = marks[i];
        if let Some(depth) = skipping.as_mut() {
            let (compound_opens, compound_closes) = &compounds[i];
            *depth = (*depth + paren_opens + brace_opens + compound_opens.len())
                .saturating_sub(paren_closes + brace_closes + compound_closes);
            if *depth == 0 && !matches!(after, Some(Sep::Or | Sep::Pipe)) {
                skipping = None;
            }
            before = after;
            continue;
        }
        let (cmd_words, after_prefix) = (&commands[i].0, commands[i].1);
        let starts_subshell = cmd_words.first().is_some_and(|w| w.starts_with('('));
        // `x=$( cd y && pwd )` runs the cd in the substitution.
        let opens_in_assignment = words
            .iter()
            .take_while(|w| is_assignment(w))
            .any(|w| w.contains('('));
        let in_subshell = subshell > 0 || starts_subshell || opens_in_assignment;
        // Backticks hold a command the split does not see.
        if words
            .iter()
            .any(|w| w.rsplit_once('`').is_some_and(|(_, c)| moves(&c)))
        {
            return CdTarget::Unknown;
        }
        subshell_moves |= in_subshell && moves_shell(cmd_words) || opens_moving_subshell(words);
        subshell = (subshell + paren_opens).saturating_sub(paren_closes);
        if subshell == 0 {
            subshell_moves = false;
        }
        // `cat | { cd x; rg; }`: the pipe runs the group, and the cd starts
        // the group's own list.
        let opens_group = words
            .iter()
            .find(|w| !is_assignment(w))
            .is_some_and(|w| w == "{");
        let before_cmd = if opens_group { Some(Sep::Seq) } else { before };
        let own_shell = !in_subshell
            && !other_job(i)
            && before_cmd != Some(Sep::Pipe)
            && after != Some(Sep::Pipe);
        let (compound_opens, compound_closes) = &compounds[i];
        groups.truncate(groups.len().saturating_sub(brace_closes + compound_closes));
        if groups.len() < cd_depth
            || cd_depth > 0 && cd_depth >= groups.len() && switches_branch(words)
        {
            return CdTarget::Unknown;
        }
        groups.extend(compound_opens);
        groups.extend(std::iter::repeat_n(false, brace_opens));
        if own_shell
            && (after_prefix > 0 && cmd_words.get(after_prefix).is_some_and(moves)
                || cmd_words.iter().skip(1).any(|w| *w != "." && moves(w)))
        {
            return CdTarget::Unknown;
        }
        match cmd_words.first().copied() {
            // `command -v rg` looks a name up and runs nothing.
            Some("command") if matches!(cmd_words.get(1), Some(&("-v" | "-V"))) => {}
            Some(builtin) if own_shell && CWD_BUILTINS.contains(&builtin) => {
                return CdTarget::Unknown
            }
            // `set -P` makes a later `cd ..` physical.
            Some("set")
                if own_shell
                    && cmd_words[1..]
                        .iter()
                        .any(|w| w.starts_with('-') && w.contains('P') || *w == "physical") =>
            {
                return CdTarget::Unknown
            }
            Some("cd") if own_shell => {
                // `make && cd x && rg` searches only where the cd ran, but
                // `false && cd x && true; rg` searches the old directory.
                let surely_runs = match before_cmd {
                    Some(Sep::Seq | Sep::Background) | None => true,
                    Some(Sep::And) => search.is_some_and(|k| {
                        segments[i..k]
                            .iter()
                            .all(|&(_, s)| matches!(s, Some(Sep::And | Sep::Pipe)))
                    }),
                    _ => false,
                };
                let [_, arg] = cmd_words[..] else {
                    return CdTarget::Unknown;
                };
                if !surely_runs || groups.contains(&true) {
                    return CdTarget::Unknown;
                }
                let tilde = unquoted_tilde_arg(segment);
                // Bash searches `CDPATH` for a relative directory whose first
                // component is not `.` or `..` (`.codex` is searched).
                let dot_relative =
                    matches!(arg, "." | "..") || arg.starts_with("./") || arg.starts_with("../");
                if cdpath
                    && !arg.starts_with('/')
                    && !dot_relative
                    && !(tilde && arg.starts_with('~'))
                {
                    return CdTarget::Unknown;
                }
                let base = current.as_deref().unwrap_or(Path::new(cwd));
                match resolve_cd_arg(arg, base, home, tilde) {
                    None => return CdTarget::Unknown,
                    Some(Some(dir)) => {
                        current = Some(dir);
                        cd_depth = groups.len();
                        if after == Some(Sep::Or) {
                            skipping = Some(0);
                        }
                    }
                    // A directory missing now may be made before the cd
                    // runs (`mkdir x && cd x`, `git worktree add x`); if it
                    // is not, `cd x || …` runs an operand the hook does not
                    // model.
                    Some(None) => return CdTarget::Unknown,
                }
            }
            _ => {}
        }
        before = after;
    }
    match current {
        None => CdTarget::Unchanged,
        // `dunce` drops the `\\?\` prefix wherever the plain form names the
        // same path; a long or UNC path keeps it.
        Some(dir) => dunce::canonicalize(dir)
            .ok()
            .and_then(|dir| dir.into_os_string().into_string().ok())
            .map_or(CdTarget::Unknown, CdTarget::Dir),
    }
}

/// A segment's words after its assignments and an opening `{`, with the
/// count of leading prefix words (`!`, `then`, `time -p`). A prefix the
/// split does not model may still run the command in this shell
/// (`! cd x`, `time . env.sh`, `then cd x`).
fn command_words(words: &[String]) -> (Vec<&str>, usize) {
    let cmd_words: Vec<&str> = words
        .iter()
        .map(String::as_str)
        .skip_while(|w| is_assignment(w))
        .skip_while(|w| *w == "{")
        .collect();
    let mut after_prefix = 0;
    // `time -p` takes options before its command word.
    let mut time_options = false;
    for &word in &cmd_words {
        if PREFIX_WORDS.contains(&word) {
            time_options = word == "time";
        } else if !(time_options && word.starts_with('-')) {
            break;
        }
        after_prefix += 1;
    }
    (cmd_words, after_prefix)
}

/// The compound commands a segment opens, `true` for a loop or a `case`,
/// and how many it closes (`fi`, `done`, `esac`).
fn compound_marks(cmd_words: &[&str], after_prefix: usize) -> (Vec<bool>, usize) {
    let mut opens: Vec<bool> = cmd_words[..after_prefix]
        .iter()
        .filter(|w| matches!(**w, "if" | "while" | "until"))
        .map(|w| *w != "if")
        .collect();
    if let Some(&("for" | "select" | "case")) = cmd_words.get(after_prefix) {
        opens.push(true);
    }
    let closes = usize::from(matches!(cmd_words.first(), Some(&("fi" | "done" | "esac"))));
    (opens, closes)
}

/// `cmd` with every backslash-newline outside single quotes removed, as
/// bash joins a continued line before it splits words.
fn join_continuations(cmd: &str) -> Cow<'_, str> {
    if !cmd.contains("\\\n") {
        return Cow::Borrowed(cmd);
    }
    let mut out = String::with_capacity(cmd.len());
    let mut chars = cmd.chars().peekable();
    let mut quote = None;
    while let Some(c) = chars.next() {
        let word_start = out.is_empty() || out.ends_with(METACHARS);
        match (quote, c) {
            (Some('\''), '\'') | (Some('"'), '"') => quote = None,
            (None, '\'' | '"') => quote = Some(c),
            // A comment's trailing backslash continues nothing.
            (None, '#') if word_start => {
                out.push(c);
                while let Some(next) = chars.next_if(|&n| n != '\n') {
                    out.push(next);
                }
                continue;
            }
            (None | Some('"'), '\\') => {
                if chars.next_if_eq(&'\n').is_some() {
                    continue;
                }
                out.push(c);
                if let Some(next) = chars.next() {
                    out.push(next);
                }
                continue;
            }
            _ => {}
        }
        out.push(c);
    }
    Cow::Owned(out)
}

/// Unquoted `(` / `)` and word `{` / `}` in one segment, as
/// `((paren opens, paren closes), (brace opens, brace closes))`: a quoted
/// `'{'` opens no group.
fn group_marks(segment: &str) -> ((usize, usize), (usize, usize)) {
    let bytes = segment.as_bytes();
    let (mut parens, mut braces) = ((0, 0), (0, 0));
    let mut quote = None;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match (quote, c) {
            (Some(b'\''), b'\'') | (Some(b'"'), b'"') => quote = None,
            (Some(b'"') | None, b'\\') => i += 1,
            (Some(_), _) => {}
            (None, b'\'' | b'"') => quote = Some(c),
            (None, b'(') => parens.0 += 1,
            (None, b')') => parens.1 += 1,
            (None, b'{' | b'}') => {
                // A metacharacter ends the word too: `{ a; }>log` closes.
                let ends_word = |n: &u8| {
                    n.is_ascii_whitespace()
                        || c == b'}' && matches!(n, b'>' | b'<' | b'&' | b'|' | b';' | b')')
                };
                let word = (i == 0 || bytes[i - 1].is_ascii_whitespace())
                    && bytes.get(i + 1).is_none_or(ends_word);
                match (word, c) {
                    (true, b'{') => braces.0 += 1,
                    (true, _) => braces.1 += 1,
                    _ => {}
                }
            }
            _ => {}
        }
        i += 1;
    }
    (parens, braces)
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        let mut chars = name.chars();
        chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// Whether the raw `cd` argument starts with an unquoted `~`: `'~/x'` and
/// `"~"` name a directory called `~`.
fn unquoted_tilde_arg(segment: &str) -> bool {
    let mut raw = segment
        .split_whitespace()
        .skip_while(|w| is_assignment(w) || *w == "{");
    if raw.next() != Some("cd") {
        return false;
    }
    // `cd 2>/dev/null ~/x`: a redirection before the argument is no argument.
    while let Some(word) = raw.next() {
        let op = word.trim_start_matches(|c: char| c.is_ascii_digit() || c == '&');
        if !op.starts_with(['<', '>']) {
            return word.starts_with('~');
        }
        // `2> log` puts the target in the next word.
        if op.trim_start_matches(['<', '>', '&', '|']).is_empty() {
            raw.next();
        }
    }
    false
}

/// `None` when the target cannot be known without expanding it, `Some(None)`
/// when it names no directory (the cd fails), `Some(Some(dir))` otherwise.
fn resolve_cd_arg(
    arg: &str,
    base: &Path,
    home: Option<&Path>,
    tilde: bool,
) -> Option<Option<PathBuf>> {
    // A glob expands to a directory the hook does not list.
    if arg.is_empty() || arg.starts_with('-') || arg.contains(['$', '`', '*', '?', '[']) {
        return None;
    }
    let path = match arg.strip_prefix('~').filter(|_| tilde) {
        Some("") => home?.to_path_buf(),
        Some(rest) if rest.starts_with('/') => home?.join(rest.trim_start_matches('/')),
        Some(_) => return None,
        None if Path::new(arg).is_absolute() => PathBuf::from(arg),
        None if base.is_absolute() => base.join(arg),
        None => return None,
    };
    // Bash's default `cd -L` drops `..` against the logical path, so
    // `cd /bin; cd ..` lands in `/` even where `/bin` links to `/usr/bin`.
    // Where the logical and the physical walk disagree, bash may fall back
    // from one to the other: unknown.
    let mut logical = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                logical.pop();
            }
            Component::CurDir => {}
            part => logical.push(part),
        }
    }
    match (path.is_dir(), logical.is_dir()) {
        (true, true) => Some(Some(logical)),
        (false, false) => Some(None),
        _ => None,
    }
}

/// Split a shell command into words the way the shell hands them to the
/// program: quotes group, and are removed; a backslash outside single quotes
/// escapes the next character. A quoted pattern such as
/// `grep "pub struct HookInput"` is one word, not three. An unquoted
/// redirection (`2>&1`, `&>log`, `>/dev/null`, `< in`) is dropped with its
/// target, as the command never sees it: `cd x>/dev/null` names `x`.
fn shell_words(cmd: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut drop_next = false;
    let mut chars = cmd.chars().peekable();
    let mut finish = |current: &mut String, drop_next: &mut bool| {
        let word = std::mem::take(current);
        if !std::mem::take(drop_next) {
            words.push(word);
        }
    };
    while let Some(c) = chars.next() {
        match c {
            // `<(cmd)` / `>(cmd)` is a process substitution, not a redirect.
            '<' | '>' if chars.peek() != Some(&'(') => {
                // `2>` and `&>` name the redirected stream.
                let stream = current.chars().all(|d| d.is_ascii_digit()) || current == "&";
                if in_word && stream {
                    current.clear();
                } else if in_word {
                    finish(&mut current, &mut drop_next);
                }
                in_word = false;
                while chars
                    .next_if(|&n| matches!(n, '<' | '>' | '&' | '|'))
                    .is_some()
                {}
                drop_next = true;
            }
            '\'' => {
                in_word = true;
                current.extend(chars.by_ref().take_while(|&q| q != '\''));
            }
            '"' => {
                in_word = true;
                while let Some(q) = chars.next() {
                    match q {
                        '"' => break,
                        // Inside double quotes a backslash escapes only `$`,
                        // `` ` ``, `"`, `\\` and newline; before anything else
                        // it stays literal, which is what keeps a regex like
                        // `"\bfetch\s*\("` intact.
                        '\\' => match chars.next() {
                            Some('\n') => {}
                            Some(escaped @ ('$' | '`' | '"' | '\\')) => current.push(escaped),
                            Some(other) => {
                                current.push('\\');
                                current.push(other);
                            }
                            None => current.push('\\'),
                        },
                        _ => current.push(q),
                    }
                }
            }
            '\\' => {
                in_word = true;
                current.extend(chars.next());
            }
            c if c.is_whitespace() => {
                if in_word {
                    finish(&mut current, &mut drop_next);
                    in_word = false;
                }
            }
            _ => {
                in_word = true;
                current.push(c);
            }
        }
    }
    if in_word {
        finish(&mut current, &mut drop_next);
    }
    words
}

/// Whether `words[i]` runs `rg` / `grep` / `egrep` (`git grep` included),
/// rather than naming it to a lookup (`command -v rg`, `which grep`, `type -p
/// rg`), listing files (`rg --files | xargs grep …`) or printing its version
/// or help.
fn runs_search(words: &[String], i: usize) -> bool {
    if !matches!(words[i].as_str(), "rg" | "grep" | "egrep") {
        return false;
    }
    // `shell_words` keeps a separator glued to its word (`grep -V;`).
    for word in &words[i + 1..] {
        let bare = word.trim_end_matches(['|', '&', ';']);
        match bare {
            "--version" | "-V" | "--help" => return false,
            "--files" | "--type-list" if words[i] == "rg" => return false,
            _ => {}
        }
        if bare.len() < word.len() {
            break;
        }
    }
    let flags = words[..i]
        .iter()
        .rev()
        .take_while(|w| w.starts_with('-'))
        .count();
    // `cd grep` names a directory; `command -v rg grep` names two tools.
    let names = |w: Option<&str>| {
        matches!(
            w,
            Some("which" | "type" | "hash" | "whereis" | "cd" | "pushd")
        )
    };
    let mut head = words[..i]
        .iter()
        .map(String::as_str)
        .skip_while(|w| is_assignment(w) || *w == "{" || PREFIX_WORDS.contains(w));
    let looks_up = match head.next() {
        Some("command") => head
            .take_while(|w| w.starts_with('-'))
            .any(|w| w.contains(['v', 'V'])),
        first => names(first),
    };
    !looks_up
        && match words[..i - flags].last().map(String::as_str) {
            Some("command") => flags == 0,
            last => !names(last),
        }
}

/// The flags of `rg` / `grep` whose value is the next word.
const SEARCH_FLAGS_WITH_VALUES: [&str; 22] = [
    "-f",
    "-m",
    "-A",
    "-B",
    "-C",
    "-g",
    "--glob",
    "-t",
    "--type",
    "--include",
    "--exclude",
    "--color",
    "--colour",
    "--max-count",
    "--context",
    "--after-context",
    "--before-context",
    "-j",
    "--threads",
    "--max-depth",
    "-T",
    "--type-not",
];

/// The first search call of a shell command: `rg`, `grep`, `egrep` or
/// `git grep`.
#[derive(Debug, PartialEq)]
pub(super) struct SearchCall {
    /// The word `-e` / `--regexp` names, else the first non-flag word of
    /// three or more characters.
    pub pattern: String,
    /// The non-flag words after the pattern: the files and directories the
    /// search reads. Empty when it reads its working directory or stdin.
    pub operands: Vec<String>,
    /// A pipe feeds the search and it names no operand, so it searches the
    /// pipe, not files (`git log | grep x`). `xargs grep` and `git grep` read
    /// files.
    pub reads_stdin: bool,
}

/// `None` when no word runs a search, or every word after it is a flag, a
/// flag value or too short.
pub(super) fn search_call(cmd: &str) -> Option<SearchCall> {
    let joined = join_continuations(cmd);
    let cmd = joined.as_ref();
    // `cd_target_with_home` reads the same segment, so the pattern and the
    // directory belong to one search.
    let segments = shell_segments(cmd)?;
    let (k, words) = segments
        .iter()
        .enumerate()
        .map(|(k, &(segment, _))| (k, shell_words(segment)))
        .find(|(_, w)| (0..w.len()).any(|i| runs_search(w, i)))?;
    let start = (0..words.len()).find(|&i| runs_search(&words, i))?;
    let mut rest = words[start + 1..].iter();
    // `-e PATTERN` names the pattern explicitly for both grep and rg; the
    // word after it is the answer, not a flag value to step over.
    let mut next_is_pattern = false;
    let pattern = loop {
        let word = rest.next()?;
        // `--regexp=PATTERN` and `-ePATTERN` carry the pattern in the word.
        let glued = word
            .strip_prefix("--regexp=")
            .or_else(|| word.strip_prefix("-e").filter(|p| !p.is_empty()));
        if let Some(pattern) = glued.filter(|_| !next_is_pattern) {
            if pattern.len() >= 3 {
                break pattern.to_string();
            }
            continue;
        }
        if word.starts_with('-') && !next_is_pattern {
            if word == "-e" || word == "--regexp" {
                next_is_pattern = true;
            } else if SEARCH_FLAGS_WITH_VALUES.contains(&word.as_str()) {
                rest.next();
            }
            continue;
        }
        if word.len() >= 3 {
            break word.clone();
        }
        next_is_pattern = false;
    };
    let mut operands = Vec::new();
    let mut only_operands = false;
    while let Some(word) = rest.next() {
        if only_operands || !word.starts_with('-') {
            operands.push(word.clone());
        } else if word == "--" {
            only_operands = true;
        } else if word == "-e"
            || word == "--regexp"
            || SEARCH_FLAGS_WITH_VALUES.contains(&word.as_str())
        {
            rest.next();
        }
    }
    let piped = k > 0 && segments[k - 1].1 == Some(Sep::Pipe);
    let reads_files = words[..start].iter().any(|w| w == "xargs" || w == "git");
    Some(SearchCall {
        pattern,
        reads_stdin: piped && operands.is_empty() && !reads_files,
        operands,
    })
}

#[cfg(test)]
mod tests {
    use super::CdTarget::{Dir, Unchanged, Unknown};
    use super::{cd_target_with_home, search_call, shell_segments, CdTarget, SearchCall, Sep};
    use std::path::Path;

    /// The pattern `search_call` takes from `cmd`.
    fn pattern_of(cmd: &str) -> Option<String> {
        search_call(cmd).map(|call| call.pattern)
    }

    fn canon(p: &Path) -> String {
        dunce::canonicalize(p)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string()
    }

    fn target(cmd: &str, cwd: &Path) -> CdTarget {
        cd_target_with_home(cmd, cwd.to_str().unwrap(), None, false)
    }

    fn dir(p: &Path) -> CdTarget {
        Dir(canon(p))
    }

    #[test]
    fn test_cd_target_no_cd_returns_unchanged() {
        let t = tempfile::tempdir().unwrap();
        for cmd in ["grep -rn Foo app", "", "   "] {
            assert_eq!(target(cmd, t.path()), Unchanged, "{cmd:?}");
        }
    }

    #[test]
    fn test_cd_target_quoted_cjk_path_with_spaces_resolves() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("業主 原始碼").join("outsourcing-backend-CN");
        std::fs::create_dir_all(&path).unwrap();
        let d = path.to_str().unwrap();
        let other = tempfile::tempdir().unwrap();
        let mut cmds = vec![
            format!("cd \"{d}\" && grep -rn CacheKeyQueue app"),
            format!("cd '{d}' && grep -rn CacheKeyQueue app"),
        ];
        // Unquoted, a Windows path's `\` separators are escapes to bash too.
        if cfg!(unix) {
            cmds.push(format!("cd {} && rg Foo", d.replace(' ', "\\ ")));
        }
        for cmd in cmds {
            assert_eq!(target(&cmd, other.path()), dir(&path), "{cmd}");
        }
        // A relative path carries no separator bash would eat, so the escaped
        // space holds on every platform.
        let relative = "cd 業主\\ 原始碼/outsourcing-backend-CN && rg Foo";
        assert_eq!(target(relative, t.path()), dir(&path), "{relative}");
    }

    #[test]
    fn test_cd_target_relative_to_envelope_cwd_resolves() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("sub")).unwrap();
        assert_eq!(
            target("cd sub && grep -rn Foo .", t.path()),
            dir(&t.path().join("sub"))
        );
    }

    #[test]
    fn test_cd_target_home_tilde_expands_via_home_only() {
        let h = tempfile::tempdir().unwrap();
        std::fs::create_dir(h.path().join("x")).unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let go =
            |cmd: &str, home| cd_target_with_home(cmd, cwd.path().to_str().unwrap(), home, false);
        assert_eq!(go("cd ~ && grep Foo", Some(h.path())), dir(h.path()));
        assert_eq!(
            go("cd ~/x && grep Foo", Some(h.path())),
            dir(&h.path().join("x"))
        );
        assert_eq!(
            go("cd 2>/dev/null ~/x && grep Foo", Some(h.path())),
            dir(&h.path().join("x"))
        );
        assert_eq!(
            go("cd 2> /dev/null ~/x && grep Foo", Some(h.path())),
            dir(&h.path().join("x"))
        );
        assert_eq!(go("cd ~/x && grep Foo", None), Unknown);
        assert_eq!(go("cd ~bob && grep Foo", Some(h.path())), Unknown);
    }

    #[test]
    fn test_cd_target_unknowable_target_returns_unknown() {
        let t = tempfile::tempdir().unwrap();
        for cmd in [
            "cd - && grep Foo .",
            "cd && grep Foo .",
            "cd $HOME && grep Foo .",
            "cd \"$(pwd)/x\" && grep Foo .",
            "cd `pwd` && grep Foo .",
            "cd -P . && grep Foo .",
            "cd . extra; grep Foo .",
        ] {
            assert_eq!(target(cmd, t.path()), Unknown, "{cmd}");
        }
    }

    #[test]
    fn test_cd_target_nonexistent_dir_returns_unknown() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(
            target("cd /no/such/dir_zz && grep Foo .", t.path()),
            Unknown
        );
    }

    #[test]
    fn test_cd_target_last_cd_wins_and_chains_relative() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("a/b")).unwrap();
        std::fs::create_dir(t.path().join("c")).unwrap();
        assert_eq!(
            target("cd a && cd b && grep Foo .", t.path()),
            dir(&t.path().join("a/b"))
        );
        assert_eq!(
            target("cd a && cd ../c && grep Foo .", t.path()),
            dir(&t.path().join("c"))
        );
        // A cd after the search segment does not move the search.
        assert_eq!(
            target("cd a && grep Foo . ; cd ../c", t.path()),
            dir(&t.path().join("a"))
        );
    }

    #[test]
    fn test_cd_target_semicolon_and_newline_separators_resolve() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("x")).unwrap();
        let want = dir(&t.path().join("x"));
        for cmd in [
            "cd x; grep Foo .",
            "cd x\ngrep Foo .",
            "make & cd x; rg Foo",
        ] {
            assert_eq!(target(cmd, t.path()), want, "{cmd}");
        }
    }

    #[test]
    fn test_cd_target_cd_that_may_not_run_returns_unknown() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("x")).unwrap();
        for cmd in [
            "true || cd x; rg Foo",
            "false || cd x && rg Foo",
            "make && cd x; rg Foo",
            "cd x; cd \"$PWD\"; rg Foo",
        ] {
            assert_eq!(target(cmd, t.path()), Unknown, "{cmd}");
        }
        assert_eq!(
            target("make && cd x && rg Foo", t.path()),
            dir(&t.path().join("x"))
        );
    }

    #[test]
    fn test_cd_target_cd_outside_search_shell_returns_unchanged() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("x")).unwrap();
        for cmd in [
            "cd x | rg Foo",
            "cd x & rg Foo",
            "cd x && make & rg Foo",
            "make || cd x & rg Foo",
            "cd x || true & rg Foo",
            "(cd x && make); rg Foo",
            "(make; cd x); rg Foo",
            "echo | pushd x; rg Foo",
        ] {
            assert_eq!(target(cmd, t.path()), Unchanged, "{cmd}");
        }
    }

    #[test]
    fn test_cd_target_search_word_anywhere_stops_scan() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("x")).unwrap();
        for cmd in [
            "rtk proxy rg Foo; cd x",
            "git grep Foo; cd x",
            "find . | xargs grep Foo; cd x",
        ] {
            assert_eq!(target(cmd, t.path()), Unchanged, "{cmd}");
        }
    }

    #[test]
    fn test_cd_target_quoted_tilde_is_a_relative_dir() {
        let h = tempfile::tempdir().unwrap();
        std::fs::create_dir(h.path().join("repo")).unwrap();
        let cwd = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(cwd.path().join("~/repo")).unwrap();
        let go = |cmd: &str| {
            cd_target_with_home(cmd, cwd.path().to_str().unwrap(), Some(h.path()), false)
        };
        let literal = dir(&cwd.path().join("~/repo"));
        assert_eq!(go("cd '~/repo'; rg Foo"), literal);
        assert_eq!(go("cd \"~/repo\" && rg Foo"), literal);
        assert_eq!(go("cd ~/repo && rg Foo"), dir(&h.path().join("repo")));
    }

    #[test]
    fn test_cd_target_cwd_builtin_or_open_quote_returns_unknown() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("x")).unwrap();
        for cmd in [
            "cd x; cd -; rg Foo",
            "cd x; cd $D; rg Foo",
            "pushd x; rg Foo",
            "cd x; popd; rg Foo",
            "cd x; source env.sh; rg Foo",
            "cd x && grep \"Foo .",
            "cd missing || cd x; rg Foo",
        ] {
            assert_eq!(target(cmd, t.path()), Unknown, "{cmd}");
        }
    }

    #[test]
    fn test_cd_target_or_operand_skipped_after_successful_cd() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("x/y")).unwrap();
        let want = dir(&t.path().join("x"));
        for cmd in [
            "cd x || exit 1; rg Foo",
            "cd x || false && rg Foo",
            "cd x || { echo no; exit 1; }; rg Foo",
            "cd x || { echo failed; cd y; }; rg Foo",
            "cd x || (cd y; exit); rg Foo",
        ] {
            assert_eq!(target(cmd, t.path()), want, "{cmd}");
        }
    }

    #[test]
    fn test_cd_target_quoted_group_mark_in_skipped_operand_opens_no_group() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("x/y")).unwrap();
        let want = dir(&t.path().join("x/y"));
        for cmd in [
            "cd x || echo '{'; cd y; rg Foo",
            "cd x || echo \"(\"; cd y; rg Foo",
            "cd x || echo \\{; cd y; rg Foo",
        ] {
            assert_eq!(target(cmd, t.path()), want, "{cmd}");
        }
    }

    #[test]
    fn test_cd_target_comment_text_does_not_hide_a_later_cd() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("x")).unwrap();
        let want = dir(&t.path().join("x"));
        for cmd in [
            "echo ready # call foo(\ncd x; rg Foo",
            "echo ready # don't { \ncd x; rg Foo",
            "echo ready #; cd /\ncd x; rg Foo",
            "echo a#b; cd x; rg Foo",
        ] {
            assert_eq!(target(cmd, t.path()), want, "{cmd:?}");
        }
    }

    #[test]
    fn test_cd_target_hash_inside_expansion_or_after_cr_is_literal() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("x")).unwrap();
        let want = dir(&t.path().join("x"));
        for cmd in [
            "echo ${v:- #}; cd x; rg Foo",
            "echo ${#PATH}; cd x; rg Foo",
            "echo foo\r#; cd x; rg Foo",
            "cat <<< '#('; cd x; rg Foo",
        ] {
            assert_eq!(target(cmd, t.path()), want, "{cmd:?}");
        }
    }

    #[test]
    fn test_cd_target_heredoc_or_open_expansion_returns_unknown() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("x")).unwrap();
        for cmd in [
            "cat <<EOF\n( # )\nEOF\ncd x; rg Foo",
            "echo ${v:-a; cd x; rg Foo",
            "echo ${v:-'}; cd x; #'}; rg Foo",
            "cd x; echo ${v:-{}; cd /; # }\nrg Foo",
            "cd x; ! cd /; rg Foo",
            "cd x; time cd /; rg Foo",
            "cd x; CDPATH=/var; cd tmp; rg Foo",
            "cd x; CDPATH=/var; cd .hidden; rg Foo",
            "cd x; ! . /dev/stdin <<< \"cd /\"; rg Foo",
            "cd x; time . env.sh; rg Foo",
            "cd x; time -p . env.sh; rg Foo",
            "cd x; cd /u?r; rg Foo",
            "cd x; set -P; cd ..; rg Foo",
            "cd x; set -o physical; cd ..; rg Foo",
        ] {
            assert_eq!(target(cmd, t.path()), Unknown, "{cmd:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_cd_target_parent_of_symlink_is_logical() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("real/inner")).unwrap();
        std::os::unix::fs::symlink(t.path().join("real/inner"), t.path().join("link")).unwrap();
        assert_eq!(target("cd link; cd ..; rg Foo", t.path()), dir(t.path()));
        assert_eq!(target("cd link/..; rg Foo", t.path()), dir(t.path()));
        assert_eq!(target("cd missing/..; rg Foo", t.path()), Unknown);
        assert_eq!(target("cd link/../missing; rg Foo", t.path()), Unknown);
        // Physical `real/inner/../other` exists, logical `other` does not.
        std::fs::create_dir(t.path().join("real/other")).unwrap();
        assert_eq!(target("cd link/../other; rg Foo", t.path()), Unknown);
    }

    #[test]
    fn test_cd_target_inherited_cdpath_makes_relative_cd_unknown() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("x")).unwrap();
        let go = |cmd: &str| cd_target_with_home(cmd, t.path().to_str().unwrap(), None, true);
        assert_eq!(go("cd x; rg Foo"), Unknown);
        assert_eq!(go("cd ./x; rg Foo"), dir(&t.path().join("x")));
    }

    #[test]
    fn test_cd_target_line_continuation_and_prefixed_dot_argument_resolve() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("x")).unwrap();
        std::fs::create_dir(t.path().join("y")).unwrap();
        let want = dir(&t.path().join("x"));
        for cmd in [
            "cd x && \\\nrg Foo",
            "cd \\\nx; rg Foo",
            "cd x && time git diff -- .; rg Foo",
            "cd y\n# cd z && \\\ncd ../x\nrg Foo",
        ] {
            assert_eq!(target(cmd, t.path()), want, "{cmd:?}");
        }
        // Single quotes keep the backslash-newline: the cd names `x\<nl>y`,
        // which does not exist, not `xy`, which does.
        std::fs::create_dir(t.path().join("x/xy")).unwrap();
        assert_eq!(target("cd x; cd 'x\\\ny'; rg Foo", t.path()), Unknown);
    }

    #[test]
    fn test_cd_target_cd_in_the_search_background_job_resolves() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("x")).unwrap();
        let want = dir(&t.path().join("x"));
        for cmd in [
            "cd x && rg Foo & wait",
            "cd x; rg Foo &",
            "make & cd x && rg Foo &",
        ] {
            assert_eq!(target(cmd, t.path()), want, "{cmd}");
        }
    }

    #[test]
    fn test_cd_target_cd_that_may_not_reach_the_search_is_unknown() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("x")).unwrap();
        for cmd in [
            "(cd x && rg Foo) | head",
            "false && cd x && true; rg Foo",
            "false &&\ncd x; rg Foo",
            "false && # note\n\ncd x; rg Foo",
            "false || \ncd x; rg Foo",
            "false && { cd x; }; rg Foo",
            "{ cd x; } | cat; rg Foo",
            "f() {\ncd x\n}\nrg Foo",
            "if false; then true; cd x; fi; rg Foo",
            "while true; do true; cd x; rg Foo; done",
            "for d in a b; do rg Foo; cd x; done",
            "mkdir -p new && cd new && rg -n Foo src",
            "git worktree add ../wt && cd ../wt && rg Foo",
            "cd x &&\nuntil grep -q '^w' Cargo.toml 2>&1; do\n  cd .. || exit 1\ndone",
            "if test -f /dev/null; then\n  cd x\nelse rg Foo .\nfi",
            "if false; then\n  cd x\nelse\n  true; rg Foo .\nfi",
            "case a in\nb)\n  cd x;;\na) rg Foo;;\nesac",
            "m=$(cd x && rg -l Foo .)",
            "m=`cd x && rg -l Foo .`",
            "git -C x grep -n Foo",
            "GIT_WORK_TREE=/x git grep -n Foo",
            "export GIT_DIR=/x/.git; git grep Foo",
            "scan() {\n  rg -n Foo src | head\n}\ncd x && scan",
            "function scan { rg -n Foo src; }; cd x && scan",
            "scan () { rg Foo; }; cd x; scan",
            "cd x && case a in\n  a) make ;;\n  b) make test ;;\nesac &\nrg -n Foo src\nwait",
            "cd x && git --work-tree=y grep Foo",
        ] {
            assert_eq!(target(cmd, t.path()), Unknown, "{cmd:?}");
        }
    }

    #[test]
    fn test_cd_target_cd_every_path_to_the_search_runs_keeps_cd() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("x")).unwrap();
        std::fs::create_dir(t.path().join("grep")).unwrap();
        let want = dir(&t.path().join("x"));
        for cmd in [
            "make && cd x && rg Foo",
            "make && cd x && cat f | rg Foo",
            "cd x &&\nrg Foo",
            "cat f | { cd x; rg Foo; }",
            "if true; then true; cd x; rg Foo; fi",
            "command -v rg && cd x && rg Foo .",
            "which grep >/dev/null; cd x; grep -rn Foo .",
            "command -v rg grep git\ncd x && rg -n Foo src",
            "which rg grep; cd x && rg -n Foo src",
            "cd grep && cd ../x && rg -n main src",
            "rg --version &&\ncd x &&\nrg -n Foo src",
            "cd x>/dev/null &&\nrg -n Foo src",
            "cd x 2>/dev/null && rg Foo",
            "cd x || if test -d y; then\n  cd y\nfi\nrg -n Foo src",
            "cd x && rg -n 'fn main()' src",
            "cd 2>/dev/null x && rg Foo",
            "cd x &>/dev/null; rg Foo",
            "cd x &&\n  (git status >/dev/null &\n   git diff --stat >/dev/null & wait)\nrg -n Foo src",
            "cd x && {\n  rg -n -m 1 Foo a\n  rg -n -m 1 Bar a\n} & wait",
            "cd x && git status --short |& head\nrg -n Foo src",
        ] {
            assert_eq!(target(cmd, t.path()), want, "{cmd:?}");
        }
        for cmd in [
            "cd x &&\nif test -f a; then\n  git diff --stat\nfi &\nrg -n Foo a\nwait",
            "cd x && { git status --short; }>/dev/null 2>&1 &\nrg -n Foo src\nwait",
            "cd x && if test -d .; then pwd; fi>/dev/null 2>&1 &\nrg -n Foo src\nwait",
            "cd x && { git status --short; } >/dev/null 2>&1 &\nrg -n Foo src\nwait",
            "make &&\nrg Foo",
            "m=$(cd x && pwd); rg Foo .",
            "git grep -n Foo",
            "root=$( cd x && pwd -P )\nrg -n Foo src",
            "rg -C 3 Foo",
            "git -C x status; rg Foo",
        ] {
            assert_eq!(target(cmd, t.path()), Unchanged, "{cmd:?}");
        }
    }

    #[test]
    fn test_search_call_lookup_of_rg_is_not_the_search() {
        for cmd in [
            "rg --color never -n -m 2 HookInput src",
            "cd grep && rg -n HookInput src",
            "rg --version && cd /tmp && rg -n HookInput src",
            "rg 2>/dev/null HookInput src",
            "rg -n --regexp=HookInput src/a.rs",
            "grep -rn -eHookInput src",
            "grep -rn HookInput>/tmp/out .",
            "cd /a\nrg -n -m 1 HookInput src\ncd /b\nrg --files -g x.rs | xargs grep -n other_name",
            "grep -V; grep -rn HookInput .",
            "command -v rg grep git\ncd /tmp && rg -n -m 1 HookInput src",
            "rg --files -g '**/x.rs' | xargs grep -n -m 2 HookInput",
            "command -v rg && cd /tmp && rg HookInput .",
            "which grep; grep -rn HookInput .",
            "type -p rg >/dev/null && rg HookInput",
        ] {
            assert_eq!(pattern_of(cmd).as_deref(), Some("HookInput"), "{cmd}");
        }
        assert_eq!(
            pattern_of("command rg HookInput").as_deref(),
            Some("HookInput")
        );
    }

    #[test]
    fn test_cd_target_assignments_and_groups_keep_cd() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("x")).unwrap();
        let want = dir(&t.path().join("x"));
        for cmd in [
            "X=1; cd x && rg Foo",
            "X=1 cd x && rg Foo",
            "{ cd x; rg Foo; }",
        ] {
            assert_eq!(target(cmd, t.path()), want, "{cmd}");
        }
    }

    #[test]
    fn test_cd_target_quoted_tilde_in_search_does_not_block_leading_tilde() {
        let h = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        assert_eq!(
            cd_target_with_home(
                "cd ~ && grep 'cd \"~\"' Foo .",
                cwd.path().to_str().unwrap(),
                Some(h.path()),
                false
            ),
            dir(h.path())
        );
    }

    #[test]
    fn test_cd_target_cd_inside_quotes_is_not_a_segment() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().to_str().unwrap();
        let cmd = format!("echo \"cd {d}\" && grep -rn Foo app");
        assert_eq!(target(&cmd, t.path()), Unchanged);
        let cmd = format!("echo 'a; cd {d}' && grep -rn Foo app");
        assert_eq!(target(&cmd, t.path()), Unchanged);
    }

    #[test]
    fn test_shell_segments_splits_only_unquoted_separators() {
        let segs: Vec<(&str, Option<Sep>)> = shell_segments("cd 'a;b' && grep \"x|y\" . | head")
            .unwrap()
            .into_iter()
            .map(|(s, sep)| (s.trim(), sep))
            .collect();
        assert_eq!(
            segs,
            [
                ("cd 'a;b'", Some(Sep::And)),
                ("grep \"x|y\" .", Some(Sep::Pipe)),
                ("head", None)
            ]
        );
        assert_eq!(
            shell_segments("make 2>&1 <&0 &>log >|f; x").unwrap(),
            [("make 2>&1 <&0 &>log >|f", Some(Sep::Seq)), (" x", None)]
        );
    }

    #[test]
    fn grep_quoted_multiword_pattern_kept_whole() {
        // Whitespace splitting handed the hook `"pub` (cleaned to `pub`) and
        // the graph answered with every `pub_*` symbol in the repo.
        let cmd = r#"grep -n "pub struct HookInput" -A12 crates/x.rs | head"#;
        assert_eq!(pattern_of(cmd), Some("pub struct HookInput".to_string()));
        let cmd = "rg -n 'fn compute hits' src/";
        assert_eq!(pattern_of(cmd), Some("fn compute hits".to_string()));
    }

    #[test]
    fn dash_e_names_the_pattern_instead_of_hiding_it() {
        assert_eq!(
            pattern_of("rg -e validateUser src/"),
            Some("validateUser".to_string())
        );
        assert_eq!(
            pattern_of("grep -rn --regexp validateUser src/"),
            Some("validateUser".to_string())
        );
        // A pattern that itself starts with a dash is still the pattern.
        assert_eq!(
            pattern_of("rg -e -foo_bar src/"),
            Some("-foo_bar".to_string())
        );
    }

    #[test]
    fn grep_double_quoted_regex_keeps_its_backslashes() {
        // POSIX: inside "..." a backslash before anything other than
        // $ ` " \\ or newline is literal, so the regex reaches the
        // identifier split unchanged and reduces to `fetch`, not `bfetchs`.
        let cmd = r#"grep -rn "\bfetch\s*\(" src/"#;
        assert_eq!(pattern_of(cmd).as_deref(), Some(r"\bfetch\s*\("));
        let cmd = r#"grep "a\$b\\c" f"#;
        assert_eq!(pattern_of(cmd), Some(r"a$b\c".to_string()));
    }

    #[test]
    fn grep_escaped_quote_inside_pattern_survives() {
        let cmd = r#"grep -rn "say \"hi\" now" ."#;
        assert_eq!(pattern_of(cmd), Some(r#"say "hi" now"#.to_string()));
    }

    #[test]
    fn grep_double_quoted_pattern_extracted() {
        // Regression: `strip_shell_quotes` used to delete the entire quoted
        // block, leaving the hook to pick up the next non-flag token (often
        // a pipe-side `head` / `tail`) and surface unrelated graph noise.
        let cmd = r#"git show abc:foo.rs | grep -nE "summary_blind_spots" | head -20"#;
        assert_eq!(pattern_of(cmd), Some("summary_blind_spots".to_string()));
    }

    #[test]
    fn grep_single_quoted_pattern_extracted() {
        let cmd = "rg -n 'validateUser' src/";
        assert_eq!(pattern_of(cmd), Some("validateUser".to_string()));
    }

    #[test]
    fn grep_regex_metachars_preserved() {
        let cmd = r#"grep -E "(compute_single|score|bm25)" file.rs"#;
        assert_eq!(
            pattern_of(cmd),
            Some("(compute_single|score|bm25)".to_string())
        );
    }

    #[test]
    fn no_grep_returns_none() {
        assert_eq!(pattern_of("cat foo.txt | head -20"), None);
    }

    #[test]
    fn test_search_call_operands_follow_the_pattern_and_skip_flag_values() {
        let call = |cmd: &str| search_call(cmd).unwrap();
        assert_eq!(
            call("rg -n -g '*.py' load_user src lib/a.py -A 2"),
            SearchCall {
                pattern: "load_user".into(),
                operands: vec!["src".into(), "lib/a.py".into()],
                reads_stdin: false,
            }
        );
        assert_eq!(
            call("grep -rn -e load_user -e other_name -- -odd.py").operands,
            ["-odd.py"]
        );
        assert!(call("rg load_user").operands.is_empty());
    }

    #[test]
    fn test_search_call_egrep_and_git_grep_are_searches() {
        assert_eq!(
            pattern_of("egrep -rn 'a|load_user' .").as_deref(),
            Some("a|load_user")
        );
        let git = search_call("git grep -n load_user -- src").unwrap();
        assert_eq!(
            (git.pattern.as_str(), git.operands),
            ("load_user", vec!["src".to_string()])
        );
        assert!(
            !search_call("git log | git grep load_user")
                .unwrap()
                .reads_stdin
        );
    }

    #[test]
    fn test_search_call_piped_search_without_operand_reads_stdin() {
        for cmd in [
            "git log --oneline | grep load_user",
            "cargo test 2>&1 | rg test_load_user",
        ] {
            assert!(search_call(cmd).unwrap().reads_stdin, "{cmd}");
        }
        for cmd in [
            "rg -n load_user . | head -20",
            "rg --files | xargs grep -n load_user",
            "cat list | grep load_user src",
        ] {
            assert!(!search_call(cmd).unwrap().reads_stdin, "{cmd}");
        }
    }
}
