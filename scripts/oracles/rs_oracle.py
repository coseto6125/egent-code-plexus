#!/usr/bin/env python3
"""Rust module-resolution oracle for the resolver verification harness.

Emits one JSONL line per `use` binding to stdout, plus a summary on stderr.
Schema matches docs/superpowers/specs/2026-05-15-resolver-oracle-harness.md.

Usage:
    python3 scripts/oracles/rs_oracle.py <repoPath> > oracle.jsonl

Resolution model (structural, not full rustc):

* `use crate::a::b::Bar` (in crate X) → walk X's mod tree starting from
  src/lib.rs (or src/main.rs), follow `mod foo;` decls down to the file
  expected to host `Bar`, then verify Bar is declared `pub` there.
* `use std::*`, `use core::*`, `use alloc::*` → tier="External",
  target_file="<std>".
* `use other_crate::*` → if `other_crate` is a workspace member, resolve
  through its mod tree; otherwise tier="External",
  target_file="<crate:other_crate>".
* `use super::*` / `use self::*` → resolve against the current file's
  module path (super walks one mod level up).

Edge cases NOT handled (documented):
* Macro-expanded `use`s.
* `cfg`-gated `mod` decls that resolve to multiple files — we take the
  first hit on the filesystem.
* Re-export chains: `pub use foo::Bar` from lib.rs forwarding to inner
  `foo::Bar` — we resolve at the textual specifier, not transitively.
"""

from __future__ import annotations

import json
import re
import sys
import tomllib
from pathlib import Path

# --- comment / string stripping --------------------------------------------

_LINE_COMMENT = re.compile(r"//[^\n]*")
_BLOCK_COMMENT = re.compile(r"/\*.*?\*/", re.DOTALL)
# Crude string-literal eater. Rust supports raw strings r#"..."# which we
# under-handle, but `use` keywords inside string literals are extremely rare
# in practice; this pass is just to keep the regex scan honest.
_STRING_LIT = re.compile(r'"(?:\\(?:.|\n)|[^"\\])*"')


def _blank_preserve_newlines(s: str) -> str:
    # Replace every char with a space except newlines, so line numbers and
    # MULTILINE `^` anchors stay aligned for downstream regex passes.
    return "".join("\n" if ch == "\n" else " " for ch in s)


def strip_comments_and_strings(src: str) -> str:
    """Replace comments and string literals with whitespace of equal length.

    Newlines are preserved so MULTILINE regex anchors keep aligning to the
    original line structure (critical: `_MOD_DECL` and `_USE_START` rely on
    `^` matching real line starts).
    """
    src = _BLOCK_COMMENT.sub(lambda m: _blank_preserve_newlines(m.group(0)), src)
    src = _LINE_COMMENT.sub(lambda m: _blank_preserve_newlines(m.group(0)), src)
    src = _STRING_LIT.sub(lambda m: _blank_preserve_newlines(m.group(0)), src)
    return src


# --- use-statement extraction ----------------------------------------------

# Match top-level `use ...;` and `pub use ...;` and `pub(crate) use ...;`.
# We rely on `;` terminator and brace balancing for the tree.
_USE_START = re.compile(
    r"^\s*(?:pub(?:\s*\([^)]*\))?\s+)?use\s+", re.MULTILINE
)

# Match `extern crate NAME;` (or `extern crate NAME as ALIAS;`).
_EXTERN_CRATE = re.compile(
    r"^\s*(?:pub\s+)?extern\s+crate\s+([A-Za-z_][A-Za-z0-9_]*)"
    r"(?:\s+as\s+([A-Za-z_][A-Za-z0-9_]*))?\s*;",
    re.MULTILINE,
)


def find_use_statements(src: str) -> list[str]:
    """Return the body text of each `use ...;` (excluding `use` and `;`)."""
    stripped = strip_comments_and_strings(src)
    out: list[str] = []
    for m in _USE_START.finditer(stripped):
        start = m.end()
        depth = 0
        i = start
        n = len(stripped)
        while i < n:
            ch = stripped[i]
            if ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
            elif ch == ";" and depth == 0:
                break
            i += 1
        if i < n:
            out.append(stripped[start:i].strip())
    return out


