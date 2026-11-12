#!/bin/sh
# node --check on every module of the app's own UI, the UI's words held to their catalogues (qa/i18n-check.mjs, "UI" 16),
# and FinUI's preset overlay (web/assets/finui/p) made again from the
# copied FinUI to see it is what FinUI makes: the only JS checks there are. By default in a node container, so no Node
# is needed on the machine; `qa/js-check.sh --here` runs them with the node on the PATH instead (CI, where a job's
# folders are not on the Docker daemon's disk for a bind mount).
set -eu
cd "$(dirname "$0")/.."
if [ "${1:-}" != "--here" ]; then
  exec docker run --rm -v "$PWD/web:/src/web:ro" -v "$PWD/qa:/src/qa:ro" -w /src node:22-alpine sh qa/js-check.sh --here
fi
made=$(mktemp -d)
trap 'rm -rf "$made"' EXIT
bad=0
for f in $(find web/assets/js -name "*.js"); do node --check "$f" || bad=1; done
[ $bad -eq 0 ] && echo "every module parses"
node qa/i18n-check.mjs web || bad=1
if node qa/finui-presets.mjs web/assets/finui "$made" && diff -r "$made/p" web/assets/finui/p >/dev/null; then
  echo "the preset overlay is what FinUI makes"
else
  echo "web/assets/finui/p is not what FinUI makes: node qa/finui-presets.mjs web/assets/finui web/assets/finui"
  bad=1
fi
exit $bad
