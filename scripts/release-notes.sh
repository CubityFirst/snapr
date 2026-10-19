#!/usr/bin/env bash
# Writes a release's notes from the commits that went into it: each commit
# since the previous version tag, oldest first, its subject as a bullet and
# its message (credits included) under it. Merge commits and the version
# bump ("snapr X.Y.Z") are left out, and a link to the full comparison goes
# at the end.
#
#   scripts/release-notes.sh <tag> [<repo, e.g. owner/snapr>]
#
# Needs the history and tags (a full clone, not a shallow one).

set -euo pipefail

if [ $# -lt 1 ] || [ $# -gt 2 ]; then
  echo "usage: $0 <tag> [<owner/repo>]" >&2
  exit 2
fi
tag=$1
repo=${2:-}

prev=$(git describe --tags --abbrev=0 --match 'v*' "$tag^" 2>/dev/null || true)
range=${prev:+$prev..}$tag

# Fields split by unit separators, commits by record separators, so
# messages can hold anything.
git log --reverse --no-merges --format='%h%x1f%s%x1f%b%x1e' "$range" |
  awk -v RS='\x1e' -v FS='\x1f' '
    {
      sub(/^\n+/, "", $1)
      if ($1 == "" || $2 ~ /^snapr [0-9]+\.[0-9]+\.[0-9]+/) next
      printf "- %s (%s)\n", $2, $1
      body = $3
      sub(/\n+$/, "", body)
      if (body != "") {
        n = split(body, lines, "\n")
        printf "\n"
        for (i = 1; i <= n; i++) printf "%s\n", (lines[i] == "" ? "" : "  " lines[i])
        printf "\n"
      }
      count++
    }
    END { if (!count) print "- No changes besides the version." }
  '

if [ -n "$prev" ] && [ -n "$repo" ]; then
  printf '\n**Full Changelog**: https://github.com/%s/compare/%s...%s\n' "$repo" "$prev" "$tag"
fi
