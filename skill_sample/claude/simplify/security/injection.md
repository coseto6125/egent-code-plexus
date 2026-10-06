# Query and command construction

Deepens rung 9. Read it when the diff builds a query, a command line, a path, or a template from a value it did not write itself.

## Map it first

```bash
ecp tool-map --category db                              # every database call site
ecp pattern -p 'f"SELECT $$$A"' --lang py
ecp pattern -p 'subprocess.run($$$A)' --lang py
ecp pattern -p 'f"MATCH $$$A"' --lang py                # graph query languages count
```

## Checks

1. **Values reach the driver as parameters.** `$1`, `%s`, `?` — the driver's own placeholder, with the value passed alongside. String building with an f-string, `+`, `%` or `.format` is the finding, regardless of how the value looks today.
2. **Identifiers cannot be parameterized, so they are allowlisted.** A table name, a column name, an `ORDER BY` direction, a schema: match against a fixed set, never interpolate.
3. **Query languages other than SQL count.** Cypher, JSONPath, LDAP filters, XPath, MongoDB operators. Some have no escaping mechanism at all, in which case the allowlist is the only control and a comment should say so.
4. **A shell is not used where a process would do.** `subprocess.run([...])` with a list argv and no `shell=True`. A single string with `shell=True` is a shell injection whenever any part of it varies.
5. **Paths are resolved and then confined.** Join, resolve to an absolute path, and confirm it stays under the intended root. Rejecting `..` by substring misses encoded and symlinked forms.
6. **Template engines render, they do not compile.** User input is a template *variable*, never part of the template *source*.
7. **The type of the parameter matters.** Some drivers reject or mis-handle a type the schema does not expect, turning a data bug into a whole-batch failure. Bind the type the column holds.

## Known-safe — do not report these

| Shape | Why it is fine |
|---|---|
| `cursor.execute("... WHERE id = $1", [value])` | Parameterized; the f-string ban is about the query text, not the call |
| An f-string that interpolates only a module-level constant or an enum member | Nothing varies with input; confirm the name is not rebound elsewhere |
| An ORM filter such as `.filter(id=value)` | The ORM parameterizes. Flag only `.raw()`, `.extra()`, or a raw fragment |
| `subprocess.run([...])` with a list and no shell | The argv form does not involve a shell |
| A hand-built query in a migration or a one-off script with no caller-supplied value | No input crosses the boundary |
| An f-string building a `LIMIT` from an integer already range-checked | Say so; a validated integer is not injectable |

## Failure scenario, worked

> A search filter built a Cypher `CONTAINS` clause by interpolating a keyword list the model produced. The query language offered no quote escaping, so a keyword containing a quote either broke the query or extended it. Precondition: any chat message. Authority obtained: arbitrary read across the tenant's graph.
