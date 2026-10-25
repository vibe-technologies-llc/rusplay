---
name: commit
description: Commit Rusplay changes with the project's message style. Use whenever asked to commit or push.
---

# Commit

1. Run the `check` skill. Do not commit a red tree.
2. Make sure the commit carries its documentation: rules files made false by the change are
   updated, and implemented roadmap items are deleted from `docs/*.md` (see
   `.claude/rules/roadmap.md`).
3. Message: `type: summary`, lowercase, imperative, no trailing period. `type` is one of `feat`,
   `bugfix`, `chore`, `refactor`, `perf`, `docs`, `test`. A body is optional and explains why.
4. No `Co-Authored-By` or any other trailer or attribution line.
5. Push only when asked.
