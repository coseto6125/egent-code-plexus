#!/usr/bin/env node
// TypeScript module-resolution oracle for the resolver verification harness.
//
// Emits one JSONL line per imported binding to stdout, plus a 5-line
// summary on stderr. Schema matches docs/superpowers/specs/
// 2026-05-15-resolver-oracle-harness.md.
//
// Usage:
//   node scripts/oracles/ts_oracle.mjs <repoPath> > oracle.jsonl
//
// Requires: Node >=18 and the `typescript` package resolvable on this host.
// If `typescript` is not in the current project's node_modules we fall back
// to a known-good install under $HOME (see RESOLVE_HINTS below); when even
// that fails we exit non-zero with a clear message.

import path from 'node:path';
import fs from 'node:fs';
import { createRequire } from 'node:module';
import { pathToFileURL } from 'node:url';

// --- locate typescript ------------------------------------------------------

const RESOLVE_HINTS = [
  // Search the corpus first (in case it ships its own typescript), then the
  // worktree root, then a couple of known global installs on this host.
  process.argv[2],
  process.cwd(),
  '/home/enor/agent-dashboard/frontend',
  '/home/enor/enoract/frontend',
  '/home/enor/taiwan_face_pr',
].filter(Boolean);

async function loadTypescript() {
  for (const hint of RESOLVE_HINTS) {
    try {
      const req = createRequire(path.join(path.resolve(hint), 'noop.js'));
      const tsPath = req.resolve('typescript');
      const mod = await import(pathToFileURL(tsPath).href);
      return mod.default ?? mod;
    } catch {
      /* try next hint */
    }
  }
  throw new Error(
    'Could not resolve the `typescript` package. Install it in the corpus ' +
      '(npm i -D typescript) or set NODE_PATH to a directory containing it.',
  );
}

const ts = await loadTypescript();

// --- helpers ----------------------------------------------------------------

const repoRootArg = process.argv[2];
if (!repoRootArg) {
  process.stderr.write('usage: node ts_oracle.mjs <repoPath>\n');
  process.exit(2);
}
const repoRoot = path.resolve(repoRootArg);

const SKIP_DIRS = new Set(['node_modules', 'dist', 'build', '.git', '.next', 'coverage', 'out']);
const TS_EXTS = new Set(['.ts', '.tsx', '.js', '.jsx', '.mjs', '.cjs']);

function toRepoRelPosix(abs) {
  if (abs == null) return null;
  let rel = path.relative(repoRoot, abs);
  if (path.sep !== '/') rel = rel.split(path.sep).join('/');
  if (rel.startsWith('./')) rel = rel.slice(2);
  return rel;
}

function isUnderRepo(abs) {
  if (abs == null) return false;
  const rel = path.relative(repoRoot, abs);
  return rel !== '' && !rel.startsWith('..') && !path.isAbsolute(rel);
}

function* walk(dir) {
  let entries;
  try {
    entries = fs.readdirSync(dir, { withFileTypes: true });
  } catch (err) {
    process.stderr.write(`[ts_oracle] readdir failed for ${dir}: ${err.message}\n`);
    return;
  }
  for (const ent of entries) {
    const full = path.join(dir, ent.name);
    if (ent.isDirectory()) {
      if (SKIP_DIRS.has(ent.name)) continue;
      yield* walk(full);
    } else if (ent.isFile()) {
      const ext = path.extname(ent.name);
      if (TS_EXTS.has(ext)) yield full;
    }
  }
}

// --- tsconfig discovery -----------------------------------------------------
// v1: prefer the root tsconfig.json. If it has `references`, we still only
// load the root one — tsc would split into multiple programs, but for module
// resolution the root options (baseUrl, paths) cover the alias cases that
// matter for our diff. Documented limitation.

function findRootTsconfig() {
  const direct = path.join(repoRoot, 'tsconfig.json');
  if (fs.existsSync(direct)) return direct;
  // Fall back: any tsconfig.json at depth 1.
  const top = fs.readdirSync(repoRoot, { withFileTypes: true });
  for (const ent of top) {
    if (ent.isFile() && /^tsconfig.*\.json$/.test(ent.name)) {
      return path.join(repoRoot, ent.name);
    }
  }
  return null;
}

function loadCompilerOptions(tsconfigPath) {
  if (!tsconfigPath) {
    return {
      options: {
        target: ts.ScriptTarget.ES2020,
        module: ts.ModuleKind.ESNext,
        moduleResolution: ts.ModuleResolutionKind.NodeJs,
        allowJs: true,
      },
    };
  }
  const read = ts.readConfigFile(tsconfigPath, ts.sys.readFile);
  if (read.error) {
    process.stderr.write(
      `[ts_oracle] tsconfig parse error: ${ts.flattenDiagnosticMessageText(read.error.messageText, '\n')}\n`,
    );
    return { options: {} };
  }
  const parsed = ts.parseJsonConfigFileContent(read.config, ts.sys, path.dirname(tsconfigPath));
  return { options: parsed.options };
}

// --- binding enumeration ----------------------------------------------------

