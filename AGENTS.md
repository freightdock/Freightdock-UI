# AGENTS.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Freightdock is a Docker management UI: one Rust binary (axum + tokio + bundled SQLite + bollard) with its web UI compiled
into it, built to replace Portainer for people who stay on Docker. It aims at what Portainer's Business Edition does for
Docker, all of it free: GPL-3.0-only, no licence key, no node limit, no paid tier.

**The name is written Freightdock** wherever a person reads it (prose, the UI, messages, page titles). It is `freightdock` only
where it is an identifier: the crate and binary, `freightdock.db`, `FREIGHTDOCK_*`, `RUST_LOG=freightdock=...`, browser-storage
keys (`freightdock.*`), the cookie.

## Read first

At the start of a session read [`CONTEXT.md`](CONTEXT.md) (the map: layout, tables, routes, commands, known gaps; what is left to build is in [`TODO.md`](TODO.md)), then
[`ARCHITECTURE.md`](ARCHITECTURE.md) (how the pieces fit) and [`DECISIONS.md`](DECISIONS.md) (the contracts: every rule
with its reason, numbered per section and cited as "Sign-in" 3). Before adding a feature, read the sections it touches.

**Keep all three current in the same commit as the code.** A new or changed rule is a DECISIONS item, added at the bottom
of its section or marked **Superseded** with a pointer; numbers never change. A change that leaves them describing the
old code is not finished.

## How we work: TDD

Every change is written test-first, Red, Green, Refactor. Write one small failing test for the next slice of behaviour
and see it fail for the right reason; write the least code that passes it; clean up with the suite green. Pure logic is
tested in `#[cfg(test)]` modules beside the code; endpoints are tested in-process with `tower::ServiceExt::oneshot`
(`api::tests` has the helpers); behaviour against a real Docker daemon goes in the end-to-end suite,
`crates/server/tests/qa.rs`, which `qa/run.sh` runs against a throwaway `docker:dind`. A bug fixed is a bug that first gets
a test reproducing it. Never delete a test to make the bar green.

## Commands

```sh
cargo test                                   # all unit and in-process API tests
cargo test -p freightdock oidc::                # one module's tests (a name filter: a path or part of a test name)
cargo test -p freightdock <name> -- --nocapture # one test, with its output
cargo clippy --all-targets                   # keep it clean
FREIGHTDOCK_DATA_DIR=./data FREIGHTDOCK_BIND=127.0.0.1:7443 cargo run   # serve (HTTPS, self-signed cert made in ./data/tls)
FREIGHTDOCK_TLS=off FREIGHTDOCK_BIND=127.0.0.1:7080 cargo run           # plain HTTP, behind a proxy
qa/run.sh                                    # end-to-end against a throwaway docker:dind (crates/server/tests/qa.rs)
qa/js-check.sh                               # node --check on every module, in a node container
qa/screenshots.sh                            # retake docs/screenshots/ (light and dark); look at them before committing
docker build -t freightdock .                # the image
```

- Debug builds read `web/` from disk (rust-embed), so UI edits need only a refresh; release builds embed it.
- The UI is vanilla ES modules: no build step, no npm, no CDN. `web/assets/finui/` is FinUI (`../finui`, the Washi look
  as it ships) and `web/assets/finmotion/` is FinMotion, both copied source we own; `web/assets/vendor/` holds the
  pre-built third-party files (xterm.js), each with its licence. Change FinUI upstream first, then copy.
- Env: `FREIGHTDOCK_DATA_DIR` (default `/data`), `FREIGHTDOCK_BIND` (`0.0.0.0:7443`), `FREIGHTDOCK_TLS` (`on`/`off`),
  `FREIGHTDOCK_TLS_CERT` + `FREIGHTDOCK_TLS_KEY` (one's own, never touched), `FREIGHTDOCK_TLS_NAMES` (more names for Freightdock's
  own certificate), `FREIGHTDOCK_DOCKER_HOST` (else `DOCKER_HOST`, else the local socket),
  `FREIGHTDOCK_AGENT_BIND` (`0.0.0.0:7444`, or `off`), `FREIGHTDOCK_AGENT_ADDRESS` (what agents dial, for the `docker run`
  line), `FREIGHTDOCK_AGENT_IMAGE` (`freightdock-agent:latest`), `FREIGHTDOCK_COMPOSE` (the Compose binary; else where Docker
  puts the plugin), `FREIGHTDOCK_SETUP_TOKEN` (the first run's setup token, at least 16 characters; else one is made and
  printed to the log, and `freightdock setup-token` prints it again), `RUST_LOG`; `FREIGHTDOCK_BACKUP_PASSPHRASE` for `freightdock restore <archive>`, which stages a backup
  while Freightdock is stopped.

