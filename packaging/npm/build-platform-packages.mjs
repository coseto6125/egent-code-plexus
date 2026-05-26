#!/usr/bin/env node
// Assemble the npm packages for a release from the prebuilt release tarballs:
// the main `egent-code-plexus` package plus the 5 @egent-code-plexus/<platform>
// packages, each ready for `npm publish`. Run after the GitHub Release build job.
//
//   node build-platform-packages.mjs --version 0.5.0 --artifacts <dir> --out <dir>
//
// <artifacts> holds the extracted release tarballs, one dir per target:
//   ecp-v0.5.0-x86_64-unknown-linux-gnu/ecp
//   ecp-v0.5.0-x86_64-pc-windows-msvc/ecp.exe
// <out> receives publishable package dirs: <out>/egent-code-plexus (main) and
// <out>/<platform> for each platform.
//
// Versions are stamped from --version (the git tag), never read from committed
// files — the committed package.json carries the __VERSION__ placeholder so it
// can't drift out of sync with the release tag.

import { readFileSync, writeFileSync, mkdirSync, copyFileSync, chmodSync, cpSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));

// platform npm-suffix → (rust target, npm os, npm cpu, binary filename)
const MATRIX = [
  ["linux-x64", "x86_64-unknown-linux-gnu", "linux", "x64", "ecp"],
  ["linux-arm64", "aarch64-unknown-linux-gnu", "linux", "arm64", "ecp"],
  ["darwin-x64", "x86_64-apple-darwin", "darwin", "x64", "ecp"],
  ["darwin-arm64", "aarch64-apple-darwin", "darwin", "arm64", "ecp"],
  ["win32-x64", "x86_64-pc-windows-msvc", "win32", "x64", "ecp.exe"],
];

function parseArgs(argv) {
  const out = {};
  for (let i = 0; i < argv.length; i += 2) {
    if (!argv[i].startsWith("--")) throw new Error(`expected flag, got ${argv[i]}`);
    out[argv[i].slice(2)] = argv[i + 1];
  }
  return out;
}

const { version, artifacts, out } = parseArgs(process.argv.slice(2));
if (!version || !artifacts || !out) {
  throw new Error("required: --version <v> --artifacts <dir> --out <dir>");
}

// Main package: copy sources, stamp version into package.json.
const mainOut = join(out, "egent-code-plexus");
cpSync(join(HERE, "egent-code-plexus"), mainOut, { recursive: true });
const mainManifest = readFileSync(join(mainOut, "package.json"), "utf8").replaceAll("__VERSION__", version);
writeFileSync(join(mainOut, "package.json"), mainManifest);
console.log(`assembled egent-code-plexus -> ${mainOut}`);

// Platform packages: one prebuilt binary each, version + os/cpu stamped in.
const template = readFileSync(join(HERE, "platform", "template", "package.json"), "utf8");
const readme = readFileSync(join(HERE, "platform", "README.md"), "utf8");

for (const [suffix, target, os, cpu, bin] of MATRIX) {
  const pkgDir = join(out, suffix);
  const binDir = join(pkgDir, "bin");
  mkdirSync(binDir, { recursive: true });

  const srcBin = join(artifacts, `ecp-v${version}-${target}`, bin);
  const dstBin = join(binDir, bin);
  copyFileSync(srcBin, dstBin);
  if (os !== "win32") chmodSync(dstBin, 0o755);

  const manifest = template
    .replaceAll("__PLATFORM__", suffix)
    .replaceAll("__OS__", os)
    .replaceAll("__CPU__", cpu)
    .replaceAll("__BIN__", bin)
    .replaceAll("__VERSION__", version);
  writeFileSync(join(pkgDir, "package.json"), manifest);
  writeFileSync(join(pkgDir, "README.md"), readme.replaceAll("__PLATFORM__", suffix));

  console.log(`assembled @egent-code-plexus/${suffix} -> ${pkgDir}`);
}
