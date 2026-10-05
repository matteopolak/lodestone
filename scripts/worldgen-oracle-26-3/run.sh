#!/usr/bin/env bash
# Compile and run a worldgen JVM oracle against the real server classes of the
# current Minecraft release (repo-root `mc-version`, default 26.3) in an
# ephemeral temurin:25-jdk container (Apple `container`; the host needs no java).
#   usage: run.sh <OracleClassName> [args...]
# Stdout is the oracle's output. Arguments reach the oracle as its argv.
set -euo pipefail
CLASS="${1:?usage: run.sh <OracleClass> [args...]}"
shift || true
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
VERSION="${LODESTONE_MC_VERSION:-$(tr -d '[:space:]' < "$REPO_ROOT/mc-version")}"
CACHE="${LODESTONE_MC_CACHE:-$REPO_ROOT/.cache/mc/$VERSION}"
if [ ! -f "$CACHE/versions/$VERSION/server-$VERSION.jar" ]; then
  echo "server jar not found under $CACHE/versions/$VERSION" >&2
  exit 1
fi
HERE="$(cd "$(dirname "$0")" && pwd)"
container system start >/dev/null 2>&1 || true
container run --rm --memory 4g \
  -v "$CACHE:/mc:ro" -v "$HERE:/oracle:ro" -w /work \
  eclipse-temurin:25-jdk bash -c '
    set -e
    LIB_CP="$(find /mc/libraries -name "*.jar" | tr "\n" ":")"
    CP="/mc/versions/'"$VERSION"'/server-'"$VERSION"'.jar:$LIB_CP"
    mkdir -p /work
    cp /oracle/*.java /work/
    if [ -d /oracle/patches ]; then
      # Patched vanilla classes shadow the jar'"'"'s: the jar is signed, so its classes and an
      # unsigned patch cannot share a package; repack it without signatures first.
      mkdir -p /work/jar && (cd /work/jar && jar xf /mc/versions/'"$VERSION"'/server-'"$VERSION"'.jar && rm -f META-INF/*.SF META-INF/*.RSA META-INF/*.DSA)
      javac -nowarn -cp "$CP" -d /work/jar $(find /oracle/patches -name "*.java")
      CP="/work/jar:$LIB_CP"
    fi
    javac -nowarn -cp "$CP" -d /work /work/'"$CLASS"'.java
    java -Dmax.bg.threads=1 -cp "/work:$CP" '"$CLASS"' "$@"
  ' _ "$@"
