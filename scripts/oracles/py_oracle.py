#!/usr/bin/env python3
"""Python module-resolution oracle for the GitNexus resolver verification harness.

Emits one JSONL line per imported binding to stdout, with a 5-line summary on
stderr. Contract documented in
``docs/superpowers/specs/2026-05-15-resolver-oracle-harness.md``.

Usage:
    python3 scripts/oracles/py_oracle.py <repoPath> > oracle.jsonl

Resolution strategy:
    * Parse each ``*.py`` / ``*.pyi`` file with ``ast``.
    * For each ``Import`` / ``ImportFrom`` node emit a binding line.
    * Resolve the target using ``importlib.util.find_spec`` against a sys.path
      built from the inferred package root (src/ layout or repo root).
    * Relative imports use the source file's derived package context.

Edge case explicitly NOT solved in v1 (per spec):
    ``__init__`` re-export chains. If ``foo/__init__.py`` does
    ``from .bar import X`` and a consumer writes ``from foo import X``,
    ``find_spec("foo")`` returns ``foo/__init__.py`` not ``foo/bar.py``. That
    matches Python's import semantics; the diff harness handles this via the
    ``a/b.py == a/b/__init__.py`` extension-equivalence rule.
"""

from __future__ import annotations

import ast
import importlib.machinery
import importlib.util
import json
import sys
from pathlib import Path
from typing import Any, Iterator

SKIP_DIRS: frozenset[str] = frozenset({
    ".venv",
    "venv",
    "env",
    "__pycache__",
    ".git",
    "dist",
    "build",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    "node_modules",
})

SOURCE_SUFFIXES: tuple[str, ...] = (".py", ".pyi")


def find_package_root(repo_root: Path) -> Path:
    """Pick the directory that should sit at the head of sys.path.

    Heuristic:
      * If ``<repo>/src`` exists and contains at least one entry → ``src/``.
      * Otherwise → ``repo_root`` itself (flat layout).
    """
    src = repo_root / "src"
    if src.is_dir() and any(src.iterdir()):
        return src
    return repo_root


def iter_source_files(repo_root: Path) -> Iterator[Path]:
    """Yield every ``.py`` / ``.pyi`` file under ``repo_root``, pruning skip dirs."""
    stack: list[Path] = [repo_root]
    while stack:
        current = stack.pop()
        try:
            entries = list(current.iterdir())
        except (PermissionError, OSError):
            continue
        for entry in entries:
            name = entry.name
            if entry.is_dir():
                if name in SKIP_DIRS or name.endswith(".egg-info"):
                    continue
                stack.append(entry)
            elif entry.is_file() and entry.suffix in SOURCE_SUFFIXES:
                yield entry


def derive_package_context(src_file: Path, pkg_root: Path) -> str | None:
    """Compute the dotted package name that contains ``src_file``.

    e.g. ``src/myproj/utils/helpers.py`` with pkg_root ``src/`` → ``myproj.utils``.
    Returns ``None`` if the file is not under pkg_root (we still process it,
    but relative imports won't resolve).
    """
    try:
        rel = src_file.resolve().relative_to(pkg_root.resolve())
    except ValueError:
        return None
    parts = list(rel.parts)
    if not parts:
        return None
    # Drop the filename. If it's __init__.py(.pyi), the file IS the package.
    stem = src_file.stem
    if stem == "__init__":
        package_parts = parts[:-1]
    else:
        package_parts = parts[:-1]
    return ".".join(package_parts) if package_parts else None


def repo_relative(path: Path | None, repo_root: Path) -> str | None:
    """Return repo-relative POSIX path; absolute POSIX if outside repo."""
    if path is None:
        return None
    try:
        resolved = path.resolve()
    except OSError:
        return path.as_posix()
    try:
        return resolved.relative_to(repo_root).as_posix()
    except ValueError:
        return resolved.as_posix()


def spec_origin(spec: importlib.machinery.ModuleSpec | None) -> Path | None:
    """Extract a filesystem Path from a ModuleSpec, or ``None`` for namespace pkgs / builtins."""
    if spec is None:
        return None
    origin = spec.origin
    if origin in (None, "built-in", "frozen"):
        # Namespace packages have origin=None but submodule_search_locations set.
        # We treat those as unresolved (no single file to attribute to).
        return None
    try:
        return Path(origin)
    except (TypeError, ValueError):
        return None


def resolve_specifier(
    specifier: str,
    level: int,
    package_context: str | None,
    finder: importlib.machinery.PathFinder,
    search_path: list[str],
) -> Path | None:
    """Resolve a possibly-relative module specifier to a file Path.

    ``specifier`` is the textual module path from the AST (may be empty for
    ``from . import x``). ``level`` is the dot count (0 for absolute imports).
    """
    if level > 0:
        # Relative import. Need package_context to anchor.
        if package_context is None:
            return None
        try:
            # importlib.util.resolve_name expects ".name" / "..name" form.
            rel_name = ("." * level) + (specifier or "")
            full_name = importlib.util.resolve_name(rel_name, package_context)
        except (ImportError, ValueError):
            return None
    else:
        full_name = specifier
        if not full_name:
            return None

    try:
        spec = finder.find_spec(full_name, search_path)
    except (ImportError, ModuleNotFoundError, ValueError, AttributeError):
        return None
    if spec is not None:
        return spec_origin(spec)

    # Fall back to the global finder for stdlib / installed packages.
    try:
        spec = importlib.util.find_spec(full_name)
    except (ImportError, ModuleNotFoundError, ValueError, AttributeError):
        return None
    return spec_origin(spec)


