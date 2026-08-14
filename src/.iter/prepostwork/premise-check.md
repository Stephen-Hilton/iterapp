# Prework step: premise-check

A work item's text is prose frozen at the moment it was written. It runs later — often
hours later, and always against a tree the `git-pull` prework has just made newer than
the text. So the defect the item describes may already be fixed, and an item that reads
as well-evidenced can be entirely false by the time you get it.

This step is the mechanical re-check. Run it before the mainwork, and let its verdict
decide whether the mainwork happens at all.

## 1. Look for the PREMISE block

Read to the end of the mainwork text. An item written under the current convention ends
with:

    PREMISE (re-verify before mainwork):
    - authored-at: <short commit hash> on <branch>, <UTC timestamp>
    - holds-if: <shell command>   # expected: <exact output or exit code>

## 2. If there is a PREMISE block, run every holds-if line

Run each command exactly as written — do not improve it, do not substitute an
equivalent. The author chose that command because it distinguishes "defect still
present" from "defect already fixed"; a rewrite can quietly lose that.

Compare what you got against the `# expected:` comment on the same line. Paste the
actual output into your report either way; a premise that held is evidence too.

**All lines match what was expected** — the premise still holds. Say so, with the
outputs, and go on to the mainwork.

**Any line does not match** — the premise is stale. Do these three things:

1. Find what superseded it. Run, for the files the item names:

       git log --oneline <authored-at hash>..HEAD -- <file> <file> …

   If that hash is not an ancestor of HEAD (someone rebased, or the stamp was written
   by hand), use the timestamp instead:

       git log --oneline --since="<UTC timestamp>" -- <file> <file> …

2. Report, as the last line of this step:

       PREWORK-FAILED: premise stale

   followed by the holds-if line that failed, what it actually printed, and the
   superseding commits with their subjects.

3. **Do not perform the mainwork.** Nothing in the engine reads the words
   `PREWORK-FAILED` — it is a marker for whoever reads the item afterwards. What
   actually stops the work is you. Every step of a work item runs in one continuous
   agent session (each later step resumes the same session), so the mainwork turn
   arrives with this finding already in your context. Restate the stale verdict there
   and stop. Do not repair the item's reasoning, do not find a nearby thing to fix
   instead, and do not treat "the defect is already gone" as success — say the premise
   died and let a person or a fresh item decide what is actually needed now.

## 3. If there is no PREMISE block (an item written before this convention)

You still have two timestamps to work with: `times.added` on the work item, and the git
history of whatever files the item names. Run:

    git log --oneline --since="<times.added>" -- <file> <file> …

No commits — the files the item talks about have not moved since it was written, so its
premise is as good as it was. Proceed.

Commits listed — something changed under the item after it was written. Do not assume
it is stale, and do not assume it is fine: read those commits, and re-verify the item's
specific claims against the tree as it is now, before building anything on them. Report
what you checked and what you found.

## 4. When the item names no files and carries no premise

There is nothing to check. Say "no premise to check" and proceed — that is a valid
outcome, not an error. It is also worth one line in your report, because an item nobody
can re-verify is a weakness in the item.
