---
name: "reqs bizreq"
description: "bizreq for reqs"
children:
  reqpaths: []
---

# Business requirements — project-wide

Global business requirements for **Sample Ledger**: rules that apply across the
WHOLE project. Component-local requirements stay beside their component
(`<component>/*.bizreq.iter.md`); anything that spans components belongs here.

This file's location is the `global_bizreq_path` setting in
`.iter/.engine/config.json` (default `{codepath}/reqs/bizreq.iter.md`). Agents get
the resolved path as `$ITER_BIZREQ`. The engine surfaces this file and the global
techreq to every agent automatically — exactly these two files, never a directory
scan.

This file needs no frontmatter — it is a plain context doc, never a map node.

Sample Ledger is a tiny personal money log. A person records amounts they spent
or received, each with a short note, and later asks what the running total is and
what the individual records were.

- **BR-1** — A person can record one money movement at a time: an amount and a
  short note describing it. Money coming in is positive, money going out is
  negative.
- **BR-2** — Every recorded movement is kept permanently and in the order it was
  recorded. Nothing already recorded is ever silently changed or dropped.
- **BR-3** — A person can ask for the running total of everything recorded so
  far, and get one number back.
- **BR-4** — A person can ask for a readable report: every movement in the order
  it happened, followed by the running total.
- **BR-5** — A person can ask for the same report as a comma-separated file so it
  can be opened in a spreadsheet.
- **BR-6** — When a person asks for something the system does not understand, it
  says so in one plain sentence and records nothing. A confusing request must
  never leave a half-written record behind.
- **BR-7** — Asking for a report on an empty log is a normal thing to do, not an
  error: the report comes back empty with a total of zero.
- **BR-8** — A person can ask separately for what came in and what went out, not
  only the single net total. Seeing "you received this much, you spent this much"
  is what makes the net number mean anything. *(Not built yet — the seed work
  items in the queue cover it.)*
