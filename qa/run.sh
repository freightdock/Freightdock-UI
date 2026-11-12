#!/usr/bin/env bash
# The end-to-end suite: a throwaway Docker (docker:dind, privileged, on a network of its own) and a Freightdock binary
# pointed at it, plain HTTP on a free port with a data folder of its own; then crates/server/tests/qa.rs drives Freightdock
# through its HTTP API (setup, pull, create, logs, history, console, attach, networks, edit with rollback, stop, remove,
# build, stacks, templates, webhooks, a registry with a password: push, browse, pull, and a pull refused without its
# credentials, API tokens and MCP, image updates against a registry in the dind, notifications to a listener, backups
# to a folder and to S3 (a MinIO beside the dind) and a restore, which has Freightdock start itself again, sign-in through
# LDAP (an OpenLDAP beside the dind, over LDAPS and StartTLS with a CA made here) and TOTP two-factor, then Swarm: the
# dind made a swarm of one, services, logs, secrets, configs, a stack, the node drained, and left again) and stops at
# the first step that does not hold. Everything it made is removed on the way out, whatever happened. Needs Docker,
# cargo and curl; the host's own containers are never touched. Docker Compose, which Freightdock deploys stacks with, is
# the release the image pins, fetched once into ~/.cache/freightdock-qa and checked by its SHA-256.
#
#   qa/run.sh                                the whole suite
#   QA_DIND_IMAGE=docker:28-dind qa/run.sh   against another Docker
#   QA_KEEP=1 qa/run.sh                      leave the dind and the data folder for a look afterwards
set -euo pipefail
cd "$(dirname "$0")/.."

dind_image=${QA_DIND_IMAGE:-docker:29-dind}
# An S3-compatible store for the backups' S3 destination: MinIO (MinIO's own images are no longer published).
s3_image=${QA_S3_IMAGE:-bitnamilegacy/minio:latest}
# A directory for the LDAP sign-in.
ldap_image=${QA_LDAP_IMAGE:-osixia/openldap:1.5.0}
name=freightdock-qa-$$
data=$(mktemp -d "${TMPDIR:-/tmp}/freightdock-qa.XXXXXX")
pid=

# say, fail, free_port and the pinned Compose ($compose), shared with qa/screenshots.sh.
. qa/lib.sh

cleanup() {
  status=$?
  [ -n "$pid" ] && kill "$pid" 2>/dev/null && wait "$pid" 2>/dev/null || true
  if [ $status -ne 0 ] && [ -f "$data/freightdock.log" ]; then
    say "the last of Freightdock's log:"
    tail -n 40 "$data/freightdock.log" >&2 || true
  fi
  if [ -n "${QA_KEEP:-}" ]; then
    say "kept: containers $name-dind, $name-s3 and $name-ldap, network $name, data $data"
  else
    docker rm -f -v "$name-dind" >/dev/null 2>&1 || true
    docker rm -f -v "$name-s3" >/dev/null 2>&1 || true
    docker rm -f -v "$name-ldap" >/dev/null 2>&1 || true
    docker network rm "$name" >/dev/null 2>&1 || true
    rm -rf "$data"
  fi
  exit $status
}
trap cleanup EXIT
trap 'exit 130' INT TERM

fetch_compose

say "building Freightdock and the suite"
cargo build -p freightdock --quiet
cargo test -p freightdock --test qa --no-run --quiet 2>/dev/null || cargo test -p freightdock --test qa --no-run

say "starting $dind_image as $name-dind"
docker image inspect "$dind_image" >/dev/null 2>&1 || docker pull --quiet "$dind_image" >/dev/null
docker network create "$name" >/dev/null
# The registries the suite runs inside the dind (one with a password, then the update check's) are published on the
# same port here and there, so both the dind's Docker and Freightdock reach them as 127.0.0.1:$rport.
rport=$(free_port)
docker run -d --privileged --name "$name-dind" --network "$name" -e DOCKER_TLS_CERTDIR= \
  -p 127.0.0.1::2375 -p "127.0.0.1:$rport:$rport" "$dind_image" --host=tcp://0.0.0.0:2375 >/dev/null
