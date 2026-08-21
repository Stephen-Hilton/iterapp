# Business requirements — project-wide

Global business requirements: rules that apply across the WHOLE project. Component-
local requirements stay beside their component (`<component>/*.bizreq.iter.md`);
anything that spans components belongs here.

This file's location is the `global_bizreq_path` setting in
`.iter/.engine/config.json` (default `{codepath}/reqs/bizreq.iter.md`, where
`{codepath}` is the resolved code root). Agents get the resolved path as
`$ITER_BIZREQ` and its directory as `$ITER_REQS`; work-item context patterns can
reference that directory as `{reqs}`. The engine surfaces this file and the global
techreq to every agent automatically — exactly these two files, never a directory
scan, so other docs beside them stay out of agent context.

This file needs no frontmatter — it is a plain context doc, never a map node.

<!-- Requirements below: stable IDs (BR-1, BR-2, …), one requirement per bullet. -->
