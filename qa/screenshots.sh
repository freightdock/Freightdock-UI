#!/bin/sh
# The README's screenshots, made again ("Project" 9, "Cluster" 33): a throwaway one-node cluster (qa/cluster.sh, on
# its own name and port so it never meets the end-to-end suite's), qa/screenshots.mjs against it, then it goes.
# Writes docs/screenshots/<page>.png and <page>-dark.png. Needs Docker, Node and Chromium; the same settings as
# qa/cluster.sh (FD_DIST, FD_IMAGE), and FD_CHROMIUM.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
export FD_NAME=${FD_NAME:-fd-shots} FD_PORT=${FD_PORT:-17444}
"$here/cluster.sh" up
set +e
node "$here/screenshots.mjs" "https://127.0.0.1:$FD_PORT" "${TMPDIR:-/tmp}/$FD_NAME.setup" "$here/../docs/screenshots"
failed=$?
set -e
"$here/cluster.sh" down
exit "$failed"