function* bindingsFromImport(node) {
  // import "x"        → no clause, side-effect, no bindings.
  // import d from "x" → default
  // import * as ns    → namespace
  // import { a, b as c } from "x" → named
  const clause = node.importClause;
  if (!clause) return;
  if (clause.name) yield clause.name.text; // default
  const nb = clause.namedBindings;
  if (nb) {
    if (ts.isNamespaceImport(nb)) {
      yield nb.name.text;
    } else if (ts.isNamedImports(nb)) {
      for (const el of nb.elements) yield el.name.text; // local name
    }
  }
}

function* bindingsFromExport(node) {
  // export { a, b as c } from "x" → "a", "c"
  // export * from "x"             → skip (no named local binding)
  // export * as ns from "x"       → "ns"
  if (!node.moduleSpecifier) return;
  const ec = node.exportClause;
  if (!ec) return; // export * from "x"
  if (ts.isNamespaceExport(ec)) {
    yield ec.name.text;
  } else if (ts.isNamedExports(ec)) {
    for (const el of ec.elements) yield el.name.text;
  }
}

// --- main -------------------------------------------------------------------

const tsconfigPath = findRootTsconfig();
const { options } = loadCompilerOptions(tsconfigPath);
const host = ts.createCompilerHost(options);
const moduleResolutionCache = ts.createModuleResolutionCache(
  repoRoot,
  (s) => s,
  options,
);

let filesScanned = 0;
let importsFound = 0;
let bindingsEmitted = 0;
let resolvedCount = 0;
let unresolvedCount = 0;

const out = process.stdout;

function emit(rec) {
  out.write(JSON.stringify(rec));
  out.write('\n');
  bindingsEmitted += 1;
  if (rec.tier === 'Unresolved') unresolvedCount += 1;
  else resolvedCount += 1;
}

function processFile(abs) {
  let text;
  try {
    text = fs.readFileSync(abs, 'utf8');
  } catch (err) {
    process.stderr.write(`[ts_oracle] read failed for ${abs}: ${err.message}\n`);
    return;
  }
  const srcRel = toRepoRelPosix(abs);
  let sf;
  try {
    sf = ts.createSourceFile(abs, text, options.target ?? ts.ScriptTarget.ES2020, true);
  } catch (err) {
    process.stderr.write(`[ts_oracle] parse failed for ${srcRel}: ${err.message}\n`);
    return;
  }

  const visit = (node) => {
    try {
      if (
        (ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) &&
        node.moduleSpecifier &&
        ts.isStringLiteral(node.moduleSpecifier)
      ) {
        const specifier = node.moduleSpecifier.text;
        importsFound += 1;

        const res = ts.resolveModuleName(specifier, abs, options, host, moduleResolutionCache);
        const resolvedAbs = res.resolvedModule?.resolvedFileName ?? null;
        const isExternal = res.resolvedModule?.isExternalLibraryImport === true;

        // For node_modules / out-of-repo resolutions: report as resolved but
        // with target_file null — the harness diff key is (src_file, name)
        // and a null target_file outside the repo is not a ghost.
        let targetFile = null;
        let tier = 'Unresolved';
        let confidence = null;
        if (resolvedAbs) {
          if (isExternal || !isUnderRepo(resolvedAbs)) {
            // Resolved to a third-party / outside-repo file. We keep it as
            // "ImportScoped" so the harness sees it as a real resolution,
            // but target_file is null because it's not in the diff scope.
            tier = 'ImportScoped';
            confidence = 1.0;
            targetFile = null;
          } else {
            tier = 'ImportScoped';
            confidence = 1.0;
            targetFile = toRepoRelPosix(resolvedAbs);
          }
        }

        const bindings = ts.isImportDeclaration(node)
          ? Array.from(bindingsFromImport(node))
          : Array.from(bindingsFromExport(node));

        for (const name of bindings) {
          emit({
            src_file: srcRel,
            name,
            specifier,
            tier,
            target_file: targetFile,
            target_kind: null,
            alt_count: 0,
            confidence,
          });
        }
      }
    } catch (err) {
      process.stderr.write(`[ts_oracle] visit error in ${srcRel}: ${err.message}\n`);
    }
    ts.forEachChild(node, visit);
  };

  try {
    ts.forEachChild(sf, visit);
  } catch (err) {
    process.stderr.write(`[ts_oracle] walk error in ${srcRel}: ${err.message}\n`);
  }
}

for (const file of walk(repoRoot)) {
  filesScanned += 1;
  processFile(file);
}

process.stderr.write(`[ts_oracle] tsconfig:        ${tsconfigPath ?? '(none, defaults)'}\n`);
process.stderr.write(`[ts_oracle] files scanned:   ${filesScanned}\n`);
process.stderr.write(`[ts_oracle] imports found:   ${importsFound}\n`);
process.stderr.write(`[ts_oracle] bindings emitted:${bindingsEmitted}\n`);
process.stderr.write(`[ts_oracle] resolved:        ${resolvedCount}\n`);
process.stderr.write(`[ts_oracle] unresolved:      ${unresolvedCount}\n`);
