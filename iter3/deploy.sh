#!/usr/bin/env bash
# Deploy the iter V3 stack locally: release binaries -> iter3/bin/, then
# (re)start iter_data against the production DynamoDB tables (prefix iter3_).
# Creds come from the repo .env (AWS_*, ITER_ADMIN_PASSWORD, ITER_JWT_SECRET).
#
# Usage: iter3/deploy.sh [sqlite|dynamodb]   (default dynamodb)
set -euo pipefail

BACKEND="${1:-dynamodb}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$REPO/iter3/bin"
RUN="$REPO/iter3/run"
PORT="${ITER_DATA_PORT:-8300}"

mkdir -p "$BIN" "$RUN"

echo "[deploy] building release binaries"
(cd "$REPO" && ~/.cargo/bin/cargo build --release -p iter_data -p iter_engine)

# macOS: cp over a live executable poisons the codesign cache -> SIGKILL.
# Always rm+cp, never cp-over.
for b in iter_data iter_engine; do
  rm -f "$BIN/$b"
  cp "$REPO/target/release/$b" "$BIN/$b"
done
echo "[deploy] binaries -> $BIN"

# restart iter_data
if [ -f "$RUN/iter_data.pid" ] && kill -0 "$(cat "$RUN/iter_data.pid")" 2>/dev/null; then
  echo "[deploy] stopping running iter_data ($(cat "$RUN/iter_data.pid"))"
  kill "$(cat "$RUN/iter_data.pid")" || true
  sleep 1
fi

if [ "$BACKEND" = "dynamodb" ]; then
  ARGS=(--backend dynamodb --prefix iter3_)
else
  ARGS=(--backend sqlite --db "$RUN/iter3.db")
fi

nohup "$BIN/iter_data" "${ARGS[@]}" \
  --listen "127.0.0.1:$PORT" \
  --secret-file "$RUN/iter_data.secret" \
  --env-file "$REPO/.env" \
  --webui-dir "$REPO/iter3/webui" \
  > "$RUN/iter_data.log" 2>&1 &
echo $! > "$RUN/iter_data.pid"

for i in $(seq 1 90); do
  curl -sf "http://127.0.0.1:$PORT/health" >/dev/null 2>&1 && break
  [ "$i" = 90 ] && { echo "[deploy] iter_data did not come up"; tail -20 "$RUN/iter_data.log"; exit 1; }
  sleep 1
done
echo "[deploy] iter_data up: http://127.0.0.1:$PORT ($BACKEND) — log: $RUN/iter_data.log"
echo "[deploy] webui: http://127.0.0.1:$PORT/  (login: admin / ITER_ADMIN_PASSWORD from .env)"
