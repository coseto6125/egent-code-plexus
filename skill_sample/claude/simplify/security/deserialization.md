# Deserialization

No Layer-1 rung. Read it when the diff turns bytes from outside into objects.

## Map it first

```bash
ecp pattern -p 'pickle.loads($X)' --lang py
grep -rn "yaml.load(\|eval(\|exec(\|marshal\|shelve" --include=*.py . | grep -v test
ecp impact --target <decode_helper> --direction up
```

## Checks

1. **No format that reconstructs arbitrary objects touches untrusted bytes.** `pickle`, `marshal`, `shelve`, `yaml.load` without `SafeLoader`, Java object streams, PHP `unserialize`. Reaching one of these from a request is remote code execution, not a data bug.
2. **`eval` and `exec` never see caller-supplied text.** Including the indirect forms: a format string compiled at runtime, a template rendered from input, a lambda built from a config value.
3. **Decoding is typed.** A schema-driven decoder that rejects unknown fields turns a mass-assignment bug into a parse error.
4. **Unknown fields are refused where the target is a database row.** A permissive decode plus a permissive update is how `is_admin` gets set by a request body.
5. **Nesting and size have limits.** A deeply nested document exhausts the parser's stack; a large one exhausts memory. Cap both before parsing.
6. **A decode failure is handled.** Malformed input from a webhook or a queue is normal, and an unhandled exception there is an availability bug.
7. **Cached or queued payloads count as untrusted** when anything outside the process can write to the cache or the queue.

## Known-safe — do not report these

| Shape | Why it is fine |
|---|---|
| `pickle` of data the same process wrote to its own cache, unreachable from outside | Confirm the store is not shared or writable elsewhere |
| `yaml.safe_load` | The safe primitive |
| `json.loads` on untrusted bytes | JSON constructs no objects; the risk is what you do with the result |
| A typed decode into a struct with a fixed field set | This is the fix |
| `eval` on a literal the repo authored | Nothing varies; still worth a comment |

## Failure scenario, worked

> A background job read its payload from a cache the web tier also wrote, and unpickled it. An endpoint that let a caller influence a cache key let them place a payload. Precondition: reaching the cache-writing endpoint. Authority obtained: code execution in the worker.
