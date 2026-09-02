# Postwork step: git-pr

Open a pull request for the current branch using the `gh` CLI.

1. Confirm the branch is pushed (run the git-push step's logic first if needed).
2. Create the PR: `gh pr create --title "<imperative summary>" --body "<body>"`.
   - Title: same convention as commit summaries — imperative, ≤ 72 chars.
   - Body: what changed, why, how it was tested (test group + counts), and the line
     `iterloop: {workid}` at the bottom.
3. If a PR already exists for this branch, do not create a duplicate — report the
   existing PR's URL instead.
4. Report the PR URL.
