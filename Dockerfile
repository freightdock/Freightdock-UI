# Freightdock's image: the binary, Docker's own CLI (pinned by its image's digest; `docker stack deploy` for Swarm stacks)
# and the Compose plugin (a pinned release, checked by its SHA-256), which Compose stacks are deployed with.
FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock CHANGELOG.md ./
COPY crates crates
COPY web web
# Cargo's registry and git checkouts (the tunnel protocol comes from the Agent repository at its tag) and the target
# folder are BuildKit caches, kept by the builder between builds, so a build compiles only what changed; the binary is
# copied out of the cache in the same step. A git dependency that is private on GitHub is fetched with the optional
# BuildKit secret git_deps_token (`--secret id=git_deps_token,env=GIT_DEPS_TOKEN`), which reaches no layer.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/src/target,sharing=locked \
    --mount=type=secret,id=git_deps_token,required=false \
    set -eu; \
    if [ -s /run/secrets/git_deps_token ]; then \
      export CARGO_NET_GIT_FETCH_WITH_CLI=true GIT_CONFIG_COUNT=1 GIT_CONFIG_VALUE_0=https://github.com/ \
        GIT_CONFIG_KEY_0="url.https://x-access-token:$(cat /run/secrets/git_deps_token)@github.com/.insteadOf"; \
    fi; \
    cargo build --release --locked -p freightdock && cp target/release/freightdock /freightdock

# Docker's own CLI, pinned by the digest of its image (so what is built is checked against it): `docker stack deploy`
# (Swarm stacks) is the base CLI, not a plugin.
FROM docker:29.9.3-cli@sha256:2357639ad1ea7d0194134bf3292622fd4e562483966ed674e5d7c945b5f3b28d AS docker

# Docker Compose, a pinned release checked against its published SHA-256 (qa/lib.sh pins the same one).
FROM debian:bookworm-slim AS compose
ARG TARGETARCH
ARG COMPOSE_VERSION=5.6.1
ARG COMPOSE_SHA256_AMD64=47737f4236e6d3ccea7291995203f8f018058783b4c3a9469119811a0b5db8c0
ARG COMPOSE_SHA256_ARM64=42cfa75f51ad0ccf290bad37899e0038a8c10a0069e85233ecc1d53e3ff1347d
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/*
RUN set -eu; \
    case "${TARGETARCH:-amd64}" in \
      amd64) arch=x86_64; sum=$COMPOSE_SHA256_AMD64 ;; \
      arm64) arch=aarch64; sum=$COMPOSE_SHA256_ARM64 ;; \
      *) echo "no Compose pinned for $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    curl -fsSL -o /docker-compose "https://github.com/docker/compose/releases/download/v${COMPOSE_VERSION}/docker-compose-linux-${arch}"; \
    echo "${sum}  /docker-compose" | sha256sum -c -; \
    chmod 0755 /docker-compose

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates git tzdata && rm -rf /var/lib/apt/lists/*
COPY --from=docker /usr/local/bin/docker /usr/local/bin/docker
COPY --from=compose /docker-compose /usr/local/libexec/docker/cli-plugins/docker-compose
COPY --from=build /freightdock /usr/local/bin/freightdock
# The agent image Freightdock's "add a host" line names: CI gives the agent it published beside this image, same tag.
ARG AGENT_IMAGE=freightdock-agent:latest
ENV FREIGHTDOCK_DATA_DIR=/data FREIGHTDOCK_BIND=0.0.0.0:7443 FREIGHTDOCK_COMPOSE=/usr/local/libexec/docker/cli-plugins/docker-compose \
    FREIGHTDOCK_AGENT_IMAGE=$AGENT_IMAGE
VOLUME /data
# 7443: the web UI and API; 7444: the agent endpoint, TLS from Freightdock's own CA (FREIGHTDOCK_AGENT_BIND).
EXPOSE 7443 7444
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s CMD ["freightdock", "healthcheck"]
ENTRYPOINT ["freightdock"]
