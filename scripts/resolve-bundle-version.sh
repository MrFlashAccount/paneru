#!/usr/bin/env bash
set -euo pipefail

# Convert Paneru's human-facing semantic version into Sparkle's monotonically
# ordered CFBundleVersion. The epoch keeps all mapped builds above historical
# run-number builds while preserving semantic ordering.

if [[ "$#" -ne 1 || -z "$1" ]]; then
  echo "Usage: $0 <major.minor.patch[-suffix]>" >&2
  exit 1
fi

VERSION="$1"
BASE_VERSION="$VERSION"

if [[ "$VERSION" == *-* ]]; then
  BASE_VERSION="${VERSION%%-*}"
  SUFFIX="${VERSION#*-}"
  if [[ ! "$SUFFIX" =~ ^[0-9A-Za-z]+([.-][0-9A-Za-z]+)*$ ]]; then
    echo "Invalid Paneru version '$VERSION': suffix must be non-empty and contain only non-empty alphanumeric components separated by '.' or '-'." >&2
    exit 1
  fi
fi

if [[ ! "$BASE_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "Invalid Paneru version '$VERSION': base version must contain exactly three numeric components (major.minor.patch)." >&2
  exit 1
fi

IFS='.' read -r MAJOR MINOR PATCH <<< "$BASE_VERSION"

normalize_decimal() {
  local value="$1"
  while [[ "${#value}" -gt 1 && "${value:0:1}" == "0" ]]; do
    value="${value:1}"
  done
  printf '%s\n' "$value"
}

MAJOR="$(normalize_decimal "$MAJOR")"
MINOR="$(normalize_decimal "$MINOR")"
PATCH="$(normalize_decimal "$PATCH")"

# Apple limits the three CFBundleVersion components to 4, 2, and 2 digits.
# The +1000 epoch therefore leaves semantic major versions 0 through 8999.
if [[ "${#MAJOR}" -gt 4 ]] || (( 10#$MAJOR > 8999 )); then
  echo "Invalid Paneru version '$VERSION': major component exceeds 8999, the maximum supported by the CFBundleVersion epoch mapping." >&2
  exit 1
fi
if [[ "${#MINOR}" -gt 2 ]] || (( 10#$MINOR > 99 )); then
  echo "Invalid Paneru version '$VERSION': minor component exceeds Apple's CFBundleVersion maximum of 99." >&2
  exit 1
fi
if [[ "${#PATCH}" -gt 2 ]] || (( 10#$PATCH > 99 )); then
  echo "Invalid Paneru version '$VERSION': patch component exceeds Apple's CFBundleVersion maximum of 99." >&2
  exit 1
fi

printf '%s.%s.%s\n' "$((1000 + 10#$MAJOR))" "$((10#$MINOR))" "$((10#$PATCH))"
