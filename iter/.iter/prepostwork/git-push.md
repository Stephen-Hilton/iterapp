# Postwork step: git-push

Push the current branch to its remote.

1. Identify the current branch. If it has no upstream, push with `-u origin <branch>`.
2. Run the push.
3. If the remote rejects (non-fast-forward), do NOT force-push. Run `git pull --rebase`
   once; if that rebases cleanly, push again. If conflicts appear, abort the rebase and
   report `POSTWORK-FAILED: push rejected, rebase conflicts` with the conflicting files.
4. Report the branch, remote, and the pushed commit range (e.g. `a1b2c3..d4e5f6`).
