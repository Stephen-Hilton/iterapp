# sampleV2 — the greenfield test environment

This directory deliberately starts (nearly) EMPTY: `iter init` scaffolded the
`.iter/` template plus the two structureV2 head files (`.iter/config.iter.json`
and `main.iter.md`), and nothing else. It exists to build test environments
from scratch with the V2 engine — point `iter start --project sampleV2` here
and grow a project greenfield.

The migration story is proven separately: `sampleV1/` is the V1 reference
project converted in place by `iter migratev2` (see git history for its V1
form).
