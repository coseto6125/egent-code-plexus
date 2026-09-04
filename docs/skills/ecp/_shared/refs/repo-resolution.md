# Repo and Graph Resolution

`ecp` needs to know which graph file to query. It uses a registry-based lookup by default.

## Preferred: --repo
Pass the path to the repository. `ecp` looks up the branch and hash in its registry and maps it to the correct graph file in `~/.ecp/`.
```bash
ecp <cmd> --repo .
```

## Advanced: --graph
Bypass the registry and point directly to a `graph.bin` file.
```bash
ecp <cmd> --graph .ecp/graph.bin
```

## Multi-repo Selectors
`--repo` takes a registry selector instead of a path on three commands only:
`find --mode bm25`, `summary`, and `contracts`. Everywhere else `--repo` must be
a directory, and a selector is refused rather than silently answered from the
current directory.

- `@all`: Every registered repository.
- `@<group>`: All members of a named group. Top-level commands refuse this one
  and point at `ecp group <verb>`.
- `name1,name2`: Explicit list. A name the registry does not hold is an error,
  not a silently narrower search.
