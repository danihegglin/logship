#!/usr/bin/env bash
# Stamps a release version into Cargo.toml and Cargo.lock so the binary and
# every package report the version from the git tag.
# Usage: packaging/set-version.sh <version>
set -euo pipefail

version=$1
root=$(cd "$(dirname "$0")/.." && pwd)

perl -0pi -e "s/^version = \"[^\"]*\"/version = \"$version\"/m" "$root/Cargo.toml"
perl -0pi -e "s/(name = \"logship\"\r?\nversion = )\"[^\"]*\"/\${1}\"$version\"/" "$root/Cargo.lock"

grep -q "^version = \"$version\"" "$root/Cargo.toml"
grep -A1 '^name = "logship"' "$root/Cargo.lock" | grep -q "version = \"$version\""
