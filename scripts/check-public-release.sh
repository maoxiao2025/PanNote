#!/usr/bin/env bash
set -euo pipefail

root="${1:-.}"

if [[ ! -d "$root" ]]; then
  printf 'ERROR: directory does not exist: %s\n' "$root" >&2
  exit 2
fi

if ! git -C "$root" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  printf 'ERROR: target must be a Git working tree so tracked and untracked files can be checked.\n' >&2
  exit 2
fi

errors=0
warnings=0
file_list="$(mktemp)"
history_list="$(mktemp)"
trap 'rm -f "$file_list" "$history_list"' EXIT
git -C "$root" ls-files --cached --others --exclude-standard -z > "$file_list"

printf 'Checking public release tree: %s\n' "$root"

while IFS= read -r -d '' path; do
  case "$path" in
    .temp/*|*/.temp/*|dist/*|*.db|*.db*|*.sqlite*|*.wav|*.mp3|*.m4a|*.caf|*.dmg|*.app/*|*.log|*.pid|*SECRET*|*secret*|*.pem|*.key|.env|.env.*)
      printf 'ERROR: sensitive or release-only path: %s\n' "$path"
      errors=$((errors + 1))
      ;;
  esac

  full_path="$root/$path"
  [[ -f "$full_path" ]] || continue

  if ! file "$full_path" | grep -Eq 'text|script|source|XML|JSON|empty'; then
    continue
  fi

  if grep -IEn \
    '(-----BEGIN (RSA |OPENSSH |EC |)PRIVATE KEY-----|AKIA[0-9A-Z]{16}|gh[pousr]_[A-Za-z0-9_]{20,}|sk-[A-Za-z0-9_-]{24,}|/Users/[^ /]+/Documents/PanNote|/Users/[^ /]+/Library/Application Support|\.workbuddy)' \
    "$full_path" >/dev/null 2>&1; then
    printf 'ERROR: possible credential, private key, or developer-machine path in %s\n' "$path"
    errors=$((errors + 1))
  fi
done < "$file_list"

git -C "$root" -c diff.renames=false log --all --format='%h' --name-only > "$history_list"
if grep -E '(^|/)(\.temp/|[^/]*\.(db|sqlite|sqlite3)([.-]|$)|[^/]*SECRET[^/]*)' "$history_list" >/dev/null; then
  printf 'ERROR: Git history contains paths matching local data/secret patterns; a clean tree is insufficient.\n'
  errors=$((errors + 1))
else
  printf 'WARNING: path-based history check found no obvious local-data paths; this is not a complete secret/history audit.\n'
  warnings=$((warnings + 1))
fi

if (( errors > 0 )); then
  printf 'FAILED: %d finding(s), %d warning(s). Do not publish this tree.\n' "$errors" "$warnings" >&2
  exit 1
fi

printf 'PASSED basic checks with %d warning(s). Human review is still required before publication.\n' "$warnings"
