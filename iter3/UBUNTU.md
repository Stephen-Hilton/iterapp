# Running iter_engine and pdy-dev on an Ubuntu server

Evaluated 2026-09-04 (Stephen's ask before bed). iter_data and iter_webui stay
central (the Lambda); only the engine and the pdy-dev checkout move.

## iter_engine on Ubuntu — no code changes needed

Build it there or cross-build here:

- **On the server**: `curl https://sh.rustup.rs -sSf | sh`, then
  `cargo build --release -p iter_engine`. The engine now uses rustls, so no
  libssl-dev / pkg-config is required. `build-essential` is enough.
- **From this Mac**: `cargo lambda build --release --x86-64 -p iter_engine --lambda-dir target/linux`
  produces `target/linux/iter_engine/bootstrap`, a plain x86-64 Linux ELF
  (verified with `file`). Rename it `iter_engine` and copy it over. Use
  `--arm64` for a Graviton/ARM box.

Runtime needs on the server:

- `claude` (Claude Code CLI, `npm i -g @anthropic-ai/claude-code`) on PATH.
  (No python3 any more: the statusline collector is gone as of 2026-09-04 —
  usage% comes from the session's stream-json event and a direct probe.)
- `git` with push credentials for the pdy-dev remote (the engine pulls,
  commits and pushes every run) and `bash`.
- Long-lived Claude tokens: run `claude setup-token` once per account (on any
  machine), put `<NAME>_TOKEN=…` lines in the engine's env file, and list the
  accounts by env-var name in the project's `accounts` (webui project gear).
  The engine routes each session to the chosen account's token and rotates
  per the switch/stop ladder — verified end to end in `iter3/e2e.sh`.
- `.iter/config.json` in the pdy-dev checkout: `data_url` = the Lambda URL,
  `engine_name` = a NEW name (e.g. `Ubuntu01`) — the engine self-registers
  on first start; then assign pdy-dev to it in the webui (engine gear →
  projects served → `{"pdy-dev":{"dirs":{"topdir":"/home/<you>/dev/pdy-dev/"}}}`).
  Give it its own engine token (`POST /api/users/<engine-user>/token`).
- Run it under systemd (`Restart=always`, `WorkingDirectory=` the checkout,
  `EnvironmentFile=` the env file). Nothing on the engine is macOS-specific;
  the only Darwin note in this repo (the in-place binary swap SIGKILL) does not
  apply on Linux.

## pdy-dev on Ubuntu — mostly environment, a few script fixes

What the repo needs from the host (from its tests, scripts and CLAUDE.md):

- **Rust toolchain** (rustup), **Docker** with a local Kubernetes and
  **kubectl** + **linkerd**: CLAUDE.md assumes Docker Desktop's built-in
  Kubernetes (context `docker-desktop`). On Ubuntu use `kind` or `k3d`
  (or microk8s) and point `KUBE_CONTEXT` at it — the deploy and test scripts
  already read `KUBE_CONTEXT` from the environment. The long-lived dev cluster
  `corridor-dev1` is remote (EKS) and unaffected.
- **python3** (the majority of test scripts), **jq**, **psql**
  (postgresql-client), **curl**, **aws** CLI with the deploy credentials.
- The **V2 `iter` binary** at `devops/iter` is a macOS arm64 Mach-O. V3 delegates
  `runtests`/`validate`/`markers`/`teststate`/`usecase` to it, so build a Linux
  copy from this repo's root crate (`cargo build --release` → `target/release/iter`)
  and place it at `devops/iter` on the server (it is a tracked file today; a
  per-OS path or a `.gitattributes` split would be cleaner).

Script fixes needed (grep `/Users/`, `/opt/homebrew` under `devops/script`):

- `forge_clauded_build_adj.sh`, `forge_clauded_test_all.sh`,
  `forge_seed_all_adj.sh`, `forge_service_all_adj.sh`,
  `forge_service_redeploy_all.sh` hard-code `REPO=/Users/stephen.hilton/dev/pdy4-dev`
  (an older checkout path even on the Mac) — derive it from the script's own
  location instead.
- `release.sh` and `count_sync.sh` prepend Homebrew's GNU coreutils/grep
  paths so GNU flags work on macOS; on Ubuntu those are the system tools, so
  the PATH lines can simply be guarded by `[ -d … ]` (they already loop over
  candidates, so they are harmless if left).
- `devops/venv_forge` is a checked-in Python venv built on macOS; recreate it
  on the server (`python3 -m venv devops/venv_forge && pip install -r …`).
- `clear_diskspace.sh` prunes Docker with Desktop-specific assumptions;
  review before running on the server.

Nothing in the Rust code, the testgroup layout, or the iter structure files
is macOS-specific. The practical order: build the Linux `iter_engine` and
V2 `iter`, install the toolchain + a local k8s, fix the five `REPO=` lines,
register `Ubuntu01`, assign pdy-dev to it, keep the Mac engine stopped (two
engines serving one checkout on different machines would each run `git pull`
against their own clone — fine — but the tree-lock semantics assume one
checkout per engine, so give each engine its own clone).
