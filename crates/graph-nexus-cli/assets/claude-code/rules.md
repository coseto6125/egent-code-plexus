gnx index: {{stats.nodes}} symbols, {{stats.edges}} rels at HEAD {{head}}.
Use `gnx inspect <name>` for symbol context, `gnx find <name>` for
exact-name definition lookup (add `--mode fuzzy` for substring or
`--mode bm25` for BM25 ranking), `gnx impact <name>` for blast radius.
{{#if graphify}}
graphify-out/ available — use that for narrative architecture context.
{{/if}}
{{#if wiki}}
graphify-out/wiki/index.md is the entry point for the indexed wiki.
{{/if}}
