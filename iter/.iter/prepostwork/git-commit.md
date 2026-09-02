# Postwork step: git-commit

Commit the work item's changes — and only the work item's changes.

1. Run `git status` and review every modified/untracked file.
2. Stage only files related to the work you just completed. Never `git add -A` blindly;
   leave unrelated modifications unstaged and mention them in your report.
3. Commit message convention:
   - First line: imperative summary, ≤ 72 chars (e.g. `add auth middleware to api router`).
   - Blank line, then 1–3 sentences of why, if the summary alone doesn't carry it.
   - Final line: `iterloop: {workid}` so commits trace back to their work item.
4. If there is nothing to commit, report "nothing to commit" — that is a valid outcome,
   not an error.
5. Report the commit hash and the list of committed files.
