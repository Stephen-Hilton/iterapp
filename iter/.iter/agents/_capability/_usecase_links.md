# Capability: edit a use-case's code node links (`iter usecase`)

Use-case nodes (`*.usecase.iter.md`) are GLOBAL objects living under `{usecases}`
(`globalinterfacedir`'s sibling setting `globalusecasedir`, exported as
`$ITER_USECASE_DIR`). They are linked ACROSS the code hierarchy rather than being
part of it, so a use-case never gets a code node file of its own.

Its `children.codenodes` is the REQUIRED link list: the `*.code.iter.md` files the
journey needs. An empty list is valid and marks work still to come.

Edit that list through the engine-owned path — never by hand, and regardless of
whether the use-case file is inside your lock scope:

    "$ITER_BIN" usecase --project "$ITER_PROJECT" --file <f> --add "<code file path>"
    "$ITER_BIN" usecase --project "$ITER_PROJECT" --file <f> --remove "<entry>"
    "$ITER_BIN" usecase --project "$ITER_PROJECT" --file <f> --list

`--add` takes a `*.code.iter.md` path or pattern and repeats; `--remove` takes an
exact existing entry and repeats; `--list` prints the resulting codenodes one per
line.

The one exception: the usecase agent owns the use-case files as its lock scope and
edits them directly. Every other agent uses this command.

**Link what was BUILT, not what was proposed.** When a code item finishes building a
node a use-case needs, link the new node file back into the use-case with `--add` at
that moment — a node that does not exist yet cannot be linked, and a link to
something never built is a lie the DAG carries forever.
