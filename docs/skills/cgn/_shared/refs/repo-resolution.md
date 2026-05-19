# Repo and Graph Resolution

`cgn` needs to know which graph file to query. It uses a registry-based lookup by default.

## Preferred: --repo
Pass the path to the repository. `cgn` looks up the branch and hash in its registry and maps it to the correct graph file in `~/.cgn/`.
```bash
cgn <cmd> --repo .
```

## Advanced: --graph
Bypass the registry and point directly to a `graph.bin` file.
```bash
cgn <cmd> --graph .cgn/graph.bin
```

## Multi-repo Selectors
- `@all`: Every registered repository.
- `@<group>`: All members of a named group.
- `name1,name2`: Explicit list.
