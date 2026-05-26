# egent-code-plexus (ecp)

Symbol-level code intelligence graph for AI agents and LLMs. Sub-30ms queries
answering "who calls X / what breaks if I change Y", across 30+ tree-sitter
languages. The `ecp` CLI is a native Rust binary; this PyPI package ships the
prebuilt binary for your platform — no Rust toolchain required.

## Install

```sh
# one-off run, no install
uvx egent-code-plexus --help

# install as a tool
uv tool install egent-code-plexus
ecp --version
```

`pipx install egent-code-plexus` works too.

## Usage

```sh
ecp find <symbol>                              # locate a definition
ecp impact --target <symbol> --direction upstream   # who calls it
ecp routes                                     # API route map
```

See the [project README](https://github.com/coseto6125/egent-code-plexus) for
the full command surface.