def make_record(
    src_file_rel: str,
    name: str,
    specifier: str,
    target: Path | None,
    repo_root: Path,
) -> dict[str, Any]:
    """Build a single JSONL record. ``target=None`` ⇒ Unresolved."""
    if target is None:
        return {
            "src_file": src_file_rel,
            "name": name,
            "specifier": specifier,
            "tier": "Unresolved",
            "target_file": None,
            "target_kind": None,
            "alt_count": 0,
            "confidence": None,
        }
    return {
        "src_file": src_file_rel,
        "name": name,
        "specifier": specifier,
        "tier": "ImportScoped",
        "target_file": repo_relative(target, repo_root),
        "target_kind": None,
        "alt_count": 0,
        "confidence": 1.0,
    }


def emit_bindings_for_file(
    src_file: Path,
    repo_root: Path,
    pkg_root: Path,
    finder: importlib.machinery.PathFinder,
    out_writer: Any,
) -> tuple[int, int, int, int]:
    """Parse ``src_file`` and emit JSONL lines. Returns (imports, bindings, intra, ext, unresolved) counters.

    Actually returns a 4-tuple (imports_seen, bindings_emitted, resolved_intra,
    resolved_external) and unresolved is derivable, but we pack only what we
    need.
    """
    try:
        source = src_file.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return (0, 0, 0, 0)
    try:
        tree = ast.parse(source, filename=str(src_file))
    except SyntaxError:
        return (0, 0, 0, 0)

    package_context = derive_package_context(src_file, pkg_root)
    # sys.path-equivalent search: source file's own dir first, then pkg root.
    search_path: list[str] = [str(src_file.parent), str(pkg_root)]
    src_file_rel = repo_relative(src_file, repo_root) or src_file.as_posix()
    repo_root_resolved = repo_root.resolve()

    imports_seen = 0
    bindings = 0
    intra = 0
    external = 0

    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            imports_seen += 1
            for alias in node.names:
                specifier = alias.name  # e.g. "x.y.z"
                # `import x.y.z` binds top-level "x"; `import x.y as q` binds "q".
                if alias.asname:
                    bound_name = alias.asname
                else:
                    bound_name = specifier.split(".", 1)[0]
                target = resolve_specifier(specifier, 0, package_context, finder, search_path)
                record = make_record(src_file_rel, bound_name, specifier, target, repo_root_resolved)
                out_writer.write(json.dumps(record, ensure_ascii=False))
                out_writer.write("\n")
                bindings += 1
                if target is None:
                    pass
                else:
                    try:
                        target.resolve().relative_to(repo_root_resolved)
                        intra += 1
                    except ValueError:
                        external += 1
        elif isinstance(node, ast.ImportFrom):
            imports_seen += 1
            specifier_module = node.module or ""
            level = node.level or 0
            # Build the textual specifier used in JSON output.
            if level > 0:
                specifier_text = ("." * level) + specifier_module
            else:
                specifier_text = specifier_module
            target = resolve_specifier(
                specifier_module, level, package_context, finder, search_path
            )
            for alias in node.names:
                bound_name = alias.asname if alias.asname else alias.name
                record = make_record(
                    src_file_rel, bound_name, specifier_text, target, repo_root_resolved
                )
                out_writer.write(json.dumps(record, ensure_ascii=False))
                out_writer.write("\n")
                bindings += 1
                if target is None:
                    pass
                else:
                    try:
                        target.resolve().relative_to(repo_root_resolved)
                        intra += 1
                    except ValueError:
                        external += 1

    return (imports_seen, bindings, intra, external)


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        sys.stderr.write("usage: py_oracle.py <repo_path>\n")
        return 2
    repo_root = Path(argv[1]).resolve()
    if not repo_root.is_dir():
        sys.stderr.write(f"error: not a directory: {repo_root}\n")
        return 2

    pkg_root = find_package_root(repo_root)
    # Prepend pkg_root to sys.path so importlib can locate intra-repo packages
    # without polluting whatever installed copy the user may have.
    pkg_root_str = str(pkg_root)
    if pkg_root_str not in sys.path:
        sys.path.insert(0, pkg_root_str)
    repo_root_str = str(repo_root)
    if pkg_root_str != repo_root_str and repo_root_str not in sys.path:
        sys.path.insert(1, repo_root_str)

    finder = importlib.machinery.PathFinder()

    files_scanned = 0
    imports_total = 0
    bindings_total = 0
    intra_total = 0
    external_total = 0

    out = sys.stdout
    for src_file in iter_source_files(repo_root):
        files_scanned += 1
        imports, bindings, intra, external = emit_bindings_for_file(
            src_file, repo_root, pkg_root, finder, out
        )
        imports_total += imports
        bindings_total += bindings
        intra_total += intra
        external_total += external

    unresolved_total = bindings_total - intra_total - external_total
    sys.stderr.write(f"files scanned: {files_scanned}\n")
    sys.stderr.write(f"imports found: {imports_total}\n")
    sys.stderr.write(f"bindings emitted: {bindings_total}\n")
    sys.stderr.write(f"resolved (intra-repo): {intra_total}\n")
    sys.stderr.write(f"resolved (external): {external_total}\n")
    sys.stderr.write(f"unresolved: {unresolved_total}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