def flatten_use_tree(body: str) -> list[tuple[str, str, str | None]]:
    """Flatten one `use` body to a list of (name, specifier, alias).

    `body` is everything between `use ` and `;`. Examples:
      "std::collections::HashMap"
      "std::collections::{HashMap, BTreeMap}"
      "a::b::{c, d::{e, f as g}}"
      "a::b::*"
      "a as b"
    """
    return _flatten(body.strip(), prefix="")


def _flatten(tree: str, prefix: str) -> list[tuple[str, str, str | None]]:
    tree = tree.strip()
    if not tree:
        return []
    # Find the position of the first top-level `{` (depth 0). If present
    # at the end, recursively expand each group element with the prefix
    # before the `{`.
    brace_idx = _find_top_level_brace(tree)
    if brace_idx == -1:
        # leaf: maybe "a::b::Name" or "a::b::Name as Alias" or "*"
        return [_leaf(tree, prefix)]
    # Split into "head" before `{` and grouped items inside `{ ... }`.
    head = tree[:brace_idx].rstrip()
    # Strip trailing `::` from head.
    if head.endswith("::"):
        head = head[:-2]
    group = tree[brace_idx + 1 : _matching_brace(tree, brace_idx)]
    new_prefix = _join(prefix, head)
    out: list[tuple[str, str, str | None]] = []
    for item in _split_top_level_commas(group):
        item = item.strip()
        if not item:
            continue
        out.extend(_flatten(item, new_prefix))
    return out


def _join(a: str, b: str) -> str:
    if not a:
        return b
    if not b:
        return a
    return f"{a}::{b}"


def _find_top_level_brace(s: str) -> int:
    depth = 0
    for i, ch in enumerate(s):
        if ch == "{" and depth == 0:
            return i
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
    return -1


def _matching_brace(s: str, open_idx: int) -> int:
    depth = 0
    for i in range(open_idx, len(s)):
        if s[i] == "{":
            depth += 1
        elif s[i] == "}":
            depth -= 1
            if depth == 0:
                return i
    return len(s)


def _split_top_level_commas(s: str) -> list[str]:
    depth = 0
    parts: list[str] = []
    start = 0
    for i, ch in enumerate(s):
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
        elif ch == "," and depth == 0:
            parts.append(s[start:i])
            start = i + 1
    parts.append(s[start:])
    return parts


_ALIAS = re.compile(r"^(.*?)\s+as\s+([A-Za-z_][A-Za-z0-9_]*)\s*$")


def _leaf(item: str, prefix: str) -> tuple[str, str, str | None]:
    """Convert a leaf like `a::b::Name` or `a::b::Name as Alias` or `self`
    to (name, specifier, alias)."""
    item = item.strip()
    alias: str | None = None
    m = _ALIAS.match(item)
    if m:
        item = m.group(1).strip()
        alias = m.group(2)
    full = _join(prefix, item)
    # split into specifier and last segment
    if "::" in full:
        spec, _, last = full.rpartition("::")
    else:
        spec, last = "", full
    # `use foo::{self}` binds `foo` itself.
    if last == "self":
        if "::" in spec:
            spec, _, last = spec.rpartition("::")
        else:
            last = spec
            spec = ""
    name = alias if alias is not None else last
    return (name, spec, alias)


# --- workspace + crate discovery -------------------------------------------


def load_workspace(repo: Path) -> dict:
    """Discover workspace + crates.

    Returns:
        {
          "workspace_root": Path,
          "crates": { crate_name: { "root": Path, "src": Path, "entry": Path } },
        }
    """
    # Walk down only one level: the harness convention is the repo root is
    # the workspace.
    root_toml = repo / "Cargo.toml"
    if not root_toml.exists():
        # fall back: find first Cargo.toml in any subdir
        candidates = list(repo.glob("**/Cargo.toml"))
        if not candidates:
            return {"workspace_root": repo, "crates": {}}
        root_toml = candidates[0]
    with open(root_toml, "rb") as f:
        try:
            data = tomllib.load(f)
        except Exception as e:
            print(f"[rs_oracle] failed to parse {root_toml}: {e}", file=sys.stderr)
            data = {}
    crates: dict[str, dict] = {}
    workspace_root = root_toml.parent
    members = data.get("workspace", {}).get("members", []) if isinstance(data, dict) else []
    # If there is no [workspace], treat the root as a single crate.
    if not members:
        if "package" in data:
            name = data["package"].get("name")
            if name:
                crates[name] = _crate_info(workspace_root)
        return {"workspace_root": workspace_root, "crates": crates}
    for pattern in members:
        # Resolve glob; cargo allows e.g. "crates/*".
        for member_dir in sorted(workspace_root.glob(pattern)):
            ctoml = member_dir / "Cargo.toml"
            if not ctoml.exists():
                continue
            try:
                with open(ctoml, "rb") as f:
                    cdata = tomllib.load(f)
            except Exception as e:
                print(f"[rs_oracle] failed to parse {ctoml}: {e}", file=sys.stderr)
                continue
            name = cdata.get("package", {}).get("name")
            if not name:
                continue
            info = _crate_info(member_dir)
            crates[name] = info
            # Cargo normalizes crate names: `-` in package name becomes `_`
            # when imported. Register both so use-statements like
            # `use tokio_macros::*;` match the `tokio-macros` member crate.
            norm = name.replace("-", "_")
            if norm != name and norm not in crates:
                crates[norm] = info
    return {"workspace_root": workspace_root, "crates": crates}