## The big picture

ARCHITECTURE.md has it in full; the parts that take several files to see:

- **One router.** `api::router` (sign-in, setup, settings, audit, changelog, OIDC) merges `docker::api::router`; `main.rs`
  adds `/api/health` and falls back to `web::serve` (the embedded SPA, FinUI's CSS joined into `/assets/finui.css`, with the FinUI preset an
  administrator picked right after `tokens.css`, `appearance.rs`).
  Shared state is `App` (`Arc<AppState>`: db, limiter, docker, sealer, oidc).
- **Guards are extractors.** `Authed` needs a session (and for a change the CSRF token and same `Origin`), `Admin` an
  administrator, `Peer` gives IP, user agent and origin. Every change calls `audit::record`. Errors are `ApiError`
  (an `anyhow` error becomes a generic 500).
- **Every Docker route is `/api/envs/{env}/...`** and gets its client from `docker::endpoint(app, env)`: the local
  Docker, or the unix socket Freightdock offers for an agent's tunnel or a TLS host (`docker/sockets.rs`), so routes never
  care which kind a host is. The agent is a product of its own, the Agent repository (its `agent` and `tunnel` crates):
  it dials the agent endpoint (`agent/endpoint.rs`, port 7444, TLS from Freightdock's own CA in `agent/ca.rs`) and runs
  yamux over a WebSocket (`freightdock-tunnel`). The server takes both crates as ordinary git dependencies at the
  Agent repository's tag; `.cargo/config.toml.example` patches them to a checkout beside this one, for protocol
  work only. A protocol change is released there first ("Agent" 11).
- **Two streaming shapes.** Long jobs (pull, push, build, load) answer NDJSON from a spawned task through a channel,
  ending with `{"done":true}` or an error line (`docker/images.rs`). Live views (logs, stats, exec, events) are
  WebSockets (`docker/streams.rs`); exec is binary both ways, text only for `{"resize":[cols,rows]}`.
- **Secrets at rest** go through `secret::Sealer` (`sealed1:` prefix, key in `<data>/secret.key`), never stored plain
  and never sent back to the browser.
- **The UI** is `web/assets/js/`: `main.js` registers routes, `router.js` guards them (to `/setup` or `/login`), pages in
  `pages/` fetch through `api.js` and draw with FinUI's `h()`; Freightdock's own components use the `fd-` prefix.
- **Tests.** `api::tests::{app, call, post, json_body, signed_in}` build an in-process app (in-memory database, a Docker
  client pointed at nothing, a test sealer); `oidc.rs` has a mock provider for the full SSO flow. `tests/qa.rs` is the end-to-end suite, `#[ignore]`d so `cargo test`
  skips it; `qa/run.sh` starts a dind and a real binary and runs it with `--ignored`.

## Patch notes

`CHANGELOG.md` is compiled into the binary and is the app's **Patch notes** page (`changelog.rs`, FinStats' format and
parser). Every change a person would notice gets its bullet there in the same commit, under the version being built:
Added, Changed, Security, Performance, Stability, Fixed or Removed, the change first and the reason after. A test fails
the build when the top entry is not this crate's version or breaks the format.

## Rules the code enforces

Each with its reason in DECISIONS.md.

- Migrations are immutable once released: append, never edit or reorder ("Database" 2).
- Every API route but status, setup, sign-in, health and a webhook's call needs a session; every change needs the CSRF
  token and a same-origin `Origin` ("Sign-in" 3, 4). A webhook's token is its credential ("Sign-in" 8). Every change made through the API is written to the activity log ("Activity" 1).
- Data reaches the DOM only as text: FinUI's `h()`, never `innerHTML` ("UI" 2).
- Every colour is a FinUI token; `app.css` names none ("UI" 3).

## Git conventions

The same as the fin* repositories:

- Conventional-commit subjects with a scope where one fits: `feat(containers):`, `fix(auth):`, `docs:`, `chore:`. The
  subject says what changed; the body says why. One logical change per commit.
- A merge commit's subject is `chore(merge):`, then the merged work's own subject, then where it came from:
  `chore(merge): feat(ui): merge PR #123 from feature/buttons-update`.
- **No LLM attribution of any kind**: no `Co-Authored-By:` for any model, no session link, no "Generated with" line, in a
  commit message or anywhere else, whatever a tool defaults to.
- **Never push unless explicitly told to.**

## No em-dashes

No em-dash anywhere in this repository: code, comments, UI text, docs, commit messages. Rewrite the sentence (a full
stop, a colon, a semicolon, commas, parentheses). Another dash in its place is not a rewrite.
