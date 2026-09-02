# SampleV3

Minimal sample project for the iter V3 stack (iter_data + iter_engine +
iter_webui). The E2E harness (`iter3/e2e.sh`) copies this directory to a
scratch location, git-inits it there, registers it as a project in iter_data,
and drives an engine through queued workitems against it.

Layout:
- `src/hello.sh` — the "application"
- `main.iter.md` — project head file (structureV2 files all survive in V3)
