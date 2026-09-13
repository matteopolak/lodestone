#!/usr/bin/env bash
#
# fetch-paper.sh — download the pinned Paper server jar the NMS census is
# measured against.
#
# WHY THIS EXISTS
#   `docs/java-plugin-bridge.md` reports a census of the target-package surface
#   that Paper's own bytecode calls. That number is only meaningful if
#   the next reader can reproduce it, and "download the latest Paper" is not
#   reproducible: Paper publishes several builds a week and each one changes
#   the counts. So the version AND the build number are pinned here, and the
#   download is checksum-verified against the digest the API reported for that
#   exact build.
#
# WHAT THIS JAR IS, AND IS NOT
#   It is a LOCAL MEASUREMENT INPUT. It is not committed, not redistributed,
#   and not shipped. Paper is GPL-3.0; the bridge's whole licensing argument
#   (see `docs/java-plugin-bridge.md`) rests on the user supplying their own
#   Paper jar and on us never distributing Paper bytecode, modified or
#   otherwise. `.cache/` is outside git for exactly this reason -- do not add
#   it, and do not copy this jar anywhere that is.
#
# USAGE
#   ./scripts/fetch-paper.sh              # fetch the pinned build
#   ./scripts/fetch-paper.sh --print-url  # show what it would fetch, download nothing
#
# THE API
#   PaperMC's v2 API is SUNSET -- it answers every request with
#   {"ok":false,"error":"sunset"}, HTTP 200, which a script that only checks
#   curl's exit status will happily save as a 106-byte "jar". The live API is
#   v3 ("fill"), used below, and the checksum check is what turns that class of
#   mistake into a loud failure rather than an empty census.

set -euo pipefail

# --- the pin -----------------------------------------------------------------
# Paper 26.2 build 121, channel STABLE, published 2026-08-29T11:32:25Z.
# Chosen because 26.2 is the Minecraft version this repo targets, so the census
# lines up with the decompiled source already under `.cache/mc/26.2/src`.
PAPER_VERSION="26.2"
PAPER_BUILD="121"
PAPER_SHA256="0de30efb024bc8b83c9c7d507d11802897ad8056b6110ec09fe1a91d126ccb54"
PAPER_JAR_NAME="paper-${PAPER_VERSION}-${PAPER_BUILD}.jar"
# fill-data serves objects by digest, so the URL is derived from the pin rather
# than fetched -- one less thing that can drift between runs.
PAPER_URL="https://fill-data.papermc.io/v1/objects/${PAPER_SHA256}/${PAPER_JAR_NAME}"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dest_dir="${repo_root}/.cache/paper/${PAPER_VERSION}"
dest="${dest_dir}/paper.jar"

if [[ "${1:-}" == "--print-url" ]]; then
  echo "version: ${PAPER_VERSION}"
  echo "build:   ${PAPER_BUILD}"
  echo "sha256:  ${PAPER_SHA256}"
  echo "url:     ${PAPER_URL}"
  echo "dest:    ${dest}"
  exit 0
fi

# Verify a digest with whichever tool this machine has; macOS ships
# `shasum`, most Linux images ship `sha256sum`.
digest_of() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d' ' -f1
  else
    sha256sum "$1" | cut -d' ' -f1
  fi
}

if [[ -f "${dest}" ]]; then
  have="$(digest_of "${dest}")"
  if [[ "${have}" == "${PAPER_SHA256}" ]]; then
    echo "already present and verified: ${dest}"
    echo "  paper ${PAPER_VERSION} build ${PAPER_BUILD}"
    exit 0
  fi
  echo "existing ${dest} has digest ${have}, wanted ${PAPER_SHA256} -- refetching" >&2
fi

mkdir -p "${dest_dir}"
tmp="${dest}.partial"
echo "fetching paper ${PAPER_VERSION} build ${PAPER_BUILD} ..."
# `--fail` so an HTTP error is curl's exit status rather than an error page
# written to the destination.
curl --fail --location --show-error --silent --max-time 600 --output "${tmp}" "${PAPER_URL}"

have="$(digest_of "${tmp}")"
if [[ "${have}" != "${PAPER_SHA256}" ]]; then
  rm -f "${tmp}"
  echo "CHECKSUM MISMATCH: got ${have}, expected ${PAPER_SHA256}" >&2
  echo "Refusing to install. The pin above may be stale, or the download was truncated." >&2
  exit 1
fi

mv "${tmp}" "${dest}"
echo "ok: ${dest}"
echo "  paper ${PAPER_VERSION} build ${PAPER_BUILD}, sha256 verified"
echo
echo "census it with:"
echo "  cargo run --release -p lodestone-nms-census --bin nms-census -- \\"
echo "      ${dest} --prefix <target-package/> --top 40"
