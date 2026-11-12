#!/usr/bin/env bash
# Retakes the README's and the docs' screenshots (docs/screenshots/*.png) from a throwaway Freightdock with invented
# hosts, as FinStats' QA does: four throwaway Dockers (docker:dind) on a network of their own, `local`, two hosts that
# run the real agent (`node-1`, `node-2`, enrolled with a join token, the agent image loaded into each) and one on
# Docker's TLS port (`docker-tls`); a Freightdock binary pointed at `local`, plain HTTP on a free port with a data
# folder of its own. qa/screenshots.mjs then sets everything up through Freightdock's own API (the first
# administrator, the hosts, a group and tags, a few containers and a Compose stack) and takes each picture in a real
# browser over the DevTools protocol, refusing one whose page says null, undefined or NaN, still names Shipshow, or
# does not show what it is about. Everything it made is removed on the way out.
#
# Look at the pictures before committing them. Needs Docker, cargo, Node 22 or newer and a Chromium-based browser
# (QA_BROWSER, else the first of chromium, google-chrome-stable, brave and brave-origin found).
#
#   qa/screenshots.sh                          every picture, into docs/screenshots/
#   SHOTS_ONLY=home,home-dark qa/screenshots.sh   only those (each page is <name>.png light and <name>-dark.png dark)
#   SHOTS_OUT=/tmp/shots qa/screenshots.sh     somewhere else
#   QA_AGENT_IMAGE=ghcr.io/freightdock/agent:1.0.0 qa/screenshots.sh
#                                              that agent image (else built from ../freightdock-agent when it is
#                                              there, else this version's published one)
#   QA_KEEP=1 qa/screenshots.sh                leave it all running for a look afterwards
set -euo pipefail
cd "$(dirname "$0")/.."
QA_NAME=shots
# say, fail, free_port and the pinned Compose ($compose), shared with qa/run.sh.
. qa/lib.sh

dind_image=${QA_DIND_IMAGE:-docker:29-dind}
name=freightdock-shots-$$
data=$(mktemp -d "${TMPDIR:-/tmp}/freightdock-shots.XXXXXX")
hosts="local node-1 node-2 docker-tls"
pid=

cleanup() {
  status=$?
  [ -n "$pid" ] && kill "$pid" 2>/dev/null && wait "$pid" 2>/dev/null || true
  if [ $status -ne 0 ] && [ -f "$data/freightdock.log" ]; then
    say "the last of Freightdock's log:"
    tail -n 40 "$data/freightdock.log" >&2 || true
  fi
  if [ -n "${QA_KEEP:-}" ]; then
    say "kept: the containers $name-*, network $name, data $data"
  else
    for h in $hosts; do docker rm -f -v "$name-$h" >/dev/null 2>&1 || true; done
    docker network rm "$name" >/dev/null 2>&1 || true
    rm -rf "$data"
  fi
  exit $status
}
trap cleanup EXIT
trap 'exit 130' INT TERM

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
fetch_compose

say "building Freightdock"
cargo build -p freightdock --quiet

agent_image=${QA_AGENT_IMAGE:-}
if [ -z "$agent_image" ] && [ -f ../freightdock-agent/Dockerfile ]; then
  say "building the agent's image from ../freightdock-agent"
  agent_image=freightdock-agent:shots
  docker build -q -t "$agent_image" ../freightdock-agent >/dev/null || fail "the agent's image would not build"
elif [ -z "$agent_image" ]; then
  agent_image=ghcr.io/freightdock/agent:$version
  docker image inspect "$agent_image" >/dev/null 2>&1 || docker pull --quiet "$agent_image" >/dev/null || fail "could not pull $agent_image"
fi

