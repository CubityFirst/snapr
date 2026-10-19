#!/usr/bin/env bash
# Checks commit messages follow Conventional Commits (see CONTRIBUTING.md).
#
#   scripts/check-commit-msg.sh <message file>   # as a commit-msg hook
#   scripts/check-commit-msg.sh --range <a..b>   # each commit in a range
#
# Git's own merge and revert messages, and fixup!/squash! commits, pass.

set -euo pipefail

types='feat|fix|perf|refactor|docs|style|test|build|ci|chore|revert'
pattern="^($types)(\([a-z0-9][a-z0-9-]*\))?!?: [^ ]"

ok() {
  local subject=$1
  [[ $subject =~ $pattern ]] || [[ $subject =~ ^(Merge|Revert\ \"|fixup!|squash!|amend!) ]]
}

complain() {
  echo "Not a Conventional Commit: \"$1\"" >&2
  echo "Expected <type>[(scope)][!]: <description>, with <type> one of" >&2
  echo "${types//|/, }. See CONTRIBUTING.md." >&2
}

if [ "${1:-}" = "--range" ]; then
  [ $# -eq 2 ] || { echo "usage: $0 --range <a..b>" >&2; exit 2; }
  failed=0
  while IFS= read -r line; do
    subject=${line#* }
    if ! ok "$subject"; then
      echo "${line%% *}:" >&2
      complain "$subject"
      failed=1
    fi
  done < <(git log --no-merges --format='%h %s' "$2")
  exit $failed
fi

[ $# -eq 1 ] || { echo "usage: $0 <message file> | --range <a..b>" >&2; exit 2; }
# The first line that isn't a comment.
subject=$(grep -v '^#' "$1" | sed -n '/[^[:space:]]/{p;q;}' || true)
ok "$subject" || { complain "$subject"; exit 1; }
