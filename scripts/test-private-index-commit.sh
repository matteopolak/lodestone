#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
guard=$script_dir/private-index-commit.sh
fixture=$(mktemp -d "${TMPDIR:-/tmp}/lodestone-private-index-test.XXXXXX")
a_pid=
b_pid=
cleanup() {
    [ -z "$a_pid" ] || kill "$a_pid" 2>/dev/null || true
    [ -z "$b_pid" ] || kill "$b_pid" 2>/dev/null || true
    [ -z "$a_pid" ] || wait "$a_pid" 2>/dev/null || true
    [ -z "$b_pid" ] || wait "$b_pid" 2>/dev/null || true
    rm -rf "$fixture"
}
trap cleanup EXIT HUP INT TERM

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
printf 'unselected staged\n' > "$fixture/unselected.txt"
git -C "$fixture" add unselected.txt
(cd "$fixture" && GIT_INDEX_FILE=$private_index "$guard" "$current" "safe commit" owned.txt) >/dev/null

test "$(git -C "$fixture" log -1 --format=%s)" = "safe commit"
test "$(git -C "$fixture" show HEAD:owned.txt)" = "agent edit"
test "$(git -C "$fixture" show HEAD:concurrent.txt)" = "concurrent edit"
test "$(git -C "$fixture" diff --cached --name-only)" = unselected.txt

printf 'weather a base\n' > "$fixture/weather-a.txt"
printf 'weather b base\n' > "$fixture/weather-b.txt"
git -C "$fixture" add weather-a.txt weather-b.txt
git -C "$fixture" commit -qm weather-base
weather_base=$(git -C "$fixture" rev-parse HEAD)
printf 'unrelated staged\n' > "$fixture/unrelated.txt"
git -C "$fixture" add unrelated.txt

weather_a_index=$fixture/weather-a.index
GIT_INDEX_FILE=$weather_a_index git -C "$fixture" read-tree "$weather_base"
printf 'weather a first\n' > "$fixture/weather-a.txt"
blob=$(git -C "$fixture" hash-object -w "$fixture/weather-a.txt")
GIT_INDEX_FILE=$weather_a_index git -C "$fixture" update-index --cacheinfo 100644 "$blob" weather-a.txt

race_bin=$fixture/race-bin
mkdir "$race_bin"
real_git=$(command -v git)
race_git=$race_bin/git
printf '%s\n' \
    '#!/bin/sh' \
    'case " $*" in' \
    '    *" reset -q "*)' \
    '        if [ ! -e "$PRIVATE_INDEX_RACE_MARKER" ]; then' \
    '            : > "$PRIVATE_INDEX_RACE_MARKER"' \
    '            while [ ! -e "$PRIVATE_INDEX_RACE_RELEASE" ]; do sleep 0.05; done' \
    '        fi' \
    '        ;;' \
    'esac' \
    'exec "$PRIVATE_INDEX_RACE_REAL_GIT" "$@"' > "$race_git"
chmod +x "$race_git"

(
    cd "$fixture"
    PATH="$race_bin:$PATH" \
    PRIVATE_INDEX_RACE_REAL_GIT="$real_git" \
    PRIVATE_INDEX_RACE_MARKER="$fixture/race.marker" \
    PRIVATE_INDEX_RACE_RELEASE="$fixture/race.release" \
    GIT_INDEX_FILE="$weather_a_index" \
    "$guard" "$weather_base" "first weather commit" weather-a.txt weather-b.txt
) > "$fixture/weather-a.out" 2> "$fixture/weather-a.err" &
a_pid=$!

attempt=0
while [ ! -e "$fixture/race.marker" ]; do
    if ! kill -0 "$a_pid" 2>/dev/null; then
        cat "$fixture/weather-a.err" >&2
        exit 1
    fi
    attempt=$((attempt + 1))
    [ "$attempt" -lt 200 ] || { echo "first weather helper did not reach reconciliation" >&2; exit 1; }
    sleep 0.05
done

first_weather=$(git -C "$fixture" rev-parse HEAD)
test "$(git -C "$fixture" log -1 --format=%s)" = "first weather commit"
weather_b_index=$fixture/weather-b.index
GIT_INDEX_FILE=$weather_b_index git -C "$fixture" read-tree "$first_weather"
printf 'weather b second\n' > "$fixture/weather-b.txt"
blob=$(git -C "$fixture" hash-object -w "$fixture/weather-b.txt")
GIT_INDEX_FILE=$weather_b_index git -C "$fixture" update-index --cacheinfo 100644 "$blob" weather-b.txt

(
    cd "$fixture"
    GIT_INDEX_FILE="$weather_b_index" "$guard" "$first_weather" "second weather commit" weather-b.txt
) > "$fixture/weather-b.out" 2> "$fixture/weather-b.err" &
b_pid=$!
sleep 0.2
if ! kill -0 "$b_pid" 2>/dev/null; then
    cat "$fixture/weather-b.err" >&2
    echo "second weather helper published before the first reconciled" >&2
    exit 1
fi

: > "$fixture/race.release"
wait "$a_pid"
a_pid=
wait "$b_pid"
b_pid=

test "$(git -C "$fixture" log -1 --format=%s)" = "second weather commit"
test "$(git -C "$fixture" show HEAD:weather-a.txt)" = "weather a first"
test "$(git -C "$fixture" show HEAD:weather-b.txt)" = "weather b second"
test "$(git -C "$fixture" diff --cached --name-only)" = unrelated.txt
echo "private-index pathspec and stale-base controls passed"
