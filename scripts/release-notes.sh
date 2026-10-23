#!/usr/bin/env bash
# Writes a release's notes from the commits that went into it: each commit
# since the previous version tag, oldest first, grouped by its Conventional
# Commits type (see CONTRIBUTING.md), its description as a bullet. The
# message body isn't included. Merge commits and the version bump are left
# out, and a link to the full comparison goes at the end.
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

# One section per group, in this order.
titles=("New" "Fixes" "Faster" "Other changes")
sections=("" "" "" "")

# A change reverted in the same release never shipped: both it and its
# revert ("This reverts commit <hash>" or "... of <hash>" in the message)
# are left out.
skip=" "
while IFS= read -r hash; do
  subject=$(git log -1 --format=%s "$hash")
  [[ $subject =~ ^[Rr]evert ]] || continue
  reverted=$(git log -1 --format=%b "$hash" |
    sed -nE 's/.*This reverts .*\b([0-9a-f]{7,40})\b.*/\1/p' | head -1)
  [ -n "$reverted" ] || continue
  full=$(git rev-parse --verify --quiet "$reverted^{commit}") || continue
  if git merge-base --is-ancestor "$full" "$tag" &&
    { [ -z "$prev" ] || ! git merge-base --is-ancestor "$full" "$prev"; }; then
    skip+="$hash $full "
  fi
done < <(git rev-list --no-merges "$range")

while IFS= read -r hash; do
  [[ $skip == *" $hash "* ]] && continue
  subject=$(git log -1 --format=%s "$hash")
  # The version bump: chore(release), or a bare "snapr X.Y.Z" before
  # Conventional Commits.
  if [[ $subject =~ ^chore\(release\) ]] || [[ $subject =~ ^snapr\ [0-9]+\.[0-9]+\.[0-9]+ ]]; then
    continue
  fi
  group=3
  line=$subject
  if [[ $subject =~ ^([a-z]+)(\(([a-z0-9-]+)\))?(!)?:\ +(.*)$ ]]; then
    type=${BASH_REMATCH[1]}
    scope=${BASH_REMATCH[3]}
    breaking=${BASH_REMATCH[4]}
    line=${BASH_REMATCH[5]}
    line=${line^}
    case $type in
      feat) group=0 ;;
      fix) group=1 ;;
      perf) group=2 ;;
    esac
    case $scope in
      macos) scope=macOS ;;
      windows) scope=Windows ;;
      linux) scope=Linux ;;
    esac
    [ -n "$scope" ] && line="**$scope:** $line"
    [ -n "$breaking" ] && line="**Breaking:** $line"
  fi
  sections[group]+="- $line (${hash:0:7})"$'\n'
done < <(git rev-list --reverse --no-merges "$range")

any=
for i in "${!titles[@]}"; do
  [ -z "${sections[i]}" ] && continue
  [ -n "$any" ] && echo
  any=1
  printf '## %s\n\n%s' "${titles[i]}" "${sections[i]}"
done
[ -z "$any" ] && echo "No changes besides the version."

if [ -n "$prev" ] && [ -n "$repo" ]; then
  printf '\n**Full Changelog**: https://github.com/%s/compare/%s...%s\n' "$repo" "$prev" "$tag"
fi