def _crate_info(crate_dir: Path) -> dict:
    src = crate_dir / "src"
    entry: Path | None = None
    for cand in ("lib.rs", "main.rs"):
        p = src / cand
        if p.exists():
            entry = p
            break
    return {"root": crate_dir, "src": src, "entry": entry}


# --- mod tree walking ------------------------------------------------------

# Matches `mod foo;` (file-backed) and `pub mod foo;`. Does NOT match
# `mod foo { ... }` (inline modules) — those don't change the file mapping.
_MOD_DECL = re.compile(
    r"^\s*(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;",
    re.MULTILINE,
)

# Matches inline modules: `mod foo { ... }` — we recurse INTO these to
# find further `mod bar;` decls, but the file boundary doesn't change.
_INLINE_MOD = re.compile(
    r"^\s*(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*\{",
    re.MULTILINE,
)


def file_for_mod(parent_file: Path, mod_name: str) -> Path | None:
    """Given parent file P, find the child module file for `mod NAME;`.

    Conventions:
      * If P is `src/lib.rs`, `src/main.rs`, or `foo/mod.rs`, children live
        as siblings: `foo/NAME.rs` or `foo/NAME/mod.rs`.
      * Otherwise (P is `foo/bar.rs`), children live in `foo/bar/NAME.rs`
        or `foo/bar/NAME/mod.rs`.
    """
    parent_dir = parent_file.parent
    if parent_file.name in ("lib.rs", "main.rs", "mod.rs"):
        base = parent_dir
    else:
        base = parent_dir / parent_file.stem
    cand_flat = base / f"{mod_name}.rs"
    cand_mod = base / mod_name / "mod.rs"
    if cand_flat.exists():
        return cand_flat
    if cand_mod.exists():
        return cand_mod
    return None


def build_mod_tree(entry: Path) -> dict[tuple[str, ...], Path]:
    """Walk from `entry` and return a map of mod_path → file.

    mod_path is a tuple, e.g. ("runtime", "handle") for
    `tokio/src/runtime/handle.rs`. The crate root maps to ().
    """
    tree: dict[tuple[str, ...], Path] = {(): entry}
    stack: list[tuple[tuple[str, ...], Path]] = [((), entry)]
    visited: set[Path] = {entry}
    while stack:
        mod_path, file = stack.pop()
        try:
            src = file.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        clean = strip_comments_and_strings(src)
        # file-backed mods
        for m in _MOD_DECL.finditer(clean):
            name = m.group(1)
            child = file_for_mod(file, name)
            if child is None or child in visited:
                continue
            visited.add(child)
            new_path = mod_path + (name,)
            tree[new_path] = child
            stack.append((new_path, child))
        # inline mods: their child `mod X;` decls would file-resolve relative
        # to the enclosing file (uncommon; cargo also supports `#[path]`
        # which we ignore). For v1 we don't descend into inline mod bodies
        # because the path-mapping rules get hairy and the dominant case in
        # workspaces is file-backed mods. Recorded edge case.
    return tree


# --- symbol scanning -------------------------------------------------------

_PUB_ITEM_TPL = (
    r"^\s*pub(?:\s*\([^)]*\))?\s+"
    r"(?:struct|fn|enum|trait|const|type|mod|static|union|macro|use)\s+"
)


