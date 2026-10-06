# Run: bash ~/.claude/skills/simplify/tests/test_codex_review.sh
# A fake `codex` on PATH plays each case; FAKE_MODE picks the behaviour.
set -u
SCRIPT="$(cd "$(dirname "$0")/.." && pwd)/codex-review.sh"
T=$(mktemp -d); trap 'pkill -f "[f]ake-codex-$$" 2>/dev/null; rm -rf "$T"' EXIT
mkdir -p "$T/bin"
cat > "$T/bin/codex" <<EOF
#!/usr/bin/env bash
# fake-codex-$$
if [ "\$1" = login ]; then [ "\${FAKE_LOGIN:-ok}" = ok ]; exit; fi
prompt="\${!#}"; printf '%s' "\$prompt" > "$T/prompt.seen"
echo "OpenAI Codex banner"; echo "exec: reading files"
case "\$FAKE_MODE" in
  finish)    printf 'hook: Stop\ntokens used\n56,323\nFINDING one\nFINDING two\n' ;;
  linger)    printf 'tokens used\n9\nREPORT after marker\n'; exec -a "fake-codex-$$-linger" sleep 300 ;;
  slowtail)  printf 'tokens used\n9\n'; sleep 2; echo "LATE report line"; sleep 1; echo "LATER line"; exec -a "fake-codex-$$-slow" sleep 300 ;;
  die)       echo "error: stream disconnected"; exit 1 ;;
  hang)      exec -a "fake-codex-$$-hang" sleep 300 ;;
  empty)     printf 'tokens used\n9\n' ;;
  stubborn)  printf 'tokens used\n9\nSTUBBORN report\n'; exec -a "fake-codex-$$-stubborn" bash -c "trap '' TERM; while :; do sleep 1; done" ;;
esac
EOF
chmod +x "$T/bin/codex"
printf 'Review the diff. It said "hi" & $HOME\nline two\n' > "$T/brief.md"
pass=0; fail=0
check() { if [ "$2" = "$3" ]; then pass=$((pass+1)); else fail=$((fail+1)); echo "FAIL $1: want [$3] got [$2]"; fi; }
run() { (cd "$T" && PATH="$T/bin:$PATH" FAKE_MODE="$1" FAKE_LOGIN="${FAKE_LOGIN:-ok}" timeout 40 bash "$SCRIPT" "${@:2}" 2> "$T/stderr"); echo $?; }

