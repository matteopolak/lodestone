#!/bin/sh
set -eu

usage() {
    echo "usage: $0 <base-commit> <commit-message> <path>..." >&2
    exit 2
}

[ "$#" -ge 3 ] || usage
base=$1
message=$2
shift 2

case "${GIT_INDEX_FILE-}" in
    "")
        echo "refusing to use the shared index: set GIT_INDEX_FILE to a private index path" >&2
        exit 2
        ;;
esac

repo=$(git rev-parse --show-toplevel)
current_head=$(git -C "$repo" rev-parse HEAD)
base_commit=$(git -C "$repo" rev-parse "$base^{commit}")

if [ "$current_head" != "$base_commit" ]; then
    echo "refusing stale private-index commit: expected HEAD $base_commit, found $current_head" >&2
    echo "re-read each selected path from current HEAD and rebuild the private index" >&2
    exit 1
fi

for path in "$@"; do
    case "$path" in
        /*|../*|*/../*|*/..|.)
            echo "refusing path outside the repository: $path" >&2
            exit 2
            ;;
    esac
    index_entry=$(git -C "$repo" ls-files --stage -- "$path")
    if [ -z "$index_entry" ]; then
        echo "refusing unselected path: $path is absent from the private index" >&2
        exit 1
    fi
done

tree=$(git -C "$repo" write-tree)
commit=$(printf '%s\n' "$message" | git -C "$repo" commit-tree "$tree" -p "$current_head")

if ! git -C "$repo" update-ref refs/heads/main "$commit" "$current_head"; then
    echo "HEAD advanced before publication; commit $commit was not installed" >&2
    exit 1
fi

unset GIT_INDEX_FILE
for path in "$@"; do
    if ! git -C "$repo" reset -q "$commit" -- "$path"; then
        echo "published commit $commit but could not reconcile the shared index for $path" >&2
        exit 1
    fi
done

printf '%s\n' "$commit"
