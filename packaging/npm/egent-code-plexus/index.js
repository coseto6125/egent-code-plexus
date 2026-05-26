"use strict";

// Resolve the absolute path to the ecp binary shipped by the platform-specific
// optionalDependency that npm installed for the current os/cpu. esbuild/biome
// pattern: each @egent-code-plexus/<platform> package contains exactly one
// prebuilt binary; this main package carries none, only the dispatch logic.

const { existsSync } = require("node:fs");

// node platform/arch → (npm package suffix, binary filename)
const PLATFORMS = {
  "linux x64": ["linux-x64", "ecp"],
  "linux arm64": ["linux-arm64", "ecp"],
  "darwin x64": ["darwin-x64", "ecp"],
  "darwin arm64": ["darwin-arm64", "ecp"],
  "win32 x64": ["win32-x64", "ecp.exe"],
};

function binaryPath() {
  const key = `${process.platform} ${process.arch}`;
  const entry = PLATFORMS[key];
  if (entry === undefined) {
    throw new Error(
      `egent-code-plexus: unsupported platform ${key}. ` +
        `Prebuilt binaries exist for: ${Object.keys(PLATFORMS).join(", ")}. ` +
        `Install from source instead: cargo install egent-code-plexus`,
    );
  }
  const [suffix, binName] = entry;
  // require.resolve locates the binary inside the installed platform package
  // without hardcoding node_modules layout (handles hoisting / pnpm / workspaces).
  const resolved = require.resolve(`@egent-code-plexus/${suffix}/bin/${binName}`);
  if (!existsSync(resolved)) {
    throw new Error(
      `egent-code-plexus: platform package @egent-code-plexus/${suffix} is ` +
        `installed but its binary is missing at ${resolved}. ` +
        `Reinstall, or check that optionalDependencies were not skipped ` +
        `(npm install --include=optional).`,
    );
  }
  return resolved;
}

module.exports = { binaryPath };
