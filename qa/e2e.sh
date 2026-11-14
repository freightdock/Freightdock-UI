#!/bin/sh
# The end-to-end suite against a throwaway one-node cluster: qa/cluster.sh up, qa/e2e.mjs, qa/cluster.sh down.
# The cluster is left running for a look when a step fails, or always with FD_KEEP=1; qa/cluster.sh down removes it.
# The same settings as qa/cluster.sh (FD_DIST, FD_IMAGE, FD_PORT, FD_NAME).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
port=${FD_PORT:-17443}
name=${FD_NAME:-fd-e2e}
"$here/cluster.sh" up
set +e
node "$here/e2e.mjs" "https://127.0.0.1:$port" "${TMPDIR:-/tmp}/$name.setup"
failed=$?
set -e
if [ "$failed" -ne 0 ]; then
  "$here/cluster.sh" logs 30
  echo "left running for a look; qa/cluster.sh down removes it" >&2
elif [ "${FD_KEEP:-}" != 1 ]; then
  "$here/cluster.sh" down
fi
exit "$failed"
