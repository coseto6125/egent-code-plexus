# Third-Party Notices

`code-graph-nexus` is a derivative work of [GitNexus](https://github.com/abhigyanpatwari/GitNexus)
and bundles tree-sitter grammars as path dependencies under `crates/vendor/`.
This document collects required attribution and license pointers per
[SPDX REUSE](https://reuse.software/) conventions. Canonical license texts
live under `LICENSES/`; per-component LICENSE files are also preserved
in-place inside each vendor directory.

---

## Primary project license

> Required Notice: Copyright Abhigyan Patwari (https://github.com/abhigyanpatwari/GitNexus)

- SPDX-License-Identifier: **LicenseRef-PolyForm-Noncommercial-1.0.0**
- License text: [LICENSE](./LICENSE) (also at [LICENSES/PolyForm-Noncommercial-1.0.0.txt](./LICENSES/PolyForm-Noncommercial-1.0.0.txt))
- Source: derived from `abhigyanpatwari/GitNexus`
- Inherited license is non-negotiable: PolyForm Noncommercial 1.0.0 propagates
  to every new work based on GitNexus, including this Rust reimagination.
  Commercial use is not permitted.

---

## Bundled tree-sitter grammars

Each grammar lives under `crates/vendor/tree-sitter-<lang>/` and retains its
own in-tree LICENSE file.

### tree-sitter-swift

- SPDX-License-Identifier: **MIT**
- Copyright (c) 2021 alex-pinkus
- Source: https://github.com/alex-pinkus/tree-sitter-swift
- In-tree path: `crates/vendor/tree-sitter-swift/`
- License text: [LICENSES/MIT.txt](./LICENSES/MIT.txt)

### tree-sitter-move

- SPDX-License-Identifier: **MIT**
- Copyright per upstream LICENSE
- Source: originally `tree-sitter/tree-sitter-move` (archived); active
  maintenance moved to `MystenLabs/sui` (subpath `external-crates/move/tooling/tree-sitter`)
- In-tree path: `crates/vendor/tree-sitter-move/`
- License text: [LICENSES/MIT.txt](./LICENSES/MIT.txt)

### tree-sitter-nim

- SPDX-License-Identifier: **MPL-2.0**
- Copyright 2023 Leorize <leorize+oss@disroot.org>
- Source: `alaviss/tree-sitter-nim`
- In-tree path: `crates/vendor/tree-sitter-nim/`
- License text: [LICENSES/MPL-2.0.txt](./LICENSES/MPL-2.0.txt)
- Note: MPL-2.0 is file-level copyleft. Modifications to MPL-licensed
  source files must be released under MPL-2.0; pure consumption as a
  dependency does not trigger that obligation.

### tree-sitter-cairo

- SPDX-License-Identifier: **MIT**
- Copyright (c) 2024 tree-sitter-grammars contributors
- Source: ancestor of `tree-sitter-grammars/tree-sitter-cairo` (vendored at
  an earlier minimal state; current upstream has diverged significantly)
- In-tree path: `crates/vendor/tree-sitter-cairo/`
- License text: [LICENSES/MIT.txt](./LICENSES/MIT.txt)

### tree-sitter-vyper

- SPDX-License-Identifier: **MIT**
- Copyright (c) Gustavo Oliveira (per `grammar.js` header)
- Source: personal fork; no public upstream located
- In-tree path: `crates/vendor/tree-sitter-vyper/`
- License text: [LICENSES/MIT.txt](./LICENSES/MIT.txt)

---

## Crates.io transitive dependencies

All 500+ transitive Rust dependencies use OSI-approved permissive
licenses (MIT, Apache-2.0, BSD, ISC, Zlib, MPL-2.0, Unicode-3.0,
CDLA-Permissive-2.0). The allow-list is enforced via [`deny.toml`](./deny.toml)
and verified by `cargo deny check licenses`. Per-crate license texts are
distributed alongside each crate on crates.io; their inclusion in any
downstream build artifact is handled by Cargo and the respective crate
maintainers.

No GPL, AGPL, or LGPL contamination present.
