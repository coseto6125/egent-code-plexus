# Translating the README

This directory holds non-English variants of the project README. The English one at the repo root is the source of truth; every other locale follows its structure 1-for-1.

## Adding or improving a translation

1. **Pick a locale code.** Use the [BCP-47](https://www.rfc-editor.org/info/bcp47) short form — `es`, `ru`, `ja`, `ko`, `hi`, `pt-BR`, etc. For Chinese variants we already split `zh-TW` and `zh-CN`.
2. **Copy the structure.** Mirror the section order from `README.md` exactly. The brutalist hero box, badges, metrics block, code blocks, tables, and `<details>` wrappers stay in English — only prose and table-cell descriptions get translated.
3. **Keep technical terms in English** when the native rendering would be awkward (e.g., `mmap`, `BlindSpot`, `BFS`, `TOON`, `MCP`, all flag names like `--mode bm25`). The README is a developer doc; precision beats native-vocabulary purity.
4. **Update the language picker** in every other locale file when you add a new one. The line right under the metrics block lists every available locale, with the current locale shown as `**bold**` (no link).
5. **Internal links must resolve.** Translation files live at `docs/readme_i18n/<file>.md`, so `../../LICENSE.md`, `../language-matrix.md`, and `./README_xx.md` are the right relative paths.

## Quality bar

We accept best-effort translations; we don't require certified-translator quality. What we do require:

- The 4 numbers in the metrics block stay accurate (cold index 2.60 s, query p50 142 ms, 31 languages, edge policy).
- The head-to-head + scalability tables stay numerically identical to the English version.
- The CLI command names, flag names, and JSON snippets stay byte-for-byte identical.
- The `[badges]` block stays identical across all locales.

If you spot a translation that says something different from the English source — open an issue or PR with the diff. Translations drift; they need contributors to keep them honest.

## Adding a brand-new locale

1. Create `docs/readme_i18n/README_<locale>.md` mirroring the English structure.
2. Insert the locale into the picker line in **every other README file** (root + each `docs/readme_i18n/README_*.md`).
3. (Optional) update [`README.md`](../../README.md)'s social card / GitHub repo description if you want the locale advertised at the project level.

## Removing a locale

If a translation goes stale and no one is maintaining it, prefer **removing it** over leaving a misleading out-of-date version online. Open a PR that:

1. Deletes `README_<locale>.md`.
2. Removes the locale from every picker line.
3. Notes in the PR body which sections diverged from English at time of removal, so a future contributor can resurrect the translation if they want.
