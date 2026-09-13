#!/bin/sh
set -eu

if [ "$#" -ne 2 ]; then
    printf 'usage: %s PAPER_JAR OUTPUT_PLUGIN_JAR\n' "$0" >&2
    exit 2
fi

paper_jar=$1
output_jar=$2
java_home=${JAVA_HOME:?JAVA_HOME must point to the pinned JDK 25 installation}
javac="$java_home/bin/javac"
jar="$java_home/bin/jar"

if [ ! -s "$paper_jar" ]; then
    printf 'Paper jar is missing or empty: %s\n' "$paper_jar" >&2
    exit 2
fi
if [ ! -x "$javac" ] || [ ! -x "$jar" ]; then
    printf 'JAVA_HOME must provide executable javac and jar: %s\n' "$java_home" >&2
    exit 2
fi

root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
build=$(mktemp -d "${TMPDIR:-/tmp}/lodestone-paper-driver.XXXXXX")
trap 'rm -rf "$build"' EXIT HUP INT TERM
mkdir -p "$build/classes"

# The Paper jar is an operator input. This recipe performs no network access,
# dependency resolution, jar patching, or source transformation.
"$javac" --release 25 -encoding UTF-8 -cp "$paper_jar" -d "$build/classes" \
    "$root/src/io/lodestone/conformance/PaperConformancePlugin.java"
mkdir -p "$(dirname -- "$output_jar")"
"$jar" --create --file "$output_jar" \
    -C "$build/classes" . \
    -C "$root" plugin.yml

if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$output_jar"
else
    sha256sum "$output_jar"
fi
