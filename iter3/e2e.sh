#!/usr/bin/env bash
# iter V3 end-to-end: iter_data (sqlite or dynamodb) + iter_engine + SampleV3.
# Usage: iter3/e2e.sh [sqlite|dynamodb]
# dynamodb mode needs AWS creds in the repo .env and uses table prefix
# "iter3_e2e_" so it can never touch pdy-dev's tables (pdy4-*) or the real
# iter3_ tables.
set -euo pipefail

BACKEND="${1:-sqlite}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
SCRATCH="${E2E_SCRATCH:-$(mktemp -d /tmp/iter3-e2e.XXXXXX)}"
PORT=$((18300 + RANDOM % 1000))
BASE="http://127.0.0.1:$PORT"
PROJECT="SampleV3"
PASS_ADMIN="e2e-admin-pw"
DATA_PID=""

say()  { printf '\033[36m[e2e]\033[0m %s\n' "$*"; }
fail() { printf '\033[31m[e2e] FAIL:\033[0m %s\n' "$*"; cleanup; exit 1; }
pass() { printf '\033[32m[e2e] ok:\033[0m %s\n' "$*"; }

cleanup() {
  [ -n "$DATA_PID" ] && kill "$DATA_PID" 2>/dev/null || true
  wait 2>/dev/null || true
}
trap cleanup EXIT

command -v jq >/dev/null || fail "jq is required"

say "backend=$BACKEND scratch=$SCRATCH port=$PORT"
mkdir -p "$SCRATCH"

# ---------- build ----------
say "building binaries"
(cd "$REPO" && ~/.cargo/bin/cargo build -p iter_data -p iter_engine >/dev/null 2>&1) || fail "cargo build"
DATA_BIN="$REPO/target/debug/iter_data"
ENGINE_BIN="$REPO/target/debug/iter_engine"

# ---------- start iter_data ----------
if [ "$BACKEND" = "dynamodb" ]; then
  ITER_ADMIN_PASSWORD=$PASS_ADMIN "$DATA_BIN" \
    --backend dynamodb --prefix iter3_e2e_ \
    --listen "127.0.0.1:$PORT" \
    --secret-file "$SCRATCH/jwt.secret" \
    --env-file "$REPO/.env" \
    --webui-dir "$REPO/iter3/webui" > "$SCRATCH/iter_data.log" 2>&1 &
else
  ITER_ADMIN_PASSWORD=$PASS_ADMIN "$DATA_BIN" \
    --backend sqlite --db "$SCRATCH/iter3.db" \
    --listen "127.0.0.1:$PORT" \
    --secret-file "$SCRATCH/jwt.secret" \
    --env-file /dev/null \
    --webui-dir "$REPO/iter3/webui" > "$SCRATCH/iter_data.log" 2>&1 &
fi
DATA_PID=$!

for i in $(seq 1 60); do
  curl -sf "$BASE/health" >/dev/null 2>&1 && break
  [ "$i" = 60 ] && { cat "$SCRATCH/iter_data.log"; fail "iter_data did not start"; }
  sleep 1
done
pass "iter_data up ($(curl -sf "$BASE/health" | jq -r .backend))"

# ---------- auth ----------
TOKEN=$(curl -sf -X POST "$BASE/auth/login" -H content-type:application/json \
  -d "{\"user\":\"admin\",\"password\":\"$PASS_ADMIN\"}" | jq -r .token)
[ -n "$TOKEN" ] && [ "$TOKEN" != null ] || fail "admin login"
AUTH=(-H "authorization: Bearer $TOKEN" -H content-type:application/json)
pass "admin login"

# bad password must fail
curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/auth/login" -H content-type:application/json \
  -d '{"user":"admin","password":"wrong"}' | grep -q 401 || fail "bad password accepted"
pass "bad password rejected"

# unauthenticated API must fail
curl -s -o /dev/null -w '%{http_code}' "$BASE/api/projects" | grep -q 401 || fail "unauthenticated read allowed"
pass "unauthenticated request rejected"

# ---------- users: engine principal ----------
curl -sf "${AUTH[@]}" -X PUT "$BASE/api/users/engine01" \
  -d '{"role":"engine","password":"unused-login-pw","email":""}' >/dev/null || fail "create engine user"
ENGINE_TOKEN=$(curl -sf "${AUTH[@]}" -X POST "$BASE/api/users/engine01/token" -d '{"ttl_days":365}' | jq -r .token)
[ -n "$ENGINE_TOKEN" ] && [ "$ENGINE_TOKEN" != null ] || fail "mint engine token"
pass "engine user + long-lived token"

# ---------- sample project on disk ----------
SAMPLE="$SCRATCH/sample"
cp -R "$REPO/sampleV3" "$SAMPLE"
(cd "$SAMPLE" && git init -q && git add -A && git -c user.email=e2e@iter -c user.name=e2e commit -qm seed)
pass "sample project git-initialized"

