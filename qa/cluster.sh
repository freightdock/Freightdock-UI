#!/bin/sh
# A throwaway one-node Freightdock cluster in a privileged `docker:dind` container, for the end-to-end suite
# (qa/e2e.sh) and the screenshots (qa/screenshots.sh). Nothing of it touches the host but that one container.
#
#   qa/cluster.sh up      start it: a manager and a worker in the container, the UI beside them
#   qa/cluster.sh down    remove it, with everything it ran
#   qa/cluster.sh logs    the manager's, the worker's and the UI's logs
#
# FD_DIST    the folder with the static freightdock-manager and freightdock-worker
#            (Freightdock-Manager/deploy/build-static.sh puts them in ../dist, the default)
# FD_IMAGE   the UI image to run (default freightdock:e2e, built from this repository when it is not there)
# FD_PORT    the host port the UI is published on, on 127.0.0.1 only (default 17443)
# FD_NAME    the container's name (default fd-e2e)
# FD_PRELOAD images copied into the cluster from the host's Docker, so no test waits on a registry
#            (default nginx:alpine)
#
# The UI's first administrator is made with the setup token written to <FD_NAME>.setup in the system's temporary
# folder; `up` prints the address. The UI runs beside the manager on the node (not as the managers' system workload,
# which needs an address of the LAN's pool), with the manager's admin token, so it sees what an Administrator does.
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
dist=${FD_DIST:-$here/../dist}
image=${FD_IMAGE:-freightdock:e2e}
port=${FD_PORT:-17443}
name=${FD_NAME:-fd-e2e}
preload=${FD_PRELOAD:-nginx:alpine}
setup_file=${TMPDIR:-/tmp}/$name.setup
on() { docker exec "$name" sh -c "$1"; }

wait_for() { # what, tries, command
  what=$1 tries=$2; shift 2
  for _ in $(seq "$tries"); do
    if "$@" >/dev/null 2>&1; then return 0; fi
    sleep 1
  done
  echo "$what did not come up" >&2
  return 1
}

up() {
  for b in freightdock-manager freightdock-worker; do
    [ -x "$dist/$b" ] || { echo "no $dist/$b: run Freightdock-Manager/deploy/build-static.sh, or set FD_DIST" >&2; exit 1; }
  done
  if ! docker image inspect "$image" >/dev/null 2>&1; then
    echo "building $image"
    docker build -q -t "$image" "$here" >/dev/null
  fi
  docker rm -f "$name" >/dev/null 2>&1 || true
  docker run -d --privileged --name "$name" -p "127.0.0.1:$port:7443" docker:28-dind >/dev/null
  wait_for "the node's Docker" 60 on "docker info"
  docker cp "$dist/freightdock-manager" "$name:/usr/local/bin/freightdock-manager"
  docker cp "$dist/freightdock-worker" "$name:/usr/local/bin/freightdock-worker"
  ip=$(docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' "$name")

  on "mkdir -p /var/lib/freightdock-manager /var/lib/freightdock-worker"
  docker exec -d -e FREIGHTDOCK_MANAGER_DATA=/var/lib/freightdock-manager -e "FREIGHTDOCK_MANAGER_ADVERTISE=$ip:7450" \
    -e FREIGHTDOCK_MANAGER_LOCAL_WORKER_TOKEN=/var/lib/freightdock-manager/local-worker.token \
    "$name" sh -c 'freightdock-manager serve >/var/log/freightdock-manager.log 2>&1; echo "exited with $?" >>/var/log/freightdock-manager.log'
  wait_for "the manager" 60 on "wget -qO- http://127.0.0.1:7451/v1/health"
  wait_for "the worker's token" 30 on "test -s /var/lib/freightdock-manager/local-worker.token"
  docker exec -d -e FREIGHTDOCK_WORKER_DATA=/var/lib/freightdock-worker -e FREIGHTDOCK_MANAGERS=127.0.0.1:7450 \
    -e FREIGHTDOCK_JOIN_TOKEN_FILE=/var/lib/freightdock-manager/local-worker.token -e FREIGHTDOCK_LABELS=zone=a \
    "$name" sh -c 'freightdock-worker >/var/log/freightdock-worker.log 2>&1; echo "exited with $?" >>/var/log/freightdock-worker.log'

  for i in $image $preload; do
    docker image inspect "$i" >/dev/null 2>&1 || docker pull -q "$i" >/dev/null
    docker save "$i" | docker exec -i "$name" docker load -q >/dev/null
  done
  setup=$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')
  umask 077 && printf '%s\n' "$setup" >"$setup_file"
  on "docker run -d -q --name freightdock-ui --network host -v /var/lib/freightdock-manager/admin.token:/run/admin.token:ro \
      -e FREIGHTDOCK_CLUSTER_API=http://127.0.0.1:7451 -e FREIGHTDOCK_CLUSTER_TOKEN_FILE=/run/admin.token \
      -e FREIGHTDOCK_SETUP_TOKEN=$setup -e FREIGHTDOCK_TLS_NAMES=localhost $image" >/dev/null
  wait_for "the UI" 60 curl -skf "https://127.0.0.1:$port/api/health"
  echo "https://127.0.0.1:$port (setup token in $setup_file)"
}

case ${1:-} in
  up) up ;;
  down) docker rm -f "$name" >/dev/null 2>&1 || true; rm -f "$setup_file" ;;
  logs)
    for f in manager worker; do echo "== $f"; on "tail -n ${2:-40} /var/log/freightdock-$f.log"; done
    echo "== ui"; on "docker logs --tail ${2:-40} freightdock-ui 2>&1"
    ;;
  *) echo "usage: $0 up|down|logs" >&2; exit 2 ;;
esac
