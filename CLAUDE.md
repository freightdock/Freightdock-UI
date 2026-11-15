# AGENTS.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Freightdock is the web UI of a Freightdock cluster: one Rust binary (axum + tokio + bundled SQLite) with its web UI
compiled into it, over the managers' admin API. The cluster itself is the Freightdock-Manager and Freightdock-Worker
repositories (the contract between them Freightdock-Contracts); a one-node cluster does everything a single host did,
so this UI has no Docker of its own ("Cluster" 10). GPL-3.0-only, no licence key, no node limit, no paid tier.

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
(`api::tests` has the helpers); the managers are faked in-process (`cluster::tests`, `replica::tests`), and the real
cluster is checked on the test cluster. A bug fixed is a bug that first gets a test reproducing it. Never delete a test to make the bar green.

## Commands

```sh
cargo test                                   # all unit and in-process API tests
cargo test -p freightdock oidc::                # one module's tests (a name filter: a path or part of a test name)
cargo test -p freightdock <name> -- --nocapture # one test, with its output
cargo clippy --all-targets                   # keep it clean
FREIGHTDOCK_DATA_DIR=./data FREIGHTDOCK_BIND=127.0.0.1:7443 FREIGHTDOCK_CLUSTER_API=http://127.0.0.1:7451 cargo run
                                             # serve (HTTPS, self-signed cert made in ./data/tls), beside a manager
FREIGHTDOCK_TLS=off FREIGHTDOCK_BIND=127.0.0.1:7080 cargo run           # plain HTTP, behind a proxy
qa/js-check.sh                               # node --check on every module and the i18n check, in a node container
qa/e2e.sh                                    # the end-to-end suite on a throwaway one-node cluster (Docker, Node, ../dist)
qa/screenshots.sh                            # docs/screenshots/, light and dark, from a throwaway cluster (and Chromium)
docker build -t freightdock .                # the image
```

- Debug builds read `web/` from disk (rust-embed), so UI edits need only a refresh; release builds embed it.
- The UI is vanilla ES modules: no build step, no npm, no CDN. `web/assets/finui/` is FinUI (`../finui`, the Washi look
  as it ships) and `web/assets/finmotion/` is FinMotion, both copied source we own; `web/assets/vendor/` holds the
  pre-built third-party files (CodeMirror), each with its licence. Change FinUI upstream first, then copy.
- Env: `FREIGHTDOCK_DATA_DIR` (default `/data`), `FREIGHTDOCK_BIND` (`0.0.0.0:7443`), `FREIGHTDOCK_TLS` (`on`/`off`),
  `FREIGHTDOCK_TLS_CERT` + `FREIGHTDOCK_TLS_KEY` (one's own, never touched), `FREIGHTDOCK_TLS_NAMES` (more names for Freightdock's
  own certificate), `FREIGHTDOCK_CLUSTER_API` (the managers' admin API, several comma separated),
  `FREIGHTDOCK_CLUSTER_TOKEN_FILE` (the API token's file), `FREIGHTDOCK_CLUSTER_CA_FILE` (the cluster CA, for
  `https://`), `FREIGHTDOCK_CLUSTER_TOKEN` and `FREIGHTDOCK_CLUSTER_CA` (the values, before the files: how the
  managers start the UI they run), `FREIGHTDOCK_SETUP_TOKEN` (the first run's setup token, at least 16 characters; else one is made and
  printed to the log, and `freightdock setup-token` prints it again), `RUST_LOG`.

## The big picture

ARCHITECTURE.md has it in full; the parts that take several files to see:

- **One router.** `api::router` (sign-in, setup, settings, audit, changelog, OIDC) merges `tokens`, `tls`, `users` and
  `cluster`; `main.rs` adds `/api/health` and falls back to `web::serve` (the embedded SPA, FinUI's CSS joined into
  `/assets/finui.css`, with the FinUI preset an administrator picked right after `tokens.css`, `appearance.rs`).
  Shared state is `App` (`Arc<AppState>`: db, limiter, sealer, oidc, the cluster).
- **One guard.** `access::routes::guard` holds every route to its line in `RULES` (Public, Session, Admin, or Stack / Node for operators) and
  refuses a route without one; `Authed` needs a session (and for a change the CSRF token and same `Origin`), `Admin`
  an administrator, `Peer` gives IP, user agent and origin. Every change calls `audit::record`. Errors are `ApiError`
  (an `anyhow` error becomes a generic 500).
- **The cluster.** Every `/api/cluster/...` route is one call to a manager's admin API (`cluster.rs`): several
  addresses, the next tried only when a connection could not be made, so a change is never sent twice; over HTTPS the
  cluster CA alone is trusted. The managers' refusals keep their status and sentence.
- **The database is the managers'.** In a cluster `replica.rs` loads it from them before it opens, triggers write every
  row change to `_replog`, and `Db::call` pushes it as the next change; a Freightdock that lost the race is stale and
  exits with 75 to load again ("Cluster" 9).
- **Secrets at rest** go through `secret::Sealer` (`sealed1:` prefix, key in `<data>/secret.key`, replicated with the
  database), never stored plain and never sent back to the browser.
- **The UI** is `web/assets/js/`: `main.js` registers routes, `router.js` guards them (to `/setup` or `/login`), pages in
  `pages/` fetch through `api.js` and draw with FinUI's `h()`; Freightdock's own components use the `fd-` prefix.
- **Tests.** `api::tests::{app, call, post, json_body, signed_in}` build an in-process app (in-memory database, a test
  sealer); `access::api_tests::World` signs in an Administrator and plain users; `oidc.rs` has a mock provider for the
  full SSO flow.

## Patch notes

`CHANGELOG.md` is compiled into the binary and is the app's **Patch notes** page (`changelog.rs`, FinStats' format and
parser). Every change a person would notice gets its bullet there in the same commit, under the version being built:
Added, Changed, Security, Performance, Stability, Fixed or Removed, the change first and the reason after. A test fails
the build when the top entry is not this crate's version or breaks the format.

## Rules the code enforces

Each with its reason in DECISIONS.md.

- Migrations are immutable once released: append, never edit or reorder ("Database" 2).
- Every API route but status, setup, sign-in, health and the TLS CA needs a session, and every route has a rule in
  `access/routes.rs` ("Access" 5, 23); every change needs the CSRF token and a same-origin `Origin` ("Sign-in" 3, 4).
  Every change made through the API is written to the activity log ("Activity" 1).
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
