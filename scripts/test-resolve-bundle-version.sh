#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RESOLVER="$ROOT/scripts/resolve-bundle-version.sh"
ASSERTIONS=0

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

assert_resolves() {
  local input="$1"
  local expected="$2"
  local actual
  actual="$("$RESOLVER" "$input")" || fail "'$input' was unexpectedly rejected"
  [[ "$actual" == "$expected" ]] || fail "'$input' resolved to '$actual', expected '$expected'"
  ASSERTIONS=$((ASSERTIONS + 1))
}

assert_rejected() {
  local input="$1"
  local expected_message="$2"
  local output
  if output="$("$RESOLVER" "$input" 2>&1)"; then
    fail "'$input' unexpectedly resolved to '$output'"
  fi
  [[ "$output" == *"$expected_message"* ]] \
    || fail "'$input' failed without expected message '$expected_message': $output"
  ASSERTIONS=$((ASSERTIONS + 1))
}

version_is_less_than() {
  local left="$1"
  local right="$2"
  local left_major left_minor left_patch
  local right_major right_minor right_patch

  IFS='.' read -r left_major left_minor left_patch <<< "$left"
  IFS='.' read -r right_major right_minor right_patch <<< "$right"
  left_minor="${left_minor:-0}"
  left_patch="${left_patch:-0}"
  right_minor="${right_minor:-0}"
  right_patch="${right_patch:-0}"

  if (( 10#$left_major != 10#$right_major )); then
    (( 10#$left_major < 10#$right_major ))
  elif (( 10#$left_minor != 10#$right_minor )); then
    (( 10#$left_minor < 10#$right_minor ))
  else
    (( 10#$left_patch < 10#$right_patch ))
  fi
}

assert_ordered() {
  local older="$1"
  local newer="$2"
  version_is_less_than "$older" "$newer" \
    || fail "expected CFBundleVersion '$older' to sort below '$newer'"
  ASSERTIONS=$((ASSERTIONS + 1))
}

assert_resolves "0.6.15-pr20" "1000.6.15"
assert_resolves "0.6.15" "1000.6.15"
assert_resolves "0.6.16" "1000.6.16"
assert_resolves "1.2.3" "1001.2.3"
assert_resolves "0.0.0" "1000.0.0"
assert_resolves "0001.08.09-rc.1" "1001.8.9"
assert_resolves "8999.99.99" "9999.99.99"

PR_BUILD="$("$RESOLVER" "0.6.15-pr20")"
RELEASE_BUILD="$("$RESOLVER" "0.6.16")"
assert_ordered "$PR_BUILD" "$RELEASE_BUILD"
assert_ordered "40" "$PR_BUILD"

assert_rejected "" "Usage:"
assert_rejected "1.2" "exactly three numeric components"
assert_rejected "1.2.3.4" "exactly three numeric components"
assert_rejected "v1.2.3" "exactly three numeric components"
assert_rejected "1.two.3" "exactly three numeric components"
assert_rejected "1.2.3-" "suffix must be non-empty"
assert_rejected "1.2.3--pr" "suffix must be non-empty"
assert_rejected "1.2.3-pr/20" "suffix must be non-empty"
assert_rejected "9000.0.0" "major component exceeds 8999"
assert_rejected "1.100.0" "minor component exceeds"
assert_rejected "1.0.100" "patch component exceeds"
assert_rejected "999999999999999999999999.0.0" "major component exceeds 8999"

printf 'PASS: %s resolver assertions\n' "$ASSERTIONS"
