# Guide: Deep Symbol Analysis

Use this guide when you need to understand the implementation, relationships, and context of a specific code entity (Function, Class, Method, etc.).

## 1. Locate the Symbol
If you know the exact name:
- Use [`cgn find "name"`](../_shared/cli/find.md) to get the definition.

If you only know a fragment:
- Use [`cgn find "fragment" --mode bm25`](../_shared/cli/find.md).

## 2. Inspect Context
Once you have the name:
- Use [`cgn inspect --name <NAME>`](../_shared/cli/inspect.md).
- Read the signature and body to understand the logic.
- Look at `edges` to see what this symbol calls or imports.

## 3. Verify callers
- Check the `callers` section in the `inspect` output to see immediate users.