def file_defines_pub(file: Path, name: str) -> bool:
    """Does `file` declare a top-level `pub <kind> NAME`?"""
    try:
        src = file.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return False
    clean = strip_comments_and_strings(src)
    pat = re.compile(_PUB_ITEM_TPL + re.escape(name) + r"\b", re.MULTILINE)
    if pat.search(clean):
        return True
    # `pub use foo::Bar` exposes Bar from this file too.
    reexport = re.compile(
        r"^\s*pub(?:\s*\([^)]*\))?\s+use\s+[^;]*\b"
        + re.escape(name)
        + r"\b[^;]*;",
        re.MULTILINE,
    )
    return bool(reexport.search(clean))


# --- resolution ------------------------------------------------------------

STD_CRATES = {"std", "core", "alloc", "proc_macro", "test"}


def split_specifier(specifier: str) -> list[str]:
    """Split `a::b::c` to ['a','b','c']. Empty input → []."""
    if not specifier:
        return []
    return [seg for seg in specifier.split("::") if seg]


def resolve_use(
    name: str,
    specifier: str,
    src_file: Path,
    src_crate: str | None,
    src_mod_path: tuple[str, ...] | None,
    workspace: dict,
) -> tuple[str, Path | str | None, float | None]:
    """Resolve one binding.

    Returns (tier, target_file_or_marker, confidence).

      * tier ∈ {"ImportScoped", "External", "Unresolved"}
      * target marker is a Path for in-repo, a string ("<std>" or
        "<crate:NAME>") for external, or None for unresolved.
    """
    segs = split_specifier(specifier)
    # Glob imports (`use a::b::*;`) don't bind a name we can verify; we still
    # emit them but resolve the *module* path.
    if name == "*":
        # Treat the full specifier as the module to resolve.
        return _resolve_module_path(segs, src_file, src_crate, src_mod_path, workspace)
    # Pure-name (extern crate): specifier empty.
    if not segs:
        # name itself is the root: `extern crate foo;` or `use foo;`
        if name in STD_CRATES:
            return ("External", "<std>", 1.0)
        if name in workspace["crates"]:
            entry = workspace["crates"][name]["entry"]
            if entry is not None:
                return ("ImportScoped", entry, 1.0)
        return ("External", f"<crate:{name}>", 1.0)

    head = segs[0]
    # std/core/alloc → external
    if head in STD_CRATES:
        return ("External", "<std>", 1.0)
    # crate:: → current crate
    if head == "crate":
        if src_crate is None:
            return ("Unresolved", None, None)
        target_segs = segs[1:] + [name]
        return _resolve_in_crate(src_crate, target_segs, workspace)
    # super:: → walk up src_mod_path
    if head == "super":
        if src_mod_path is None or src_crate is None:
            return ("Unresolved", None, None)
        up = 0
        i = 0
        while i < len(segs) and segs[i] == "super":
            up += 1
            i += 1
        if up > len(src_mod_path):
            return ("Unresolved", None, None)
        base = src_mod_path[: len(src_mod_path) - up]
        target_segs = list(base) + segs[i:] + [name]
        return _resolve_in_crate(src_crate, target_segs, workspace)
    # self:: → current module
    if head == "self":
        if src_mod_path is None or src_crate is None:
            return ("Unresolved", None, None)
        target_segs = list(src_mod_path) + segs[1:] + [name]
        return _resolve_in_crate(src_crate, target_segs, workspace)
    # Otherwise, head is a crate name (workspace member or external).
    if head in workspace["crates"]:
        target_segs = segs[1:] + [name]
        return _resolve_in_crate(head, target_segs, workspace)
    # Rust 2015 / inside-module relative: `use task::spawn` from a file
    # that has `pub mod task;` as a child. Try the head as a child of the
    # current module path before giving up to External.
    if src_crate is not None and src_mod_path is not None:
        tree = workspace["crates"][src_crate].get("tree", {})
        candidate_mod = src_mod_path + (head,)
        if candidate_mod in tree:
            target_segs = list(candidate_mod) + segs[1:] + [name]
            return _resolve_in_crate(src_crate, target_segs, workspace)
    return ("External", f"<crate:{head}>", 1.0)