for i in $(seq 1 60); do
  docker exec "$name-dind" docker info >/dev/null 2>&1 && break
  [ "$i" -eq 60 ] && { docker logs --tail 30 "$name-dind" >&2; fail "the dind Docker did not come up within a minute"; }
  sleep 1
done
dport=$(docker port "$name-dind" 2375/tcp | head -n 1 | sed 's/.*://')
[ -n "$dport" ] || fail "the dind Docker's port is not published"

say "starting $s3_image as $name-s3"
docker image inspect "$s3_image" >/dev/null 2>&1 || docker pull --quiet "$s3_image" >/dev/null
docker run -d --name "$name-s3" --network "$name" -p 127.0.0.1::9000 \
  -e MINIO_ROOT_USER=qa-access-key -e MINIO_ROOT_PASSWORD=qa-secret-key-1234 "$s3_image" >/dev/null
sport=$(docker port "$name-s3" 9000/tcp | head -n 1 | sed 's/.*://')
[ -n "$sport" ] || fail "the S3 store's port is not published"

say "starting $ldap_image as $name-ldap"
# Its certificate, from a CA made for this run, names 127.0.0.1, where Freightdock reaches it. The DH parameters are RFC
# 7919's ffdhe2048, written at once, which spares the image the minute it takes to make its own (GnuTLS, which slapd
# uses there, refuses the quick DSA-style ones).
certs="$data/ldap-certs"
mkdir -p "$certs"
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 2 -subj "/CN=Freightdock QA LDAP CA" \
  -keyout "$certs/ca.key" -out "$certs/ca.crt" 2>/dev/null || fail "openssl could not make the LDAP CA"
openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -subj "/CN=localhost" -keyout "$certs/ldap.key" -out "$certs/ldap.csr" 2>/dev/null
printf 'subjectAltName=DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth\n' >"$certs/ext.cnf"
openssl x509 -req -in "$certs/ldap.csr" -CA "$certs/ca.crt" -CAkey "$certs/ca.key" -CAcreateserial -days 2 \
  -extfile "$certs/ext.cnf" -out "$certs/ldap.crt" 2>/dev/null || fail "openssl could not sign the LDAP certificate"
openssl genpkey -genparam -algorithm DH -pkeyopt group:ffdhe2048 -out "$certs/dhparam.pem" 2>/dev/null \
  || fail "openssl could not write the DH parameters"
