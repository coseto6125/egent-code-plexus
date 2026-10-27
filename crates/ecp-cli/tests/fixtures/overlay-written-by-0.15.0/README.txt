Session overlay files written by the released ecp 0.15.0 (`ecp --version`:
`ecp 0.15.0+8f7e3c3`, installed at ~/.local/bin/ecp) for one dirty file.

Files
  app.py                          the dirty worktree file, as the session saw it
  dirty_files.json                the session manifest (format 3,
                                  builder_fingerprint "v0.15.0+schema11")
  graph_overlay/e4bdd84a3e2d78f5.bin
                                  the v3 fragment of app.py: helper (line 1),
                                  go (line 5), added (line 9)

The tests copy app.py into a fresh worktree and move the manifest's
`mtime_ns` to the copy's mtime, so the mtime check alone would accept it.
Today's reader opens `<session>/l1/` only and never `dirty_files.json` or
`graph_overlay/`, so the test copies these files into the legacy namespace and
expects the reader to serve no input from them.

How it was made (2026-10-22, bash, fresh dirs from mktemp, never ~/.ecp)
  R=$(mktemp -d <scratchpad>/ovl015.XXXXXX); H=$(mktemp -d <scratchpad>/ovl015home.XXXXXX)
  cd $R
  printf 'def helper():\n    return 1\n\n\ndef go():\n    return helper()\n' > app.py
  printf 'from app import go\n\n\ndef main():\n    go()\n' > main.py
  git init -q && git add -A && git -c user.email=t@t -c user.name=t commit -qm init
  unset CLAUDE_CODE_SESSION_ID
  export ECP_HOME=$H ECP_SKIP_BG_REBUILD=1 ECP_NO_UPDATE_CHECK=1
  ~/.local/bin/ecp admin index --repo .
  printf 'def helper():\n    return 1\n\n\ndef go():\n    return added()\n\n\ndef added():\n    return helper()\n' > app.py
  CLAUDE_CODE_SESSION_ID=ovl015-fixture ~/.local/bin/ecp find added --repo .
      -> l1.refreshed written=1 failed=0
  cp $H/<repo_dir>/sessions/ovl015-fixture/dirty_files.json .
  cp $H/<repo_dir>/sessions/ovl015-fixture/graph_overlay/e4bdd84a3e2d78f5.bin graph_overlay/
  cp $R/app.py .

The session's session_meta.json and overlay.lock files are left out: the
tests build their own session_meta.json.
