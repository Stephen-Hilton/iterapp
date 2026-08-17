# Business requirements — project-wide

Global business requirements: rules that apply across the WHOLE project. Component-
local requirements stay beside their component (`<component>/*.bizreq.iter.md`);
anything that spans components belongs here.

This directory (`.iter/reqs/`) is iterapp's default project-wide reqs location. To
relocate it, add a `reqs:` key to the project-level marker's frontmatter (the
`level: project` `*.iter.md` at the code root), e.g. `reqs: docs/requirements` —
relative paths resolve against the code root, `~` works. Agents get the resolved
directory as `$ITER_REQS`; work-item context patterns can reference it as `{reqs}`
(e.g. `{reqs}/*.iter.md`). Files in this directory are surfaced to every agent
automatically.

This file needs no frontmatter — it is a plain context doc, never a map node.

<!-- Requirements below: stable IDs (BR-1, BR-2, …), one requirement per bullet. -->