chmod 0644 "$certs"/*
docker image inspect "$ldap_image" >/dev/null 2>&1 || docker pull --quiet "$ldap_image" >/dev/null
docker run -d --name "$name-ldap" --network "$name" -p 127.0.0.1::389 -p 127.0.0.1::636 \
  -v "$certs:/container/service/slapd/assets/certs" -e LDAP_TLS_CRT_FILENAME=ldap.crt -e LDAP_TLS_KEY_FILENAME=ldap.key \
  -e LDAP_TLS_CA_CRT_FILENAME=ca.crt -e LDAP_TLS_DH_PARAM_FILENAME=dhparam.pem -e LDAP_TLS_VERIFY_CLIENT=never \
  -e LDAP_ADMIN_PASSWORD=qa-ldap-admin -e LDAP_READONLY_USER=true -e LDAP_READONLY_USER_USERNAME=freightdock \
  -e LDAP_READONLY_USER_PASSWORD=qa-ldap-readonly "$ldap_image" --copy-service >/dev/null
# The image starts slapd once to set itself up and then again for good, with TLS: a directory that answers on ldap://
# alone may be the first one, about to stop. Ready is both, ldap:// and ldaps://; and the seed is tried again while the
# server cannot be reached at all (ldapadd's 255), which a slow machine still meets in the hand-over.
ldap_ready() {
  docker exec "$name-ldap" ldapsearch -x -H ldap://localhost -D cn=admin,dc=example,dc=org -w qa-ldap-admin -b dc=example,dc=org -s base >/dev/null 2>&1 \
    && docker exec -e LDAPTLS_REQCERT=never "$name-ldap" ldapsearch -x -H ldaps://localhost -D cn=admin,dc=example,dc=org -w qa-ldap-admin -b dc=example,dc=org -s base >/dev/null 2>&1
}
for i in $(seq 1 180); do
  ldap_ready && break
  [ "$i" -eq 180 ] && { docker logs --tail 30 "$name-ldap" >&2; fail "the LDAP directory did not come up within three minutes"; }
  sleep 1
done
for i in $(seq 1 30); do
  st=0
  docker exec -i "$name-ldap" ldapadd -x -H ldap://localhost -D cn=admin,dc=example,dc=org -w qa-ldap-admin <qa/ldap/seed.ldif >/dev/null || st=$?
  [ "$st" -eq 0 ] && break
  [ "$st" -ne 255 ] || [ "$i" -eq 30 ] && fail "the LDAP directory would not take qa/ldap/seed.ldif"
  sleep 1
  ldap_ready || true
done
ldaps_port=$(docker port "$name-ldap" 636/tcp | head -n 1 | sed 's/.*://')
ldap_port=$(docker port "$name-ldap" 389/tcp | head -n 1 | sed 's/.*://')
[ -n "$ldaps_port" ] && [ -n "$ldap_port" ] || fail "the LDAP directory's ports are not published"

port=$(free_port)
# The agent endpoint on a free port too: its default, 7444, is a running Freightdock's. No two of the ports the same.
while [ "$port" = "$rport" ]; do port=$(free_port); done
aport=$(free_port)
while [ "$aport" = "$port" ] || [ "$aport" = "$rport" ]; do aport=$(free_port); done
say "Freightdock on 127.0.0.1:$port (agents on $aport), Docker at tcp://127.0.0.1:$dport, data in $data"
# The first run's setup token, given rather than read from the log ("Sign-in" 14); the suite sends it to /api/setup.
setup_token=$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')
FREIGHTDOCK_SETUP_TOKEN="$setup_token" FREIGHTDOCK_DATA_DIR="$data/data" FREIGHTDOCK_BIND="127.0.0.1:$port" FREIGHTDOCK_AGENT_BIND="127.0.0.1:$aport" FREIGHTDOCK_TLS=off \
  FREIGHTDOCK_DOCKER_HOST="tcp://127.0.0.1:$dport" FREIGHTDOCK_COMPOSE="$compose" RUST_LOG="freightdock=info,warn" \
  target/debug/freightdock >"$data/freightdock.log" 2>&1 &
pid=$!

# The suite starts one project itself, as somebody on the command line would, to adopt it: with the same Compose, at
# the dind, in a folder of the data folder that goes with it.
FREIGHTDOCK_QA_URL="http://127.0.0.1:$port" FREIGHTDOCK_QA_SETUP_TOKEN="$setup_token" FREIGHTDOCK_QA_COMPOSE="$compose" FREIGHTDOCK_QA_DOCKER="tcp://127.0.0.1:$dport" \
  FREIGHTDOCK_QA_SCRATCH="$data/elsewhere" FREIGHTDOCK_QA_REGISTRY_PORT="$rport" FREIGHTDOCK_QA_S3="http://127.0.0.1:$sport" \
  FREIGHTDOCK_QA_LDAPS="ldaps://127.0.0.1:$ldaps_port" FREIGHTDOCK_QA_LDAP="ldap://127.0.0.1:$ldap_port" FREIGHTDOCK_QA_LDAP_CA="$certs/ca.crt" \
  cargo test -p freightdock --quiet --test qa -- --ignored --nocapture \
  || fail "the end-to-end suite (crates/server/tests/qa.rs) did not pass"
kill -0 "$pid" 2>/dev/null || fail "Freightdock stopped during the suite"
say "every step passed"
