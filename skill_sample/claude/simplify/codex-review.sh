#!/usr/bin/env bash
# usage: codex-review.sh <brief-file> <report-file> [timeout-seconds, default 3600]
# Runs one read-only codex review of the repo at $PWD and writes its final report to <report-file>.
# The codex log lands next to it as <report-file>.log and stays only when the run fails.
# Exit: 0 report written · 2 codex ended without a report, or the report could not be written
#       3 no report before the timeout · 4 codex missing or not logged in · 64 usage, or the report path is not writable
# Run it as a background call: it exits when the review ends, so the harness notification is the signal.
set -u
brief=${1:-}; report=${2:-}; limit=${3:-3600}
[ -s "$brief" ] && [ -n "$report" ] && [[ "$limit" =~ ^[1-9][0-9]*$ ]] || { sed -n 2p "$0" >&2; exit 64; }

log="$report.log"
# Empty both before anything can fail: the grep below runs before the background redirect opens the log,
# and a run that stops early must not leave an older report behind. Only exit 0 means the file holds this run's report.
{ : > "$report" && : > "$log"; } 2>/dev/null || { echo "cannot write $report or $log" >&2; exit 64; }
command -v codex >/dev/null || { echo "codex not installed" >&2; exit 4; }
codex login status >/dev/null 2>&1 || { echo "codex not logged in" >&2; exit 4; }
# setsid puts codex in its own process group, so one kill reaches the node wrapper and its native child.
setsid codex exec -m "${CODEX_MODEL:-gpt-6-astra}" -c model_reasoning_effort="medium" \
  --sandbox read-only --skip-git-repo-check -C "$PWD" "$(cat "$brief")" < /dev/null > "$log" 2>&1 &
pid=$!
# A finished codex can stay alive for an hour after its report, so every exit path ends the group.
stop() {
  kill -TERM -- "-$pid" 2>/dev/null || return 0
  for _ in $(seq 1 10); do kill -0 -- "-$pid" 2>/dev/null || return 0; sleep 0.5; done
  kill -KILL -- "-$pid" 2>/dev/null
}
trap stop EXIT
trap 'exit 130' INT TERM

# `tokens used` is printed by the CLI once, when the run finishes; the report follows it.
deadline=$((SECONDS + limit))
until grep -qx 'tokens used' "$log"; do
  kill -0 "$pid" 2>/dev/null || { grep -qx 'tokens used' "$log" && break; echo "codex ended without a report; log: $log" >&2; exit 2; }
  [ "$SECONDS" -lt "$deadline" ] || { echo "codex: no report after ${limit}s; log: $log" >&2; exit 3; }
  sleep 2
done

# The report is written right after the marker: wait until codex exits or the log holds still for 3s.
size=-1; still=0
for _ in $(seq 1 30); do
  kill -0 "$pid" 2>/dev/null || break
  now=$(stat -c %s "$log")
  if [ "$now" = "$size" ]; then still=$((still + 1)); [ "$still" -ge 3 ] && break; else size=$now; still=0; fi
  sleep 1
done

# Skip the marker and the token count on the line after it; keep everything below.
tmp=$(mktemp "$report.XXXXXX") || { echo "cannot write next to $report; log: $log" >&2; exit 2; }
awk 'f == 2 { print } f == 1 { f = 2 } /^tokens used$/ && !f { f = 1 }' "$log" > "$tmp" \
  || { rm -f "$tmp"; echo "cannot extract the report; log: $log" >&2; exit 2; }
[ -s "$tmp" ] || { rm -f "$tmp"; echo "codex finished with an empty report; log: $log" >&2; exit 2; }
mv -f "$tmp" "$report" || { rm -f "$tmp"; echo "cannot write $report; log: $log" >&2; exit 2; }
rm -f "$log"
