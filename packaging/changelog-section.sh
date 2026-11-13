#!/usr/bin/env bash
# Print the CHANGELOG.md section for a release, without its heading.
#
#   packaging/changelog-section.sh v2.0.0 > notes.md
#
# Fails if CHANGELOG.md has no `## [X.Y.Z]` section for that version, so a
# release can't go out without one.
set -euo pipefail

version="${1:?usage: changelog-section.sh <tag or version>}"
version="${version#v}"
changelog="${2:-CHANGELOG.md}"

notes="$(awk -v v="$version" '
  /^## \[/ { if (found) exit; if (index($0, "## [" v "]") == 1) { found = 1; next } }
  found { print }
' "$changelog")"

if [ -z "$(printf '%s' "$notes" | tr -d '[:space:]')" ]; then
  echo "changelog-section: no section for $version in $changelog" >&2
  exit 1
fi
printf '%s\n' "$notes"