# ---------- project / agents / engine records ----------
curl -sf "${AUTH[@]}" -X PUT "$BASE/api/agents/code" \
  -d '{"desc":"coder","max":2,"timeoutsec":600,"model":"opus","promptbody":"You are the code agent."}' >/dev/null || fail "agent put"

curl -sf "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT" -d @- >/dev/null <<EOF || fail "project put"
{"desc":"sample project","state":"Running","gitrepo":"",
 "maxagents":{">98%":0,"else":2},
 "agents":{"code":{"max":1}},
 "failure":{"maxattempts":2,"first_retry_second":1,"retry_backoff_exponent":2},
 "engines":["Engine01"],"accounts":[]}
EOF

curl -sf "${AUTH[@]}" -X PUT "$BASE/api/engines/Engine01" -d @- >/dev/null <<EOF || fail "engine put"
{"host":"$(hostname)","state":"Stopped","ticksec":1,"full_refresh_minutes":360,
 "queuelock":{"retryms":50,"breaksec":60},
 "projects":{"$PROJECT":{"dirs":{"topdir":"$SAMPLE"}}}}
EOF
pass "project/agent/engine records created"

# versions rows should exist already
SEQ0=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/versions" | jq '[.[]|select(.table=="project")][0].seq')
[ "$SEQ0" -ge 1 ] || fail "versions seq missing after project write"
pass "versions seq bumping (project seq=$SEQ0)"

