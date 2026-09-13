#!/usr/bin/env bash
# Prepare one immutable seed copy of a sealed world, then create an isolated
# writable clone for one JVM read. The clone is deliberately disposable: the
# server is allowed to write its normal lock/cache/save files without making
# those writes visible to the next shard.
set -euo pipefail

usage() {
  cat >&2 <<'EOF'
usage: frozen-world-clone.sh clone SOURCE CACHE_ROOT FREEZE_STAMP
       frozen-world-clone.sh selftest
EOF
}

copy_tree() {
  local from="$1" to="$2"
  mkdir -p "$to"
  cp -R "$from/." "$to/"
}

assert_not_below_source() {
  local source="$1" cache="$2"
  local source_real cache_parent cache_real
  source_real="$(cd "$source" && pwd -P)"
  cache_parent="$(dirname "$cache")"
  mkdir -p "$cache_parent"
  cache_real="$(cd "$cache_parent" && pwd -P)/$(basename "$cache")"
  case "$cache_real" in
    "$source_real"|"$source_real"/*)
      echo "frozen-world clone cache must not be inside the sealed source root: $cache_real" >&2
      exit 2
      ;;
  esac
}

acquire_lock() {
  local cache="$1" lock="$cache/.lock" owner
  mkdir -p "$cache"
  while ! mkdir "$lock" 2>/dev/null; do
    owner=""
    if [ -f "$lock/pid" ]; then owner="$(cat "$lock/pid" 2>/dev/null || true)"; fi
    if [ -n "$owner" ] && ! kill -0 "$owner" 2>/dev/null; then
      rm -rf "$lock"
      continue
    fi
    sleep 1
  done
  printf '%s\n' "$$" > "$lock/pid"
}

ensure_seed() {
  local source="$1" cache="$2" stamp="$3"
  local seed="$cache/seed" marker="$cache/seed.marker" expected actual temporary
  expected="$(cat "$source/$stamp" 2>/dev/null || true)"
  if [ -z "$expected" ]; then
    echo "sealed source is missing the selected freeze stamp: $source/$stamp" >&2
    exit 2
  fi
  actual=""
  if [ -f "$marker" ]; then actual="$(cat "$marker" 2>/dev/null || true)"; fi
  if [ -d "$seed" ] && [ "$actual" = "$expected" ]; then
    return
  fi
  if [ -e "$seed" ] || [ -e "$marker" ]; then
    rm -rf "$seed" "$marker"
  fi
  temporary="$cache/.seed.tmp.$$"
  rm -rf "$temporary"
  copy_tree "$source" "$temporary"
  chmod -R a-w "$temporary"
  mv "$temporary" "$seed"
  printf '%s\n' "$expected" > "$marker"
}

create_clone() {
  local source="$1" cache="$2" stamp="$3" clone
  assert_not_below_source "$source" "$cache"
  mkdir -p "$cache"
  acquire_lock "$cache"
  ensure_seed "$source" "$cache" "$stamp"
  clone="$(mktemp -d "$cache/read.XXXXXX")"
  # APFS and Linux request fail-closed clone operations. Other hosts use an
  # ordinary copy because correctness still matters more than optimization.
  if ! case "$(uname -s)" in
    Darwin) cp -cR "$cache/seed/." "$clone/" ;;
    Linux) cp -a --reflink=always "$cache/seed/." "$clone/" ;;
    *) cp -R "$cache/seed/." "$clone/" ;;
  esac
  then
    echo "filesystem does not support the requested frozen-world clone" >&2
    chmod -R u+w "$clone" 2>/dev/null || true
    rm -rf "$clone"
    rm -rf "$cache/.lock"
    exit 2
  fi
  chmod -R u+w "$clone"
  rm -rf "$cache/.lock"
  printf '%s\n' "$clone"
}

selftest() {
  local root source cache a b before
  root="$(mktemp -d "${TMPDIR:-/tmp}/lodestone-frozen-clone-selftest.XXXXXX")"
  trap "chmod -R u+w '$root' 2>/dev/null || true; rm -rf '$root'" EXIT
  source="$root/source"; cache="$root/cache"
  mkdir -p "$source/region"
  printf 'frozen bytes\n' > "$source/region/r.0.0.mca"
  printf 'seal\n' > "$source/lodestone-large-parity-materialization-v6-end.freeze.sha256"
  before="$(shasum -a 256 "$source/region/r.0.0.mca" | awk '{print $1}')"
  a="$(create_clone "$source" "$cache" lodestone-large-parity-materialization-v6-end.freeze.sha256)"
  printf 'mutable shard A\n' > "$a/region/mutable.state"
  if [ "$(shasum -a 256 "$source/region/r.0.0.mca" | awk '{print $1}')" != "$before" ] || [ -e "$source/region/mutable.state" ]; then
    echo "source changed through read-A clone" >&2
    exit 1
  fi
  rm -rf "$a"
  b="$(create_clone "$source" "$cache" lodestone-large-parity-materialization-v6-end.freeze.sha256)"
  if [ -e "$b/region/mutable.state" ]; then
    echo "read-B clone reused mutable state from read A" >&2
    exit 1
  fi
  printf 'mutable shard B\n' > "$b/region/mutable.state"
  if [ -e "$source/region/mutable.state" ]; then
    echo "source changed through read-B clone" >&2
    exit 1
  fi
  rm -rf "$b"
  echo "frozen-world clone selftest ok: source isolation, read-A/read-B isolation, and interrupted-read cleanup"
}

case "${1:-}" in
  clone)
    [ "$#" -eq 4 ] || { usage; exit 2; }
    create_clone "$2" "$3" "$4"
    ;;
  selftest)
    [ "$#" -eq 1 ] || { usage; exit 2; }
    selftest
    ;;
  *) usage; exit 2 ;;
esac