def _resolve_module_path(
    segs: list[str],
    src_file: Path,
    src_crate: str | None,
    src_mod_path: tuple[str, ...] | None,
    workspace: dict,
) -> tuple[str, Path | str | None, float | None]:
    """Resolve a module path (for glob `use a::b::*;`)."""
    if not segs:
        return ("Unresolved", None, None)
    head = segs[0]
    if head in STD_CRATES:
        return ("External", "<std>", 1.0)
    if head == "crate" and src_crate is not None:
        tree = workspace["crates"][src_crate].get("tree", {})
        mod_path = tuple(segs[1:])
        if mod_path in tree:
            return ("ImportScoped", tree[mod_path], 1.0)
        return ("Unresolved", None, None)
    if head == "super" and src_mod_path is not None and src_crate is not None:
        up = 0
        i = 0
        while i < len(segs) and segs[i] == "super":
            up += 1
            i += 1
        if up > len(src_mod_path):
            return ("Unresolved", None, None)
        base = src_mod_path[: len(src_mod_path) - up]
        mod_path = base + tuple(segs[i:])
        tree = workspace["crates"][src_crate].get("tree", {})
        if mod_path in tree:
            return ("ImportScoped", tree[mod_path], 1.0)
        return ("Unresolved", None, None)
    if head == "self" and src_mod_path is not None and src_crate is not None:
        mod_path = src_mod_path + tuple(segs[1:])
        tree = workspace["crates"][src_crate].get("tree", {})
        if mod_path in tree:
            return ("ImportScoped", tree[mod_path], 1.0)
        return ("Unresolved", None, None)
    if head in workspace["crates"]:
        tree = workspace["crates"][head].get("tree", {})
        mod_path = tuple(segs[1:])
        if mod_path in tree:
            return ("ImportScoped", tree[mod_path], 1.0)
        return ("Unresolved", None, None)
    return ("External", f"<crate:{head}>", 1.0)


def _resolve_in_crate(
    crate_name: str,
    target_segs: list[str],
    workspace: dict,
) -> tuple[str, Path | str | None, float | None]:
    """Given a target path inside `crate_name` (e.g. ["foo","bar","Bar"]),
    find the file declaring `Bar` (the last seg).

    The strategy: walk segs from longest to shortest as a module prefix; for
    each prefix, check if the prefix's file declares the suffix's first name
    as a pub item or pub use. The first hit wins.
    """
    if crate_name not in workspace["crates"]:
        return ("Unresolved", None, None)
    crate = workspace["crates"][crate_name]
    tree: dict[tuple[str, ...], Path] = crate.get("tree", {})
    if not tree or not target_segs:
        return ("Unresolved", None, None)
    # Longest possible module prefix is len(target_segs) - 1 (last seg is
    # always the item name in our model). But some segs may themselves be
    # re-exported items, so we try shorter prefixes too.
    for prefix_len in range(len(target_segs) - 1, -1, -1):
        prefix = tuple(target_segs[:prefix_len])
        if prefix not in tree:
            continue
        suffix = target_segs[prefix_len:]
        if not suffix:
            # The "item" is actually the module itself.
            return ("ImportScoped", tree[prefix], 1.0)
        item_name = suffix[0]
        file = tree[prefix]
        if file_defines_pub(file, item_name):
            return ("ImportScoped", file, 1.0)
    return ("Unresolved", None, None)


# --- per-file processing ---------------------------------------------------


def determine_crate_and_mod_path(
    file: Path, workspace: dict
) -> tuple[str | None, tuple[str, ...] | None]:
    """Map a file path to (crate_name, mod_path). Returns (None, None) if
    the file isn't part of any registered crate's mod tree."""
    for crate_name, crate in workspace["crates"].items():
        tree = crate.get("tree", {})
        for mod_path, mod_file in tree.items():
            if mod_file == file:
                return (crate_name, mod_path)
    return (None, None)


def to_repo_rel_posix(path: Path | str | None, repo_root: Path) -> str | None:
    if path is None:
        return None
    if isinstance(path, str):
        return path
    try:
        rel = path.resolve().relative_to(repo_root.resolve())
    except ValueError:
        return path.as_posix()
    return rel.as_posix()


