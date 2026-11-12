# Freightdock's image: the cluster's UI, one binary. It talks to the managers' API only, so it needs no Docker CLI,
# no Compose and no Docker socket.
FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock CHANGELOG.md ./
COPY crates crates
COPY web web
# Cargo's registry and git checkouts and the target folder are BuildKit caches, kept by the builder between builds, so
# a build compiles only what changed; the binary is copied out of the cache in the same step. A git dependency that is private on GitHub is fetched with the optional
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

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates tzdata && rm -rf /var/lib/apt/lists/*
COPY --from=build /freightdock /usr/local/bin/freightdock
ENV FREIGHTDOCK_DATA_DIR=/data FREIGHTDOCK_BIND=0.0.0.0:7443
VOLUME /data
# 7443: the web UI and its API.
EXPOSE 7443
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s CMD ["freightdock", "healthcheck"]
ENTRYPOINT ["freightdock"]
