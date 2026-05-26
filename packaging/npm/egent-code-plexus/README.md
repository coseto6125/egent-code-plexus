# egent-code-plexus (ecp)

Symbol-level code intelligence graph for AI agents and LLMs — multi-language AST analysis with sub-30ms structural queries (who calls X, blast radius, routes, contracts).

This npm package ships the prebuilt `ecp` binary. It carries no native code itself; on install, npm picks the matching `@egent-code-plexus/<platform>` package for your OS/CPU.

## Install

```sh
# Run without installing
npx egent-code-plexus --help

# Or install globally
npm install -g egent-code-plexus
ecp --help
```

Prebuilt binaries are provided for Linux (x64/arm64), macOS (x64/arm64), and Windows (x64). No Rust toolchain or source compile is needed.

## Other install channels

| Channel | Command |
| --- | --- |
| PyPI (uv) | `uvx egent-code-plexus` |
| Homebrew | `brew install coseto6125/tap/egent-code-plexus` |
| Cargo | `cargo install --git https://github.com/coseto6125/egent-code-plexus egent-code-plexus --bin ecp --locked` |

## Documentation

Full docs, language coverage, and query reference: **https://github.com/coseto6125/egent-code-plexus**

## License

MIT OR Apache-2.0
