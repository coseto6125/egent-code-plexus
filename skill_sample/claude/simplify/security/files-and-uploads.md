# Files and uploads

No Layer-1 rung. Read it when the diff opens a path built from input, accepts an upload, or expands an archive.

## Map it first

```bash
ecp pattern -p 'open($P)' --lang py
grep -rn "zipfile\|tarfile\|shutil.unpack\|request.files\|UploadFile" --include=*.py .
ecp impact --target <storage_helper> --direction up
```

## Checks

1. **A path built from input is resolved, then confined.** Join, resolve to absolute, and assert the result stays under the root. Substring checks for `..` miss encoded, absolute and symlinked forms.
2. **The stored name is generated, not accepted.** Keep the caller's filename as a display attribute; store under an id.
3. **Type comes from content, not from the name or the declared content type.** Both are caller-controlled.
4. **Size is capped before the bytes are buffered**, at the framework or proxy layer, so a large upload is refused rather than absorbed.
5. **An archive is expanded with a member-count, a total-size and a per-path check.** An entry named `../../etc/x` writes there; a small archive can expand to fill a disk.
6. **Uploaded content is served from a path that cannot execute it**, and with `Content-Disposition: attachment` plus a fixed content type where the file is user-supplied.
7. **A temporary file is created with the library's secure primitive** and removed on the failure path too.
8. **Symlinks are not followed** when walking a caller-supplied tree.

## Known-safe — do not report these

| Shape | Why it is fine |
|---|---|
| `open(SETTINGS.LOG_PATH)` or any path from config | Server-controlled |
| `open(BASE / filename)` where `filename` comes from a database id the server generated | Confirm the id is server-generated, then leave it |
| An upload whose type is checked by content and whose name is regenerated | This is the fix |
| `tempfile.NamedTemporaryFile` | The secure primitive |
| An archive expanded from a build artefact the repo produced | Not caller-supplied |

## Failure scenario, worked

> An ingest endpoint stored uploads under the caller's filename, joined to a per-tenant directory. A name of `../../<other-tenant>/kb/doc.md` overwrote another tenant's document. Precondition: any org member. Authority obtained: write into a neighbouring tenant's corpus, which the model then quotes.