say "starting four throwaway Dockers ($dind_image): $hosts"
docker image inspect "$dind_image" >/dev/null 2>&1 || docker pull --quiet "$dind_image" >/dev/null
docker network create "$name" >/dev/null
# The agents dial Freightdock on this network's gateway, which is this machine as the network sees it: the agent
# endpoint listens there and nowhere else.
gateway=$(docker network inspect -f '{{(index .IPAM.Config 0).Gateway}}' "$name")
[ -n "$gateway" ] || fail "the network $name has no gateway"
docker run -d --privileged --name "$name-local" --hostname local --network "$name" -e DOCKER_TLS_CERTDIR= \
  -p 127.0.0.1::2375 "$dind_image" --host=tcp://0.0.0.0:2375 --host=unix:///var/run/docker.sock >/dev/null
for n in node-1 node-2; do
  docker run -d --privileged --name "$name-$n" --hostname "$n" --network "$name" -e DOCKER_TLS_CERTDIR= \
    "$dind_image" --host=unix:///var/run/docker.sock >/dev/null
done
# Docker's own TLS: the dind makes a CA, its server certificate (for localhost and 127.0.0.1 among others) and a client
# certificate in /certs/client, which Freightdock is given.
docker run -d --privileged --name "$name-docker-tls" --hostname docker-tls --network "$name" -e DOCKER_TLS_CERTDIR=/certs \
  -p 127.0.0.1::2376 "$dind_image" >/dev/null
for h in $hosts; do
  for i in $(seq 1 60); do
    docker exec "$name-$h" docker -H unix:///var/run/docker.sock info >/dev/null 2>&1 && break
    [ "$i" -eq 60 ] && { docker logs --tail 30 "$name-$h" >&2; fail "the Docker $h did not come up within a minute"; }
    sleep 1
  done
done
for i in $(seq 1 30); do
  docker exec "$name-docker-tls" test -s /certs/client/key.pem && break
  [ "$i" -eq 30 ] && fail "the TLS Docker made no client certificate"
  sleep 1
done
for n in node-1 node-2; do
  docker save "$agent_image" | docker exec -i "$name-$n" docker load -q >/dev/null || fail "the agent image would not load into $n"
done
lport=$(docker port "$name-local" 2375/tcp | head -n 1 | sed 's/.*://')
tport=$(docker port "$name-docker-tls" 2376/tcp | head -n 1 | sed 's/.*://')
[ -n "$lport" ] && [ -n "$tport" ] || fail "the Dockers' ports are not published"
mkdir -p "$data/tls"
for f in ca cert key; do docker exec "$name-docker-tls" cat "/certs/client/$f.pem" >"$data/tls/$f.pem"; done

port=$(free_port)
aport=$(free_port)
while [ "$aport" = "$port" ]; do aport=$(free_port); done
setup_token=$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')
say "Freightdock on 127.0.0.1:$port, agents on $gateway:$aport, data in $data"
FREIGHTDOCK_SETUP_TOKEN="$setup_token" FREIGHTDOCK_DATA_DIR="$data/data" FREIGHTDOCK_BIND="127.0.0.1:$port" \
  FREIGHTDOCK_AGENT_BIND="$gateway:$aport" FREIGHTDOCK_AGENT_ADDRESS="$gateway:$aport" FREIGHTDOCK_TLS=off \
  FREIGHTDOCK_DOCKER_HOST="tcp://127.0.0.1:$lport" FREIGHTDOCK_COMPOSE="$compose" RUST_LOG="freightdock=info,warn" \
  target/debug/freightdock >"$data/freightdock.log" 2>&1 &
pid=$!

SHOTS_BASE="http://127.0.0.1:$port" SHOTS_SETUP_TOKEN="$setup_token" SHOTS_DIND="$name" SHOTS_AGENT_IMAGE="$agent_image" \
  SHOTS_AGENT_SERVER="$gateway:$aport" SHOTS_TLS_URL="tcp://127.0.0.1:$tport" SHOTS_TLS_DIR="$data/tls" \
  SHOTS_OUT="${SHOTS_OUT:-docs/screenshots}" node qa/screenshots.mjs \
  || fail "the screenshots were not taken"
kill -0 "$pid" 2>/dev/null || fail "Freightdock stopped while the pictures were taken"
say "done: look at them before committing them"
