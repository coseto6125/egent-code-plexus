#!/usr/bin/env bash
# Source-only file. Provides assert_* helpers + tmpdir management for SKILL tests.
# Use:
#   source "$(dirname "$0")/test-helpers.sh"
#   tmp=$(mktemp_test_dir)
#   ... run things ...
#   pass

set -euo pipefail

mktemp_test_dir() {
    local dir
    dir=$(mktemp -d -t "skill-test-XXXXXX")
    # shellcheck disable=SC2064
    trap "rm -rf '$dir'" EXIT
    echo "$dir"
}

assert_equal() {
    local expected="$1" actual="$2" label="${3:-values}"
    if [[ "$expected" != "$actual" ]]; then
        echo "FAIL: $label" >&2
        echo "  expected: $expected" >&2
        echo "  actual:   $actual" >&2
        exit 1
    fi
}

assert_file_exists() {
    local path="$1"
    if [[ ! -f "$path" ]]; then
        echo "FAIL: file not found: $path" >&2
        exit 1
    fi
}

assert_grep() {
    local pattern="$1" file="$2"
    if ! grep -qE "$pattern" "$file"; then
        echo "FAIL: pattern '$pattern' not found in $file" >&2
        exit 1
    fi
}

assert_no_grep() {
    local pattern="$1" file="$2"
    if grep -qE "$pattern" "$file"; then
        echo "FAIL: pattern '$pattern' unexpectedly found in $file" >&2
        exit 1
    fi
}

assert_exit() {
    local expected_code="$1"; shift
    local actual_code=0
    "$@" || actual_code=$?
    assert_equal "$expected_code" "$actual_code" "exit code of: $*"
}

pass() {
    echo "PASS: ${BASH_SOURCE[1]##*/}"
}
