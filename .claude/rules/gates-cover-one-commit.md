---
paths:
  - "**/scripts/**"
  - "**/tests/**"
  - "**/test/**"
  - "**/docs/RUNS.md"
  - "**/CHANGELOG.md"
---

# A gate covers one commit

A run that a release or merge decision rests on (a full suite, a corpus run, an
integration suite, a benchmark) covers the commit and settings it ran on, and
nothing else.

- Record the commit, the settings and the tool versions with the result. A run
  over a dirty tree records its diff, or it gates nothing.
- Any later change un-gates the tree, a one-line fix and a settings change
  included. Re-run the gate on the new commit, or run only the part the change
  can reach and say which part was skipped.
- Never carry a gate's result to a commit it did not run on, and never stitch
  one result from runs on different commits.
- A gate that did not finish, or whose first output does not prove it ran on
  the intended commit, is not a result: report it as not run.
