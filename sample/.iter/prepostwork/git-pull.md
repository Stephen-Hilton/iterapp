# Prework step: git-pull

Bring the working tree up to date before starting the main work.

1. Confirm you are inside a git repository at the codepath. If not, report "not a git
   repository" and continue — this step becomes a no-op.
2. Run `git pull --rebase` on the current branch.
3. If the pull succeeds, report the branch name and new HEAD short-hash.
4. If the pull produces a conflict: **abort the rebase** (`git rebase --abort`), leave
   the tree exactly as you found it, and report `PREWORK-FAILED: pull conflict` with the
   conflicting files listed. Do not attempt to resolve conflicts; do not proceed as if
   the pull worked.