# ---------- workitems ----------
WA=$(curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems" \
  -d '{"name":"write A","agent":"exec","exec_shell":"echo A-done > out_a.txt; sleep 1","priority":3,"lockdirs":["{topdir}/"]}' | jq -r .id)
WB=$(curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems" \
  -d "{\"name\":\"write B after A\",\"agent\":\"exec\",\"exec_shell\":\"test -f out_a.txt && echo B-done > out_b.txt\",\"priority\":4,\"blockedby\":[\"$WA\"],\"lockdirs\":[\"{topdir}/\"]}" | jq -r .id)
WC=$(curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems" \
  -d '{"name":"write C","agent":"exec","exec_shell":"echo C-done > out_c.txt","priority":5,"lockdirs":["{topdir}/src/"]}' | jq -r .id)
[ -n "$WA" ] && [ -n "$WB" ] && [ -n "$WC" ] || fail "workitem creation"
pass "workitems queued: A=$WA B=$WB C=$WC"

# default state is queued; version is 1
ST=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$WA" | jq -r .state)
[ "$ST" = queued ] || fail "default create-state should be queued (got $ST)"

# request detail row + read back
curl -sf "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT/workitems/$WA/details/0" \
  -d '{"key":"request","valuetype":"text","value":"please write out_a.txt"}' >/dev/null || fail "detail put"
DK=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$WA/details" | jq -r '.[0].key')
[ "$DK" = request ] || fail "detail read"
pass "workitem detail rows"

# widget validation: bad widget must bounce with 400
CODE=$(curl -s -o /dev/null -w '%{http_code}' "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT/workitems/$WA/details/5" \
  -d '{"key":"question","valuetype":"json","value":{"title":"","fields":[{"key":"x","type":"nope"}]}}')
[ "$CODE" = 400 ] || fail "invalid widget accepted (HTTP $CODE)"
pass "malformed question widget bounced"

# versioned write conflict: stale expect_version -> 409
ITEM=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$WC")
V=$(echo "$ITEM" | jq -r .version)
echo "$ITEM" | jq '.priority=2' | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT/workitems/$WC?expect_version=$V" -d @- >/dev/null || fail "versioned write"
CODE=$(echo "$ITEM" | jq '.priority=9' | curl -s -o /dev/null -w '%{http_code}' "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT/workitems/$WC?expect_version=$V" -d @-)
[ "$CODE" = 409 ] || fail "stale write not rejected (HTTP $CODE)"
pass "versioned writes: fresh accepted, stale 409"

# ---------- central locks ----------
curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/locks/acquire" \
  -d '{"path":"{topdir}/x/","workid":"w-test","engine":"e2e","ttl_sec":60}' >/dev/null || fail "lock acquire"
CODE=$(curl -s -o /dev/null -w '%{http_code}' "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/locks/acquire" \
  -d '{"path":"{topdir}/x/","workid":"w-other","engine":"e2e","ttl_sec":60}')
[ "$CODE" = 409 ] || fail "second holder got the lock (HTTP $CODE)"
curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/locks/acquire" \
  -d '{"path":"{topdir}/x/","workid":"w-test","engine":"e2e","ttl_sec":60}' >/dev/null || fail "re-acquire by holder"
curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/locks/release" \
  -d '{"path":"{topdir}/x/","workid":"w-test"}' >/dev/null || fail "lock release"
pass "central lock: acquire / conflict / re-acquire / release"

# ---------- engine run ----------
mkdir -p "$SAMPLE/.iter"
cat > "$SAMPLE/.iter/config.json" <<EOF
{"data_url":"$BASE","token_envar":"ITER_ENGINE_TOKEN","engine_name":"Engine01","env_file":"$SAMPLE/.env"}
EOF
echo "ITER_ENGINE_TOKEN=$ENGINE_TOKEN" > "$SAMPLE/.env"

say "running engine (max 20 ticks @1s)"
(cd "$SAMPLE" && "$ENGINE_BIN" --config .iter/config.json --ticks 20 > "$SCRATCH/engine.log" 2>&1) || true

grep -q "start" "$SCRATCH/engine.log" || { cat "$SCRATCH/engine.log"; fail "engine never started an item"; }

for f in out_a.txt out_b.txt out_c.txt; do
  [ -f "$SAMPLE/$f" ] || { cat "$SCRATCH/engine.log"; fail "missing $SAMPLE/$f"; }
done
pass "all three workitems produced output (dependency B ran after A)"

for W in "$WA" "$WB" "$WC"; do
  ST=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$W" | jq -r .state)
  [ "$ST" = complete ] || fail "workitem $W state=$ST (expected complete)"
done
pass "all workitems complete in iter_data"

# response details written
RK=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$WA/details" | jq -r '[.[]|select(.key=="response")]|length')
[ "$RK" -ge 1 ] || fail "no response detail on A"
pass "response detail rows written"

# locks all released
NLOCKS=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/locks" | jq 'length')
[ "$NLOCKS" = 0 ] || fail "locks left behind: $NLOCKS"
pass "locks released"

# engine heartbeat + git postwork commit happened
LS=$(curl -sf "${AUTH[@]}" "$BASE/api/engines/Engine01" | jq -r .last_seen)
[ -n "$LS" ] && [ "$LS" != '""' ] || fail "no heartbeat"
NCOMMITS=$(cd "$SAMPLE" && git rev-list --count HEAD)
[ "$NCOMMITS" -ge 2 ] || fail "engine did not commit its work (commits=$NCOMMITS)"
pass "heartbeat written; git postwork committed ($NCOMMITS commits)"

# ---------- approval flow ----------
WD=$(curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems" \
  -d '{"name":"sensitive change","agent":"exec","exec_shell":"echo approved-ran > out_d.txt","state":"question","needs_approval":true}' | jq -r .id)
(cd "$SAMPLE" && ITER_ENGINE_TOKEN=$ENGINE_TOKEN "$ENGINE_BIN" --config .iter/config.json --adduser steve > "$SCRATCH/adduser.log" 2>&1) || { cat "$SCRATCH/adduser.log"; fail "--adduser"; }
grep -q "registered pubkey" "$SCRATCH/adduser.log" || { cat "$SCRATCH/adduser.log"; fail "pubkey not registered"; }
[ -f "$SAMPLE/.iter/users/steve.pem" ] || fail "keyfile missing"
grep -q "users/" "$SAMPLE/.iter/.gitignore" || fail ".iter/.gitignore missing users/ ignore"
(cd "$SAMPLE" && ITER_ENGINE_TOKEN=$ENGINE_TOKEN ITER_APPROVE_KEYPATH="$SAMPLE/.iter/users/steve.pem" \
  "$ENGINE_BIN" --config .iter/config.json --approve "${WD:0:12}" > "$SCRATCH/approve.log" 2>&1) || { cat "$SCRATCH/approve.log"; fail "--approve"; }
ST=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$WD" | jq -r .state)
[ "$ST" = queued ] || fail "approved item state=$ST (expected queued)"
pass "adduser + signed approval flow (item re-queued)"

# widget helper
echo '{"title":"t","fields":[{"key":"a","type":"text","value":""}]}' > "$SCRATCH/w.json"
"$ENGINE_BIN" --question-widget "$SCRATCH/w.json" | grep -q "OK" || fail "--question-widget valid case"
echo '{"title":"","fields":[]}' > "$SCRATCH/w2.json"
("$ENGINE_BIN" --question-widget "$SCRATCH/w2.json" || true) | grep -q "INVALID" || fail "--question-widget invalid case"
pass "--question-widget helper"

# webui served
curl -sf "$BASE/" | grep -q "ITER" || fail "webui not served"
pass "webui static page served"

# dynamodb mode: drop the isolated e2e tables so the next run starts clean.
# Deletion is prefix-guarded to iter3_e2e_ — it can never touch anything else.
if [ "$BACKEND" = "dynamodb" ] && command -v aws >/dev/null; then
  say "cleaning up iter3_e2e_* tables"
  set -a; grep -E '^AWS_' "$REPO/.env" > "$SCRATCH/aws.env" || true; . "$SCRATCH/aws.env"; set +a
  for t in $(aws dynamodb list-tables --output text --query 'TableNames[]' 2>/dev/null); do
    case "$t" in iter3_e2e_*) aws dynamodb delete-table --table-name "$t" >/dev/null 2>&1 || true;; esac
  done
fi

say "ALL E2E CHECKS PASSED ($BACKEND) — logs in $SCRATCH"
