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
- `@all`: Every registered repository.
- `@<group>`: All members of a named group.
- `name1,name2`: Explicit list.
