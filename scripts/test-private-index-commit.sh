#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
guard=$script_dir/private-index-commit.sh
fixture=$(mktemp -d "${TMPDIR:-/tmp}/lodestone-private-index-test.XXXXXX")
trap 'rm -rf "$fixture"' EXIT HUP INT TERM

git -C "$fixture" init -q
git -C "$fixture" config user.name test
git -C "$fixture" config user.email test@example.invalid
git -C "$fixture" config commit.gpgSign false
git -C "$fixture" config core.hooksPath "$script_dir/../.githooks"
printf 'base\n' > "$fixture/owned.txt"
git -C "$fixture" add owned.txt
git -C "$fixture" commit -qm base
base=$(git -C "$fixture" rev-parse HEAD)

private_index=$fixture/private.index
GIT_INDEX_FILE=$private_index git -C "$fixture" read-tree "$base"
printf 'agent edit\n' > "$fixture/owned.txt"
blob=$(git -C "$fixture" hash-object -w "$fixture/owned.txt")
GIT_INDEX_FILE=$private_index git -C "$fixture" update-index --cacheinfo 100644 "$blob" owned.txt

printf 'pathspec edit that must not land\n' > "$fixture/owned.txt"
if git -C "$fixture" commit -m "must fail" -- owned.txt >"$fixture/pathspec.out" 2>"$fixture/pathspec.err"; then
    echo "pathspec commit unexpectedly passed the shared-checkout guard" >&2
    exit 1
fi
grep -q "refusing pathspec commit" "$fixture/pathspec.err"
test "$(git -C "$fixture" log -1 --format=%s)" = base
printf 'agent edit\n' > "$fixture/owned.txt"

printf 'only edit that must not land\n' > "$fixture/owned.txt"
if git -C "$fixture" commit --only -m "must fail" -- owned.txt >"$fixture/only.out" 2>"$fixture/only.err"; then
    echo "--only commit unexpectedly passed the shared-checkout guard" >&2
    exit 1
fi
grep -q "refusing pathspec commit" "$fixture/only.err"
test "$(git -C "$fixture" log -1 --format=%s)" = base
printf 'agent edit\n' > "$fixture/owned.txt"

printf 'concurrent edit\n' > "$fixture/concurrent.txt"
git -C "$fixture" add concurrent.txt
git -C "$fixture" commit -qm concurrent

if (cd "$fixture" && GIT_INDEX_FILE=$private_index "$guard" "$base" "must fail" owned.txt) >"$fixture/out" 2>"$fixture/err"; then
    echo "stale private index unexpectedly committed" >&2
    exit 1
fi
grep -q "refusing stale private-index commit" "$fixture/err"
test "$(git -C "$fixture" log -1 --format=%s)" = concurrent

current=$(git -C "$fixture" rev-parse HEAD)
GIT_INDEX_FILE=$private_index git -C "$fixture" read-tree "$current"
blob=$(git -C "$fixture" hash-object -w "$fixture/owned.txt")
GIT_INDEX_FILE=$private_index git -C "$fixture" update-index --cacheinfo 100644 "$blob" owned.txt
(cd "$fixture" && GIT_INDEX_FILE=$private_index "$guard" "$current" "safe commit" owned.txt) >/dev/null

test "$(git -C "$fixture" log -1 --format=%s)" = "safe commit"
test "$(git -C "$fixture" show HEAD:owned.txt)" = "agent edit"
test "$(git -C "$fixture" show HEAD:concurrent.txt)" = "concurrent edit"
echo "private-index pathspec and stale-base controls passed"