def process_file(
    file: Path,
    workspace: dict,
    repo_root: Path,
    counters: dict,
) -> list[dict]:
    try:
        src = file.read_text(encoding="utf-8", errors="replace")
    except OSError as e:
        print(f"[rs_oracle] read failed for {file}: {e}", file=sys.stderr)
        return []
    src_rel = to_repo_rel_posix(file, repo_root)
    src_crate, src_mod_path = determine_crate_and_mod_path(file, workspace)
    out: list[dict] = []
    stripped = strip_comments_and_strings(src)
    # extern crate
    for m in _EXTERN_CRATE.finditer(stripped):
        crate_name = m.group(1)
        alias = m.group(2)
        bound_name = alias if alias is not None else crate_name
        tier, target, conf = resolve_use(
            bound_name, "", file, src_crate, src_mod_path, workspace
        )
        if crate_name in workspace["crates"]:
            entry = workspace["crates"][crate_name]["entry"]
            tier, target, conf = ("ImportScoped", entry, 1.0)
        elif crate_name in STD_CRATES:
            tier, target, conf = ("External", "<std>", 1.0)
        else:
            tier, target, conf = ("External", f"<crate:{crate_name}>", 1.0)
        counters[tier] = counters.get(tier, 0) + 1
        out.append(
            {
                "src_file": src_rel,
                "name": bound_name,
                "specifier": "",
                "tier": tier,
                "target_file": to_repo_rel_posix(target, repo_root),
                "target_kind": None,
                "alt_count": 0,
                "confidence": conf,
            }
        )
    # use statements
    for body in find_use_statements(src):
        try:
            bindings = flatten_use_tree(body)
        except Exception as e:
            print(f"[rs_oracle] parse error in {src_rel}: {e}", file=sys.stderr)
            continue
        for name, specifier, _alias in bindings:
            tier, target, conf = resolve_use(
                name, specifier, file, src_crate, src_mod_path, workspace
            )
            counters[tier] = counters.get(tier, 0) + 1
            out.append(
                {
                    "src_file": src_rel,
                    "name": name,
                    "specifier": specifier,
                    "tier": tier,
                    "target_file": to_repo_rel_posix(target, repo_root),
                    "target_kind": None,
                    "alt_count": 0,
                    "confidence": conf,
                }
            )
    return out


# --- main ------------------------------------------------------------------


def main() -> int:
    if len(sys.argv) < 2:
        print("usage: python3 rs_oracle.py <repoPath>", file=sys.stderr)
        return 2
    repo_root = Path(sys.argv[1]).resolve()
    if not repo_root.is_dir():
        print(f"[rs_oracle] not a directory: {repo_root}", file=sys.stderr)
        return 2

    workspace = load_workspace(repo_root)
    # Build mod trees for each crate.
    files_in_scope: list[Path] = []
    for crate_name, crate in workspace["crates"].items():
        entry = crate.get("entry")
        if entry is None:
            crate["tree"] = {}
            continue
        tree = build_mod_tree(entry)
        crate["tree"] = tree
        files_in_scope.extend(tree.values())

    # Dedup file list; some files (e.g. shared inline modules) might appear
    # multiple times across crates.
    seen: set[Path] = set()
    ordered: list[Path] = []
    for f in files_in_scope:
        if f in seen:
            continue
        seen.add(f)
        ordered.append(f)

    out = sys.stdout
    counters: dict[str, int] = {}
    bindings_emitted = 0
    for file in ordered:
        for rec in process_file(file, workspace, repo_root, counters):
            out.write(json.dumps(rec))
            out.write("\n")
            bindings_emitted += 1

    # Crate dict may carry both `tokio-macros` and `tokio_macros` for the
    # same crate; dedupe by crate root for the summary.
    canonical = {}
    for cname, info in workspace["crates"].items():
        canonical.setdefault(info["root"], cname)
    crates_summary = ", ".join(sorted(canonical.values())) or "(none)"
    print(
        f"[rs_oracle] workspace crates: {len(canonical)} "
        f"({crates_summary})",
        file=sys.stderr,
    )
    print(f"[rs_oracle] files scanned:   {len(ordered)}", file=sys.stderr)
    print(f"[rs_oracle] bindings emitted:{bindings_emitted}", file=sys.stderr)
    print(
        f"[rs_oracle] intra-workspace:  {counters.get('ImportScoped', 0)}",
        file=sys.stderr,
    )
    print(
        f"[rs_oracle] external:         {counters.get('External', 0)}",
        file=sys.stderr,
    )
    print(
        f"[rs_oracle] unresolved:       {counters.get('Unresolved', 0)}",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
