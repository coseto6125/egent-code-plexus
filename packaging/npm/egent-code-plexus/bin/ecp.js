#!/usr/bin/env node
"use strict";

// Thin launcher: forward argv + stdio to the prebuilt ecp binary and mirror its
// exit code. spawnSync (not execFileSync) so a non-zero ecp exit is a normal
// result rather than a thrown error — ecp uses exit codes as signal.

const { spawnSync } = require("node:child_process");
const { binaryPath } = require("../index.js");

const result = spawnSync(binaryPath(), process.argv.slice(2), {
  stdio: "inherit",
  windowsHide: true,
});

if (result.error) {
  throw result.error;
}
// Terminated by signal → re-raise so the parent shell observes it.
if (result.signal !== null) {
  process.kill(process.pid, result.signal);
}
process.exit(result.status ?? 1);
