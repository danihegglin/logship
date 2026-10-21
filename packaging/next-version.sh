#!/usr/bin/env bash
# Prints the tag for the next patch release: the newest v* tag with its patch
# version bumped (v0.2.1 -> v0.2.2), or v0.1.0 if there is none.
# Short tags count as padded (v0.2 is 0.2.0); tags that aren't versions are ignored.
set -euo pipefail

latest=$(git tag --list 'v*' | sed 's/^v//' |
  grep -E '^[0-9]+(\.[0-9]+){0,2}$' |
  sort -t. -k1,1n -k2,2n -k3,3n | tail -n1 || true)

if [[ -z $latest ]]; then
  echo v0.1.0
  exit 0
fi

IFS=. read -r major minor patch <<< "$latest"
echo "v$major.${minor:-0}.$(( ${patch:-0} + 1 ))"
