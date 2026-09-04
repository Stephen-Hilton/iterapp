# iter V3

Spec: `src/features/iter.v3.md`. Three components in one cargo workspace:

| crate / dir | what it is |
|---|---|
| `iter_core` | shared types, widget schema + validation, path-overlap and account-ladder logic |
| `iter_data` | the central API server: axum + storage trait (`sqlite` \| `dynamodb`), JWT auth, seq change-signals, central locks, versioned workitem writes, closed-item immutability (append-only `doc` rows + `reopen`), provenance-stamped detail rows; serves the webui statics |
| `iter_engine` | the local engine: seq-gated sync + full-refresh fallback, heartbeat, pick/lock/run/close, enforced git pre/postwork, close gate (deterministic checks + a haiku verifier before an item may close complete; bounces to queued, then question), helpers (`--adduser`, `--approve`, `--accounts`, `--question-widget`, `--doc`) |
| `webui/` | thin static client (login, projects, engines readout, workitem tiles with dependency indent, detail view, dynamic question widgets) |

## Run it

```bash
# local zero-config (sqlite):
iter3/deploy.sh sqlite

# production (DynamoDB, creds + ITER_ADMIN_PASSWORD + ITER_JWT_SECRET in repo .env):
iter3/deploy.sh            # defaults to dynamodb, prefix iter3_
```

Then open http://127.0.0.1:8300/ and sign in (`admin` / `ITER_ADMIN_PASSWORD` from `.env`).
Runtime state (binaries, pid, logs, live sample project) lives under `iter3/bin/` and `iter3/run/` (gitignored).

## Lambda (production)

```bash
pip3 install cargo-lambda          # once
iter3/deploy_lambda.sh             # build arm64 bootstrap, ensure role, create/update iter3_data, print the function URL
```

The function URL serves the embedded webui at `/` and the same API; `ITER_JWT_SECRET` in `.env` is shared with any local iter_data so tokens work on both.

## Migrating a V2 project

```bash
iter_data --backend dynamodb --prefix iter3_ --env-file .env \
  --migrate-v2 <project>/.iter/.engine/iter.db --migrate-project <name> \
  --migrate-topdir /abs/path/to/project --migrate-engine-topdir '~/dev/project/' \
  --migrate-agents-dir <project>/.iter/agents --migrate-mainfile <project>/main.iter.md [--migrate-dry-run] [--migrate-overwrite]
```

Add `ITER_USERNAME` / `ITER_PASSWORD` to `.env` to create (or re-password) the operator user as project admin in the same run.

## Engine setup

1. Admin creates the engine record (`PUT /api/engines/{name}`) with per-project `dirs`.
2. Admin creates an engine principal and mints a long-lived token:
   `PUT /api/users/engine01 {"role":"engine", ...}` then `POST /api/users/engine01/token`.
3. In the project topdir: `.iter/config.json` -> `{"data_url", "token_envar", "engine_name", "env_file"}`,
   and the token in the named env var in `.env`.
4. `iter_engine --config .iter/config.json` (add `--ticks N` for a bounded run).

## Tests

```bash
cargo test -p iter_core -p iter_data -p iter_engine   # unit
iter3/e2e.sh sqlite                                   # full stack, no AWS
iter3/e2e.sh dynamodb                                 # full stack against real DDB (prefix iter3_e2e_, auto-cleaned)
```

DynamoDB safety: table creation/deletion is prefix-guarded (`iter3_*` only; e2e uses
`iter3_e2e_*`); nothing outside those prefixes is ever touched.