test_finish_exits_writes_report_without_marker_or_count() {
  rc=$(run finish brief.md out.md)
  check "${FUNCNAME[0]} rc" "$rc" 0
  check "${FUNCNAME[0]} report" "$(cat "$T/out.md")" "$(printf 'FINDING one\nFINDING two')"
  check "${FUNCNAME[0]} log removed" "$([ -e "$T/out.md.log" ]; echo $?)" 1
}
test_brief_with_quotes_newlines_passes_verbatim() {
  run finish brief.md out.md >/dev/null
  check "${FUNCNAME[0]}" "$(cat "$T/prompt.seen")" "$(cat "$T/brief.md")"
}
test_linger_after_report_returns_and_kills_codex() {
  start=$SECONDS; rc=$(run linger brief.md linger.md)
  check "${FUNCNAME[0]} rc" "$rc" 0
  check "${FUNCNAME[0]} fast" "$(( SECONDS - start < 25 ))" 1
  check "${FUNCNAME[0]} report" "$(cat "$T/linger.md")" "REPORT after marker"
  sleep 1; check "${FUNCNAME[0]} killed" "$(pgrep -fc "[f]ake-codex-$$-linger")" 0
}
test_report_written_after_marker_is_captured() {
  rc=$(run slowtail brief.md slow.md)
  check "${FUNCNAME[0]} rc" "$rc" 0
  check "${FUNCNAME[0]} report" "$(cat "$T/slow.md")" "$(printf 'LATE report line\nLATER line')"
}
test_die_without_marker_exits_2() {
  rc=$(run die brief.md die.md)
  check "${FUNCNAME[0]} rc" "$rc" 2
  check "${FUNCNAME[0]} names log" "$(grep -c 'die.md.log' "$T/stderr")" 1
  check "${FUNCNAME[0]} log kept" "$(grep -c 'stream disconnected' "$T/die.md.log")" 1
}
test_empty_report_after_marker_exits_2() {
  check "${FUNCNAME[0]}" "$(run empty brief.md empty.md)" 2
}
test_hang_past_timeout_exits_3_and_kills_codex() {
  rc=$(run hang brief.md hang.md 3)
  check "${FUNCNAME[0]} rc" "$rc" 3
  check "${FUNCNAME[0]} log kept" "$(grep -c banner "$T/hang.md.log")" 1
  sleep 1; check "${FUNCNAME[0]} killed" "$(pgrep -fc "[f]ake-codex-$$-hang")" 0
}
test_not_logged_in_exits_4_and_empties_old_report() {
  printf 'OLD report\n' > "$T/nl.md"
  check "${FUNCNAME[0]} rc" "$(FAKE_LOGIN=no run finish brief.md nl.md)" 4
  check "${FUNCNAME[0]} emptied" "$(cat "$T/nl.md")" ""
}
test_codex_missing_exits_4() {
  rc=$( (cd "$T" && PATH="/usr/bin:/bin" bash "$SCRIPT" brief.md miss.md 2>/dev/null); echo $?)
  check "${FUNCNAME[0]}" "$rc" 4
}
test_usage_errors_exit_64() {
  : > "$T/blank.md"
  check "${FUNCNAME[0]} no args" "$(run finish)" 64
  check "${FUNCNAME[0]} missing brief" "$(run finish nope.md out.md)" 64
  check "${FUNCNAME[0]} empty brief" "$(run finish blank.md out.md)" 64
  check "${FUNCNAME[0]} bad timeout" "$(run finish brief.md out.md 0)" 64
  check "${FUNCNAME[0]} list timeout" "$(run finish brief.md out.md '5 6')" 64
}
test_report_path_with_dotted_dir_or_log_suffix_keeps_report_and_log_apart() {
  mkdir -p "$T/v1.2"
  check "${FUNCNAME[0]} dotted rc" "$(run finish brief.md v1.2/out)" 0
  check "${FUNCNAME[0]} dotted report" "$(cat "$T/v1.2/out")" "$(printf 'FINDING one\nFINDING two')"
  check "${FUNCNAME[0]} log suffix rc" "$(run finish brief.md r.log)" 0
  check "${FUNCNAME[0]} log suffix report" "$(cat "$T/r.log")" "$(printf 'FINDING one\nFINDING two')"
}
test_unwritable_existing_report_exits_64_and_keeps_old_content() {
  printf 'OLD report\n' > "$T/ro.md"; chmod 444 "$T/ro.md"
  rc=$(run finish brief.md ro.md)
  check "${FUNCNAME[0]} rc" "$rc" 64
  check "${FUNCNAME[0]} old kept" "$(cat "$T/ro.md")" "OLD report"
  chmod 644 "$T/ro.md"
}
test_codex_ignoring_term_is_killed() {
  rc=$(run stubborn brief.md stub.md)
  check "${FUNCNAME[0]} rc" "$rc" 0
  check "${FUNCNAME[0]} report" "$(cat "$T/stub.md")" "STUBBORN report"
  sleep 1; check "${FUNCNAME[0]} killed" "$(pgrep -fc "[f]ake-codex-$$-stubborn")" 0
}
test_parallel_runs_keep_separate_logs() {
  a=$(run finish brief.md pa.md) & b=$(run finish brief.md pb.md); wait
  check "${FUNCNAME[0]} a" "$(cat "$T/pa.md")" "$(printf 'FINDING one\nFINDING two')"
  check "${FUNCNAME[0]} b" "$(cat "$T/pb.md")" "$(printf 'FINDING one\nFINDING two')"
}

for t in $(declare -F | awk '$3 ~ /^test_/ {print $3}'); do "$t"; done
echo "passed $pass, failed $fail"
[ "$fail" -eq 0 ]
