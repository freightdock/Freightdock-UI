# Contributing

Thank you for helping. Freightdock is the web UI of a Freightdock cluster: one Rust binary (axum, tokio, SQLite) with a
vanilla JavaScript UI built in, over the managers' admin API. The managers and workers are the Freightdock-Manager and
Freightdock-Worker repositories. This page is how work is done here. The map of the code is [CONTEXT.md](CONTEXT.md), how it fits together
[ARCHITECTURE.md](ARCHITECTURE.md), and every rule with its reason [DECISIONS.md](DECISIONS.md). Read the sections your
change touches before you start.

## First, once per clone

```sh
git config core.hooksPath .githooks
```

This turns on the pre-commit hook (`.githooks/pre-commit`). It refuses a commit that stages a database, a key, a
certificate, a `.env` file or a data folder, or adds a line with a token, a private key or a private address, then runs
[gitleaks](https://github.com/gitleaks/gitleaks) with `.gitleaks.toml` on what is staged: the installed `gitleaks`, or
its Docker image when it is not installed. CI runs gitleaks again on every push and pull request.

Names that must never be committed but cannot be written in a public file (your own domains, host names, user names)
go in `.git/info/private-patterns`, one extended regular expression a line. That file is inside `.git`, so it is never
committed; the hook reads it on every commit:

```
# .git/info/private-patterns
\bmy-nas\b
mydomain\.example
```

Examples in code and docs use the documentation addresses instead: `192.0.2.x` (RFC 5737) and `example.com` names
(RFC 2606).

## Test first

Every change is written test-first: red, green, refactor.

1. Write one small test for the next piece of behaviour, and watch it fail for the right reason.
2. Write the least code that makes it pass.
3. Clean up with the suite green.

Where tests go:

- Pure logic: a `#[cfg(test)]` module beside the code.
- Endpoints: in process, with `tower::ServiceExt::oneshot` (`api::tests` has the helpers: `app`, `call`, `post`,
  `json_body`, `signed_in`).
- Against the managers: a fake manager in process (`cluster::tests`, `replica::tests`); the real cluster is checked by
  hand on a test cluster until an end-to-end suite exists (TODO.md).

A bug fixed is a bug that first gets a test that shows it. Never delete a test to make the bar green.

## Running things

```sh
cargo test                                   # unit and in-process API tests
cargo test -p freightdock oidc::                # one module's tests
cargo clippy --all-targets -- -D warnings    # keep it clean
qa/js-check.sh                               # node --check on every UI module, in a node container
FREIGHTDOCK_DATA_DIR=./data FREIGHTDOCK_TLS=off FREIGHTDOCK_BIND=127.0.0.1:7080 cargo run   # serve, plain HTTP
```

A debug build reads `web/` from disk, so a UI change needs only a refresh. The UI has no build step, no npm and no CDN.
`web/assets/finui/` and `web/assets/finmotion/` are copies of FinUI and FinMotion: change them upstream first, then copy.

## The documents go with the code

CONTEXT.md, ARCHITECTURE.md and DECISIONS.md are kept current in the same commit as the code. A new or changed rule is
a DECISIONS item, added at the bottom of its section, or an old one marked **Superseded** with a pointer; numbers never
change. A guide in `docs/` that describes what you changed is updated in the same commit too.

Every change a person would notice gets a bullet in [CHANGELOG.md](CHANGELOG.md), under the version being built (Added,
Changed, Security, Performance, Stability, Fixed or Removed), the change first and the reason after. It is compiled into
the binary as the app's Patch notes, and a test fails when the format breaks.

## Rules the code keeps

- Migrations are never edited once released: append a new one.
- Every API route but status, setup, sign-in, health and a webhook's call needs a session, and every change the CSRF
  token and a same-site `Origin`. Every change made through the API goes to the activity log.
- Data reaches the DOM only as text (FinUI's `h()`), never through `innerHTML`.
- Every colour is a FinUI token.

## Commits

- Conventional-commit subjects with a scope where one fits: `feat(containers): ...`, `fix(auth): ...`, `docs: ...`,
  `chore: ...`. The subject says what changed, the body why. One logical change per commit.
- No generated-by or co-author lines for tools in commit messages.
- No em-dashes anywhere: code, comments, UI text, docs, commit messages. Rewrite the sentence instead.
- The name is **Freightdock** wherever a person reads it, and `freightdock` only where it is an identifier.

## Pull requests

Small and focused is easiest to review. The template has the checklist. For a large change, open an issue first so
we can agree on the shape before you spend the time.

Security problems are not for issues or pull requests: see [SECURITY.md](SECURITY.md). Everyone taking part follows the
[code of conduct](CODE_OF_CONDUCT.md).

By contributing you agree that your work is released under the [GNU General Public License v3.0 only](LICENSE).
