# CLAUDE.md

## Commits

- One line. No body.
- Conventional Commits: `type: subject`, or `type(scope): subject`.
- Types: `feat`, `fix`, `docs`, `refactor`, `perf`, `test`, `ci`, `build`, `chore`.
- Scope is a crate or an area when it helps: `ssh`, `s3`, `webdav`, `ui`, `core`.
- Subject in the imperative, lowercase, no trailing period, under 72 characters.
- One commit per meaningful change, not per step: squash the steps that got you there.
- Keep the `Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>` trailer.

```
feat(ssh): keep the sudo password in memory
fix: clear marks when the filter changes
docs: rewrite the README for a first release
chore: untrack docs/internals.md
```
