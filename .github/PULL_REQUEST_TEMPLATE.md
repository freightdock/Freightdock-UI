## What changes, and why

<!-- One logical change. The title is a conventional commit subject: feat(stacks): ..., fix(auth): ... -->

## Checklist

- [ ] Written test first: a failing test, then the code ([CONTRIBUTING.md](../CONTRIBUTING.md)).
- [ ] `cargo test` and `cargo clippy --all-targets -- -D warnings` pass; `qa/js-check.sh` for UI changes.
- [ ] `qa/run.sh` passes, if it touches what a real Docker does.
- [ ] CONTEXT.md, ARCHITECTURE.md and DECISIONS.md describe the new code (in the same commit).
- [ ] A CHANGELOG.md bullet, if a person would notice it.
- [ ] Docs in `docs/` updated, if it changes how something is done.
- [ ] No secrets, no private addresses or host names (examples use 192.0.2.x and example.com); the hook is on.
- [ ] No em-dashes.
