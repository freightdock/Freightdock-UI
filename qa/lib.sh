# What qa/run.sh and qa/screenshots.sh share: their messages, a free port, and the Docker Compose the image pins.
# Sourced, never run; the script that sources it is in the repository's root and sets `QA_NAME` for its messages.

say() { printf '%s: %s\n' "${QA_NAME:-qa}" "$*" >&2; }
fail() { say "FAILED: $*"; exit 1; }

# A port nothing listens on, well away from the ones people use (7443 included).
free_port() {
  local p
  for _ in $(seq 1 50); do
    p=$((20000 + RANDOM % 30000))
    if ! (exec 3<>"/dev/tcp/127.0.0.1/$p") 2>/dev/null; then echo "$p"; return; fi
  done
  fail "no free port found"
}

# The Compose the image bundles (Dockerfile: COMPOSE_VERSION and its checksums), for Freightdock on this machine:
# fetched once into the cache and checked by its SHA-256. Sets $compose.
fetch_compose() {
  compose_version=5.6.1
  case "$(uname -m)" in
    x86_64) compose_arch=x86_64; compose_sum=47737f4236e6d3ccea7291995203f8f018058783b4c3a9469119811a0b5db8c0 ;;
    aarch64|arm64) compose_arch=aarch64; compose_sum=42cfa75f51ad0ccf290bad37899e0038a8c10a0069e85233ecc1d53e3ff1347d ;;
    *) fail "no Compose pinned for $(uname -m)" ;;
  esac
  compose="${XDG_CACHE_HOME:-$HOME/.cache}/freightdock-qa/docker-compose-$compose_version-$compose_arch"
  if ! echo "$compose_sum  $compose" | sha256sum -c - >/dev/null 2>&1; then
    say "fetching Docker Compose $compose_version"
    mkdir -p "$(dirname "$compose")"
    curl -fsSL -o "$compose.part" "https://github.com/docker/compose/releases/download/v$compose_version/docker-compose-linux-$compose_arch" \
      || fail "could not fetch Docker Compose $compose_version"
    echo "$compose_sum  $compose.part" | sha256sum -c - >/dev/null || { rm -f "$compose.part"; fail "Docker Compose $compose_version does not match its checksum"; }
    chmod 0755 "$compose.part" && mv "$compose.part" "$compose"
  fi
}
