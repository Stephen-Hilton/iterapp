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

drop_e2e_tables() {
  # dynamodb mode: drop the isolated e2e tables so the next run starts clean —
  # on success AND on failure. Prefix-guarded to iter3_e2e_; nothing else is touched.
  if [ "$BACKEND" = "dynamodb" ] && command -v aws >/dev/null; then
    set -a; grep -E '^AWS_' "$REPO/.env" > "$SCRATCH/aws.env" || true; . "$SCRATCH/aws.env"; set +a
    for t in $(aws dynamodb list-tables --output text --query 'TableNames[]' 2>/dev/null); do
      case "$t" in iter3_e2e_*) aws dynamodb delete-table --table-name "$t" >/dev/null 2>&1 || true;; esac
    done
  fi
}
cleanup() {
  [ -n "${PROBE_PID:-}" ] && kill "$PROBE_PID" 2>/dev/null || true
  [ -n "$DATA_PID" ] && kill "$DATA_PID" 2>/dev/null || true
  wait 2>/dev/null || true
  drop_e2e_tables
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
{"host":"$(hostname)","state":"Stopped","ticksec":1,"full_refresh_minutes":360,"probe_stale_min":0,
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

# ---------- deep dependencies + failed blocker + drain settle (2026-09-04) ----------
DP=$(curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems" \
  -d '{"name":"deep plan","agent":"exec","exec_shell":"true","state":"complete","lockdirs":["{topdir}/dp/"]}' | jq -r .id)
DC=$(curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems" \
  -d "{\"name\":\"deep child\",\"agent\":\"exec\",\"exec_shell\":\"echo child > out_dchild.txt\",\"createdby\":\"$DP\",\"state\":\"parked\",\"lockdirs\":[\"{topdir}/dc/\"]}" | jq -r .id)
DD=$(curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems" \
  -d "{\"name\":\"deep dependent\",\"agent\":\"exec\",\"exec_shell\":\"echo dep > out_ddep.txt\",\"blockedby\":[\"$DP\"],\"lockdirs\":[\"{topdir}/dd/\"]}" | jq -r .id)
DS=$(curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems" \
  -d "{\"name\":\"shallow dependent\",\"agent\":\"exec\",\"exec_shell\":\"echo shallow > out_dshallow.txt\",\"blockedby\":[\"$DP\"],\"blockedby_shallow\":true,\"lockdirs\":[\"{topdir}/ds/\"]}" | jq -r .id)
DF=$(curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems" \
  -d '{"name":"failed blocker","agent":"exec","exec_shell":"false","state":"failed","lockdirs":["{topdir}/df/"]}' | jq -r .id)
DG=$(curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems" \
  -d "{\"name\":\"depends on failed\",\"agent\":\"exec\",\"exec_shell\":\"echo never > out_dnever.txt\",\"blockedby\":[\"$DF\"],\"lockdirs\":[\"{topdir}/dg/\"]}" | jq -r .id)
(cd "$SAMPLE" && "$ENGINE_BIN" --config .iter/config.json --ticks 4 > "$SCRATCH/engine-deep.log" 2>&1) || true
[ ! -f "$SAMPLE/out_ddep.txt" ] || fail "deep dependent ran while the blocker's child was still open"
[ -f "$SAMPLE/out_dshallow.txt" ] || { cat "$SCRATCH/engine-deep.log"; fail "shallow dependent did not run"; }
[ ! -f "$SAMPLE/out_dnever.txt" ] || fail "item depending on a FAILED blocker ran"
[ "$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$DG" | jq -r .state)" = queued ] || fail "failed-blocker dependent should stay queued (blocked)"
pass "dependencies are deep: waited on the blocker's child; shallow opt-out ran; failed blocker holds its dependent in place"
# reopen the failed blocker (it now succeeds) -> its dependent flows naturally
FRESH=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$DF")
curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems/$DF/reopen" -d '{"reason":"retry"}' >/dev/null || fail "reopen failed blocker"
FRESH=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$DF")
echo "$FRESH" | jq '.exec_shell="true"' | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT/workitems/$DF?expect_version=$(echo "$FRESH" | jq -r .version)" -d @- >/dev/null
(cd "$SAMPLE" && "$ENGINE_BIN" --config .iter/config.json --ticks 8 > "$SCRATCH/engine-deep3.log" 2>&1) || true
[ -f "$SAMPLE/out_dnever.txt" ] || { cat "$SCRATCH/engine-deep3.log"; fail "dependent did not flow after the failed blocker was retried"; }
pass "retrying the failed blocker released its dependent without any manual requeue"
# release the child -> dependent runs on the next pass
ITEM=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$DC")
echo "$ITEM" | jq '.state="queued"' | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT/workitems/$DC?expect_version=$(echo "$ITEM" | jq -r .version)" -d @- >/dev/null
(cd "$SAMPLE" && "$ENGINE_BIN" --config .iter/config.json --ticks 8 > "$SCRATCH/engine-deep2.log" 2>&1) || true
[ -f "$SAMPLE/out_ddep.txt" ] || { cat "$SCRATCH/engine-deep2.log"; fail "deep dependent did not run after the child completed"; }
pass "deep dependent released once the blocker's descendants completed"
# Draining settles to Stopped when nothing is running (engine-driven)
curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT" | jq '.state="Draining"' | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT" -d @- >/dev/null
(cd "$SAMPLE" && "$ENGINE_BIN" --config .iter/config.json --ticks 2 > "$SCRATCH/engine-settle.log" 2>&1) || true
[ "$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT" | jq -r .state)" = Stopped ] || fail "Draining did not settle to Stopped"
grep -q "drained -> Stopped" "$SCRATCH/engine-settle.log" || fail "engine did not log the settle"
curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT" | jq '.state="Running"' | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT" -d @- >/dev/null
pass "Draining is transitional: settled to Stopped once drained"

# ---------- stop mid-run + retry backoff (2026-09-04) ----------
SL=$(curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems" \
  -d '{"name":"long runner to stop","agent":"exec","exec_shell":"sleep 30; echo never > out_stopped.txt","priority":1,"lockdirs":["{topdir}/stop/"]}' | jq -r .id)
(cd "$SAMPLE" && "$ENGINE_BIN" --config .iter/config.json --ticks 6 > "$SCRATCH/engine-stop.log" 2>&1) &
ENGPID=$!
for i in $(seq 1 20); do [ "$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$SL" | jq -r .state)" = in-progress ] && break; sleep 0.5; done
FRESH=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$SL")
[ "$(echo "$FRESH" | jq -r .state)" = in-progress ] || fail "stop scenario: item never started"
echo "$FRESH" | jq '.stop_requested=true' | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT/workitems/$SL?expect_version=$(echo "$FRESH" | jq -r .version)" -d @- >/dev/null || fail "set stop_requested"
wait $ENGPID || true
[ ! -f "$SAMPLE/out_stopped.txt" ] || fail "stopped item ran to completion"
SJ=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$SL")
[ "$(echo "$SJ" | jq -r .state)" = parked ] || { cat "$SCRATCH/engine-stop.log"; fail "stopped item state=$(echo "$SJ" | jq -r .state) (expected parked)"; }
echo "$SJ" | jq -r .lasterror | grep -q "STOPPED by user" || fail "stop note missing"
[ "$(echo "$SJ" | jq -r .stop_requested)" = false ] || fail "stop flag not cleared"
pass "stop mid-run: session killed within a tick, item parked with note, flag cleared"
curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT" | jq '.failure={"maxattempts":3,"first_retry_second":600,"retry_backoff_exponent":2}' \
  | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT" -d @- >/dev/null
RB=$(curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems" \
  -d '{"name":"always fails","agent":"exec","exec_shell":"exit 3","priority":1,"lockdirs":["{topdir}/rb/"]}' | jq -r .id)
(cd "$SAMPLE" && "$ENGINE_BIN" --config .iter/config.json --ticks 4 > "$SCRATCH/engine-backoff.log" 2>&1) || true
RJ=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$RB")
[ "$(echo "$RJ" | jq -r .attempt)" = 1 ] || fail "backoff: expected exactly one attempt in 4 ticks, got $(echo "$RJ" | jq -r .attempt)"
[ "$(echo "$RJ" | jq -r .state)" = queued ] || fail "backoff: state $(echo "$RJ" | jq -r .state)"
[ -n "$(echo "$RJ" | jq -r .retry_after)" ] && [ "$(echo "$RJ" | jq -r .retry_after)" \> "$(date -u +%Y-%m-%dT%H:%M:%SZ)" ] || fail "backoff: retry_after not in the future ($(echo "$RJ" | jq -r .retry_after))"
grep -q "retry after 600s" "$SCRATCH/engine-backoff.log" || fail "backoff log line missing"
pass "retry backoff: a failed attempt waits first_retry_second before the next pick"
curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT" | jq '.failure={"maxattempts":2,"first_retry_second":1,"retry_backoff_exponent":2}' \
  | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT" -d @- >/dev/null
FRESH=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$RB")
echo "$FRESH" | jq '.state="parked"' | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT/workitems/$RB?expect_version=$(echo "$FRESH" | jq -r .version)" -d @- >/dev/null

# ---------- Run Now override (2026-09-04) ----------
# cap 1: a long item occupies the only slot; a run_now item must start anyway
curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT" | jq '.maxagents={"else":1}' \
  | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT" -d @- >/dev/null || fail "cap=1 project update"
RL=$(curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems" \
  -d '{"name":"long occupant","agent":"exec","exec_shell":"sleep 6; echo long > out_long.txt","priority":1,"lockdirs":["{topdir}/a/"]}' | jq -r .id)
RN=$(curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems" \
  -d '{"name":"run now please","agent":"exec","exec_shell":"echo now > out_now.txt","priority":9,"lockdirs":["{topdir}/b/"],"run_now":true}' | jq -r .id)
RW=$(curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems" \
  -d '{"name":"ordinary waiter","agent":"exec","exec_shell":"echo wait > out_wait.txt","priority":2,"lockdirs":["{topdir}/c/"]}' | jq -r .id)
say "running engine with cap 1 + run_now"
(cd "$SAMPLE" && "$ENGINE_BIN" --config .iter/config.json --ticks 3 > "$SCRATCH/engine-runnow.log" 2>&1) || true
grep -q "run-now override: starting" "$SCRATCH/engine-runnow.log" || { cat "$SCRATCH/engine-runnow.log"; fail "no run-now override in engine log"; }
[ -f "$SAMPLE/out_now.txt" ] || { cat "$SCRATCH/engine-runnow.log"; fail "run_now item did not run past the cap"; }
[ ! -f "$SAMPLE/out_wait.txt" ] || fail "ordinary item started while the cap was saturated"
[ "$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$RN" | jq -r .run_now)" = false ] || fail "run_now flag not consumed on claim"
pass "run now: started past a full cap; ordinary work waited; flag consumed"
(cd "$SAMPLE" && "$ENGINE_BIN" --config .iter/config.json --ticks 8 > "$SCRATCH/engine-runnow2.log" 2>&1) || true
for W in "$RL" "$RN" "$RW"; do
  [ "$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$W" | jq -r .state)" = complete ] || fail "run-now scenario item $W not complete"
done
pass "run now: everything drained to complete afterwards"
curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT" | jq '.maxagents={">98%":0,"else":2}' \
  | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT" -d @- >/dev/null

# ---------- closed items: immutable, docs append, reopen ----------
ITEM=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$WA")
V=$(echo "$ITEM" | jq -r .version)
CODE=$(echo "$ITEM" | jq '.priority=1' | curl -s -o /dev/null -w '%{http_code}' "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT/workitems/$WA?expect_version=$V" -d @-)
[ "$CODE" = 403 ] || fail "closed workitem PUT accepted (HTTP $CODE)"
CODE=$(curl -s -o /dev/null -w '%{http_code}' "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT/workitems/$WA/details/0" \
  -d '{"key":"request","valuetype":"text","value":"rewritten history"}')
[ "$CODE" = 403 ] || fail "closed workitem detail PUT accepted (HTTP $CODE)"
CODE=$(curl -s -o /dev/null -w '%{http_code}' "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems/$WA/details" \
  -d '{"key":"response","valuetype":"text","value":"fake response"}')
[ "$CODE" = 403 ] || fail "closed workitem accepted a non-doc append (HTTP $CODE)"
# tags are the one summary-row exception on closed items
echo "$ITEM" | jq '.tags=[{"text":"regressed","color":"#f54927"}]' | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT/workitems/$WA?expect_version=$V" -d @- >/dev/null || fail "tag-only edit on closed item rejected"
[ "$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$WA" | jq -r '.tags[0].text')" = regressed ] || fail "tag edit not persisted"
[ "$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$WA" | jq -r .state)" = complete ] || fail "tag edit changed state"
pass "closed item: summary PUT, detail PUT and non-doc append rejected (403); tag-only edit allowed"

NDET=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$WA/details" | jq 'length')
DOC=$(curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems/$WA/details" \
  -d '{"key":"doc","valuetype":"text","value":"closeout: verified by hand"}') || fail "doc append on closed item"
[ "$(echo "$DOC" | jq -r .by)" = admin ] || fail "doc row not stamped with principal ($(echo "$DOC" | jq -c .))"
[ -n "$(echo "$DOC" | jq -r .ts)" ] && [ "$(echo "$DOC" | jq -r .ts)" != null ] || fail "doc row not stamped with ts"
# the engine helper appends as the engine principal, by id prefix
(cd "$SAMPLE" && ITER_ENGINE_TOKEN=$ENGINE_TOKEN "$ENGINE_BIN" --config .iter/config.json --doc "${WA:0:12}" --text "closeout from an agent" > "$SCRATCH/doc.log" 2>&1) || { cat "$SCRATCH/doc.log"; fail "--doc helper"; }
grep -q "appended" "$SCRATCH/doc.log" || { cat "$SCRATCH/doc.log"; fail "--doc did not report success"; }
DETS=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$WA/details")
[ "$(echo "$DETS" | jq 'length')" = $((NDET + 2)) ] || fail "expected $((NDET+2)) detail rows, got $(echo "$DETS" | jq 'length')"
echo "$DETS" | jq -e '[.[]|select(.key=="doc")]|length==2' >/dev/null || fail "two doc rows expected"
echo "$DETS" | jq -e '[.[]|select(.key=="doc" and .by=="engine01")]|length==1' >/dev/null || fail "engine doc row lacks engine principal"
# orders are unique and dense (atomic allocation)
[ "$(echo "$DETS" | jq '[.[].order]|unique|length')" = "$(echo "$DETS" | jq 'length')" ] || fail "duplicate detail orders"
pass "closed item: doc rows append via API and --doc helper, provenance-stamped, orders unique"

# reopen: engine role 403, admin ok -> queued + doc row; then it runs again
CODE=$(curl -s -o /dev/null -w '%{http_code}' -H "authorization: Bearer $ENGINE_TOKEN" -H content-type:application/json \
  -X POST "$BASE/api/projects/$PROJECT/workitems/$WA/reopen" -d '{"reason":"rogue"}')
[ "$CODE" = 403 ] || fail "engine role reopened a closed item (HTTP $CODE)"
curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems/$WA/reopen" -d '{"reason":"rerun after fix"}' >/dev/null || fail "admin reopen"
ST=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$WA" | jq -r .state)
[ "$ST" = queued ] || fail "reopened item state=$ST"
curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$WA/details" | jq -e '[.[]|select(.key=="doc" and (.value|test("reopened by admin.*rerun after fix")))]|length==1' >/dev/null || fail "reopen doc row missing"
rm -f "$SAMPLE/out_a.txt"
(cd "$SAMPLE" && "$ENGINE_BIN" --config .iter/config.json --ticks 6 > "$SCRATCH/engine-reopen.log" 2>&1) || true
[ -f "$SAMPLE/out_a.txt" ] || { cat "$SCRATCH/engine-reopen.log"; fail "reopened item did not run again"; }
[ "$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$WA" | jq -r .state)" = complete ] || fail "reopened item did not complete"
pass "reopen: users-only, back to queued with a doc record, ran and closed again"


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

# ---- user timezone (2026-09-04): stored on the user record, echoed by login; data stays UTC ----
curl -sf "${AUTH[@]}" "$BASE/api/users/admin" | jq '.timezone="America/Los_Angeles"' | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/users/admin" -d @- >/dev/null || fail "self timezone put"
[ "$(curl -sf "${AUTH[@]}" "$BASE/api/users/admin" | jq -r .timezone)" = "America/Los_Angeles" ] || fail "timezone not stored"
LOGIN2=$(curl -s -X POST "$BASE/auth/login" -H content-type:application/json -d "{\"user\":\"admin\",\"password\":\"$PASS_ADMIN\"}")
[ "$(echo "$LOGIN2" | jq -r .timezone)" = "America/Los_Angeles" ] || fail "login does not echo the timezone: $LOGIN2"
pass "user timezone: stored on the record (self-service PUT), echoed by login"

# ---- viewer role (2026-09-04): reads everything, writes nothing but their own profile ----
curl -sf "${AUTH[@]}" -X PUT "$BASE/api/users/gerald" -d '{"role":"viewer","password":"view-only-pw","email":""}' >/dev/null || fail "create viewer"
VTOK=$(curl -sf -X POST "$BASE/auth/login" -H content-type:application/json -d '{"user":"gerald","password":"view-only-pw"}' | jq -r .token)
[ -n "$VTOK" ] && [ "$VTOK" != null ] || fail "viewer login"
VAUTH=(-H "authorization: Bearer $VTOK" -H content-type:application/json)
[ "$(curl -sf "${VAUTH[@]}" "$BASE/api/projects/$PROJECT/workitems" | jq 'length')" -ge 1 ] || fail "viewer cannot read workitems"
CODE=$(curl -s -o /dev/null -w '%{http_code}' "${VAUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems" -d '{"name":"viewer write","agent":"exec","exec_shell":"true"}')
[ "$CODE" = 403 ] || fail "viewer could create a workitem (HTTP $CODE)"
ANYID=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems" | jq -r '.[0].id')
CODE=$(curl -s -o /dev/null -w '%{http_code}' "${VAUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems/$ANYID/explain" -d '{}')
[ "$CODE" = 403 ] || fail "viewer could request an ELI5 (HTTP $CODE)"
curl -sf "${VAUTH[@]}" "$BASE/api/users/gerald" | jq '.timezone="Asia/Riyadh"' | curl -sf "${VAUTH[@]}" -X PUT "$BASE/api/users/gerald" -d @- >/dev/null || fail "viewer self profile put"
[ "$(curl -sf "${AUTH[@]}" "$BASE/api/users/gerald" | jq -r '.role+" "+.timezone')" = "viewer Asia/Riyadh" ] || fail "viewer profile edit changed the role or lost the timezone"
pass "viewer role: reads, 403 on queue writes and ELI5, may edit own profile"

# widget helper
echo '{"title":"t","fields":[{"key":"a","type":"text","value":""}]}' > "$SCRATCH/w.json"
"$ENGINE_BIN" --question-widget "$SCRATCH/w.json" | grep -q "OK" || fail "--question-widget valid case"
echo '{"title":"","fields":[]}' > "$SCRATCH/w2.json"
("$ENGINE_BIN" --question-widget "$SCRATCH/w2.json" || true) | grep -q "INVALID" || fail "--question-widget invalid case"
pass "--question-widget helper"

# ---------- scheduled workitems (itersched port) ----------
# template with last_fired in the past -> due on the first check; fires ONCE
TPL=$(curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems" \
  -d '{"name":"sched: heartbeat file","agent":"exec","exec_shell":"echo sched-ran >> out_sched.txt","state":"scheduled","priority":8,"sched":{"kind":"every","every_min":5,"last_fired":"2026-09-01T00:00:00Z"}}' | jq -r .id)
[ -n "$TPL" ] && [ "$TPL" != null ] || fail "schedule template create"

# users-only: the engine role must NOT be able to create schedules
CODE=$(curl -s -o /dev/null -w '%{http_code}' -H "authorization: Bearer $ENGINE_TOKEN" -H content-type:application/json \
  -X POST "$BASE/api/projects/$PROJECT/workitems" \
  -d '{"name":"rogue schedule","agent":"exec","state":"scheduled","sched":{"kind":"every","every_min":1}}')
[ "$CODE" = 403 ] || fail "engine role created a schedule (HTTP $CODE)"
pass "schedules are users-only (engine role got 403)"

say "running engine for schedule fire"
(cd "$SAMPLE" && "$ENGINE_BIN" --config .iter/config.json --ticks 8 > "$SCRATCH/engine-sched.log" 2>&1) || true
grep -q "schedule 'sched: heartbeat file' fired" "$SCRATCH/engine-sched.log" || { cat "$SCRATCH/engine-sched.log"; fail "schedule did not fire"; }
[ -f "$SAMPLE/out_sched.txt" ] || fail "scheduled clone did not run"
NCLONES=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems" | jq "[.[]|select(.source_schedule==\"$TPL\")]|length")
[ "$NCLONES" = 1 ] || fail "expected exactly 1 clone, got $NCLONES (dedup/refire broken)"
TPLSTATE=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$TPL" | jq -r .state)
[ "$TPLSTATE" = scheduled ] || fail "template state changed to $TPLSTATE"
LF=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$TPL" | jq -r .sched.last_fired)
[ "$LF" != "2026-09-01T00:00:00Z" ] || fail "last_fired not updated"
pass "schedule fired once, clone completed, template intact, last_fired claimed"

# ---------- usage%-driven account gating ----------
export ITER_USAGE_DIR="$SCRATCH/usage"
mkdir -p "$ITER_USAGE_DIR"
FUTURE=$(( $(date +%s) + 86400 )); export FUTURE
# fake api.anthropic.com/v1/messages for the idle probe: logs the auth header,
# answers with anthropic-ratelimit-unified-* headers (5h 42%, 7d 10%); a
# "$PROBE_LOG.reject" flag file turns it into a 429 at 5h 100% (hard limit)
PROBE_LOG="$SCRATCH/probe.log"; : > "$PROBE_LOG"
cat > "$SCRATCH/probe_server.py" <<'PYS'
import http.server, os, sys
PORT, LOG, FUT = int(sys.argv[1]), sys.argv[2], sys.argv[3]
class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        self.rfile.read(int(self.headers.get('Content-Length') or 0))
        with open(LOG, 'a') as f:
            f.write("auth=%s beta=%s\n" % (self.headers.get('Authorization'), self.headers.get('anthropic-beta')))
        rej = os.path.exists(LOG + '.reject')
        body = b'{"type":"error","error":{"type":"rate_limit_error"}}' if rej else b'{"type":"message","content":[{"type":"text","text":"#"}]}'
        self.send_response(429 if rej else 200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('anthropic-ratelimit-unified-status', 'rejected' if rej else 'allowed_warning')
        self.send_header('anthropic-ratelimit-unified-5h-status', 'rejected' if rej else 'allowed')
        self.send_header('anthropic-ratelimit-unified-5h-utilization', '1.0' if rej else '0.42')
        self.send_header('anthropic-ratelimit-unified-5h-reset', FUT)
        self.send_header('anthropic-ratelimit-unified-7d-status', 'allowed_warning')
        self.send_header('anthropic-ratelimit-unified-7d-utilization', '0.10')
        self.send_header('anthropic-ratelimit-unified-7d-reset', FUT)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers(); self.wfile.write(body)
    def log_message(self, *a): pass
http.server.HTTPServer(('127.0.0.1', PORT), H).serve_forever()
PYS
PROBE_PORT="${ITER_E2E_PROBE_PORT:-8399}"
python3 "$SCRATCH/probe_server.py" "$PROBE_PORT" "$PROBE_LOG" "$FUTURE" &
PROBE_PID=$!
export ITER_USAGE_PROBE_URL="http://127.0.0.1:$PROBE_PORT/v1/messages"
ITEM=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT")
echo "$ITEM" | jq '.accounts=[{"name":"TestAcct","token_envar":"FAKE_TOKEN","order":1,"switch":80,"stop":99}]' \
  | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT" -d @- >/dev/null || fail "project accounts update"
cat > "$ITER_USAGE_DIR/iter3-usage-TestAcct.json" <<EOF
{"ts":"$(date -u +%Y-%m-%dT%H:%M:%SZ)","rate_limits":{"five_hour":{"used_percentage":99.9,"resets_at":$FUTURE},"seven_day":{"used_percentage":50.0,"resets_at":$FUTURE}}}
EOF
WU=$(curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems" \
  -d '{"name":"usage-gated work","agent":"exec","exec_shell":"echo gated > out_gated.txt","priority":3}' | jq -r .id)
(cd "$SAMPLE" && ITER_USAGE_DIR="$ITER_USAGE_DIR" "$ENGINE_BIN" --config .iter/config.json --ticks 4 > "$SCRATCH/engine-usage.log" 2>&1) || true
[ ! -f "$SAMPLE/out_gated.txt" ] || fail "engine ran work while all accounts were at stop%"
grep -q "all accounts at stop%" "$SCRATCH/engine-usage.log" || fail "no stop-hold log line"
ST=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$WU" | jq -r .state)
[ "$ST" = queued ] || fail "gated item state=$ST (expected queued)"
pass "all-accounts-at-stop%: engine held all activity"
cat > "$ITER_USAGE_DIR/iter3-usage-TestAcct.json" <<EOF
{"ts":"$(date -u +%Y-%m-%dT%H:%M:%SZ)","rate_limits":{"five_hour":{"used_percentage":10.0,"resets_at":$FUTURE},"seven_day":{"used_percentage":5.0,"resets_at":$FUTURE}}}
EOF
(cd "$SAMPLE" && ITER_USAGE_DIR="$ITER_USAGE_DIR" "$ENGINE_BIN" --config .iter/config.json --ticks 6 > "$SCRATCH/engine-usage2.log" 2>&1) || true
[ -f "$SAMPLE/out_gated.txt" ] || { cat "$SCRATCH/engine-usage2.log"; fail "engine did not resume after usage dropped"; }
ACCT=$(curl -sf "${AUTH[@]}" "$BASE/api/engines/Engine01" | jq -r .account)
[ "$ACCT" = TestAcct ] || fail "engine did not report chosen account (got '$ACCT')"
pass "usage refresh resumed work; chosen account reported centrally"

# ---------- draining monitoring ----------
ITEM=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT")
V=$(echo "$ITEM" | jq -r .version 2>/dev/null || true)
echo "$ITEM" | jq '.state="Draining"' | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT" -d @- >/dev/null || fail "set Draining"
WD2=$(curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems" \
  -d '{"name":"work during drain","agent":"exec","exec_shell":"echo drained > out_drain.txt","priority":1}' | jq -r .id)
(cd "$SAMPLE" && ITER_USAGE_DIR="$ITER_USAGE_DIR" "$ENGINE_BIN" --config .iter/config.json --ticks 4 > "$SCRATCH/engine-drain.log" 2>&1) || true
[ ! -f "$SAMPLE/out_drain.txt" ] || fail "engine started new work while Draining"
ST=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$WD2" | jq -r .state)
[ "$ST" = queued ] || fail "drain item state=$ST"
STATUS=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/status")
# nothing was running, so the transitional Draining settled to Stopped on the engine's first tick
[ "$(echo "$STATUS" | jq -r .project_state)" = Stopped ] || fail "status project_state (expected settled Stopped, got $(echo "$STATUS" | jq -r .project_state))"
[ "$(echo "$STATUS" | jq -r .all_drained)" = true ] || fail "status all_drained"
echo "$STATUS" | jq -e '.engines|length >= 1' >/dev/null || fail "status has no engines"
pass "Draining: no new picks; settled to Stopped; status endpoint reports drain state + engine liveness"
curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT" | jq '.state="Running"' \
  | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT" -d @- >/dev/null

# ---------- close gate (spec: Close Gate, 2026-09-03) ----------
# A fake `claude` on PATH answers worker and verifier sessions from the prompt
# text, so the gate's state machine runs end to end without spending tokens.
FAKEBIN="$SCRATCH/fakebin"; mkdir -p "$FAKEBIN"
export GATE_LOG="$SCRATCH/gate_log.txt"; : > "$GATE_LOG"
export GATE_PROMPTS="$SCRATCH/prompts"; mkdir -p "$GATE_PROMPTS"
cat > "$FAKEBIN/claude" <<'FAKE'
#!/usr/bin/env bash
# fake claude for e2e: -p <prompt> ... ; prints a Claude Code json result object
allargs="$*"; prompt=""; while [ $# -gt 0 ]; do case "$1" in -p) prompt="$2"; shift;; esac; shift; done
name=$(grep -o -m1 -E '(Title|Workitem): gate-[a-z0-9]*' <<<"$prompt" | sed -E 's/.*: //')
if grep -q "# Explain this work item simply (ELI5)" <<<"$prompt"; then
  printf '%s\n' "$prompt" > "$GATE_PROMPTS/eli5-prompt.txt"
  echo "explain tools=$allargs" >> "$GATE_LOG"
  printf '{"type":"result","subtype":"success","is_error":false,"num_turns":4,"session_id":"fake-sid","total_cost_usd":0.05,"usage":{"input_tokens":500,"output_tokens":80},"result":"EXPLAINED-MARKER: when a customer asks for chips, we first check they paid."}\n'; exit 0
fi
if ! grep -q "# Step: mainwork" <<<"$prompt" && ! grep -q "iter close-gate verifier" <<<"$prompt"; then
  # prework / selfcheck turns of the same session: record, acknowledge, move on
  [ -n "$name" ] && [ -n "${ITER_WORKID:-}" ] && echo "$name" > "$GATE_PROMPTS/.name-$ITER_WORKID"
  [ -z "$name" ] && [ -n "${ITER_WORKID:-}" ] && [ -f "$GATE_PROMPTS/.name-$ITER_WORKID" ] && name=$(cat "$GATE_PROMPTS/.name-$ITER_WORKID")
  printf '%s\n' "$prompt" > "$GATE_PROMPTS/$(date +%s%N)-$name.txt" 2>/dev/null || true
  printf '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"session_id":"fake-sid","result":"all done"}\n'; exit 0
fi
emit() {
  printf '{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","isUsingOverage":false,"unifiedWindows":{"five_hour":{"utilization":0.33,"resetsAt":%s},"seven_day":{"utilization":0.07,"resetsAt":%s}}}}\n' "${FUTURE:-0}" "${FUTURE:-0}"
  printf '{"type":"result","subtype":"%s","is_error":false,"num_turns":3,"session_id":"fake-sid","total_cost_usd":0.25,"usage":{"input_tokens":1000,"output_tokens":200},"result":%s}\n' "$1" "$(jq -Rn --arg t "$2" '$t')"; }
if grep -q "iter close-gate verifier" <<<"$prompt"; then
  echo "verifier $name" >> "$GATE_LOG"
  if grep -q "DONE-MARKER" <<<"$prompt"; then emit success '{"verdict":"complete","open":[],"reason":"all obligations met"}'
  else emit success '{"verdict":"incomplete","open":["file the ten build items"],"reason":"the message says it is waiting on a review"}'; fi
  exit 0
fi
[ -n "$name" ] && [ -n "${ITER_WORKID:-}" ] && echo "$name" > "$GATE_PROMPTS/.name-$ITER_WORKID"
[ -z "$name" ] && [ -n "${ITER_WORKID:-}" ] && [ -f "$GATE_PROMPTS/.name-$ITER_WORKID" ] && name=$(cat "$GATE_PROMPTS/.name-$ITER_WORKID")
echo "worker $name" >> "$GATE_LOG"
echo "token=${CLAUDE_CODE_OAUTH_TOKEN:-none} name=$name" >> "$GATE_PROMPTS/tokens.txt"
printf '%s\n' "$prompt" > "$GATE_PROMPTS/$(date +%s%N)-$name.txt" 2>/dev/null || true
case "$name" in
  gate-cli)
    # an agent using the iter CLI from inside its run
    "$ITER_BIN" capability > "$GATE_PROMPTS/capability-index.txt" 2>&1
    "$ITER_BIN" capability _ask_the_human > "$GATE_PROMPTS/capability-doc.txt" 2>&1
    "$ITER_BIN" add --type code --title "cli child" --mainwork "child work" --codepath "$ITER_TOPDIR/src" --depends-on "${GATE_DEP: -12}" --context "{topdir}/README.md" > "$GATE_PROMPTS/add.txt" 2>&1
    "$ITER_BIN" add --type code --title "self dep" --mainwork "x" --depends-on "$ITER_WORKID" > "$GATE_PROMPTS/add-self.txt" 2>&1 || true
    "$ITER_BIN" doc "note from the agent" > "$GATE_PROMPTS/doc.txt" 2>&1
    "$ITER_BIN" status > "$GATE_PROMPTS/status.txt" 2>&1
    "$ITER_BIN" ask --question "Which color?" > "$GATE_PROMPTS/ask.txt" 2>&1
    emit success "asked; ending turn" ;;
  gate-reject)
    "$ITER_BIN" reject --reason "premise no longer holds" > "$GATE_PROMPTS/reject.txt" 2>&1
    emit success "rejected" ;;
  gate-turncap)
    if grep -q "Close-gate feedback" <<<"$prompt"; then emit success "DONE-MARKER: finished after the cut-off"
    else emit error_max_turns "partial work, ran out of turns"; fi ;;
  gate-recovers)
    if grep -q "Close-gate feedback" <<<"$prompt"; then emit success "DONE-MARKER: filed the ten items"
    else emit success "Plan written. I'm waiting for the review to finish."; fi ;;
  *) emit success "Plan written. I'm waiting for the review to finish." ;;
esac
FAKE
chmod +x "$FAKEBIN/claude"

# agent tooling (central copies of V2's .iter text) + a project head with global context files
curl -sf "${AUTH[@]}" -X PUT "$BASE/api/tooling/_shared" -d '{"kind":"shared","desc":"all agents","body":"SHARED-RULES-MARKER {critreview_max_rounds}"}' >/dev/null || fail "tooling put"
curl -sf "${AUTH[@]}" -X PUT "$BASE/api/tooling/_ask_the_human" -d '{"kind":"capability","desc":"ask the human a question","body":"ASK-DOC-BODY"}' >/dev/null || fail "tooling put"
curl -sf "${AUTH[@]}" -X PUT "$BASE/api/tooling/user" -d '{"kind":"source","desc":"","body":"SOURCE-USER-MARKER"}' >/dev/null || fail "tooling put"
curl -sf "${AUTH[@]}" -X PUT "$BASE/api/tooling/premise-check" -d '{"kind":"prepost","desc":"","body":"PREMISE-STEP-MARKER"}' >/dev/null || fail "tooling put"
CODE=$(curl -s -o /dev/null -w '%{http_code}' -H "authorization: Bearer $ENGINE_TOKEN" -H content-type:application/json -X PUT "$BASE/api/tooling/rogue" -d '{"kind":"shared","body":"x"}')
[ "$CODE" = 403 ] || fail "engine role could write tooling (HTTP $CODE)"
[ "$(curl -sf "${AUTH[@]}" "$BASE/api/tooling" | jq 'length')" = 4 ] || fail "tooling list"
pass "agent tooling rows: admin writes, engine role read-only"
mkdir -p "$SAMPLE/reqs" "$SAMPLE/src"
printf -- '---\nprojectname: "SampleV3"\nglobalcontextfiles: ["{topdir}/reqs/*.md"]\n---\nhead\n' > "$SAMPLE/main.iter.md"
echo "req" > "$SAMPLE/reqs/techreq.md"; echo "marker" > "$SAMPLE/src/src.code.iter.md"; echo "top" > "$SAMPLE/top.code.iter.md"

curl -sf "${AUTH[@]}" -X PUT "$BASE/api/agents/gatetest" \
  -d '{"desc":"close-gate test agent","max":3,"timeoutsec":60,"model":"","promptbody":"You are the gate test agent.","closegate":{"verify":"haiku","max_bounces":1}}' >/dev/null || fail "gatetest agent put"
curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT" | jq '.agents.gatetest={"max":3}' \
  | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT" -d @- >/dev/null || fail "project gatetest override"
mk_gate_item() {
  curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems" \
    -d "{\"name\":\"$1\",\"agent\":\"gatetest\",\"priority\":2}" | jq -r .id
}
GR=$(mk_gate_item gate-recovers); GS=$(mk_gate_item gate-stuck); GT=$(mk_gate_item gate-turncap)
GC=$(curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems" \
  -d '{"name":"gate-cli","agent":"gatetest","priority":1,"lockdirs":["{topdir}/src/"],"prework":["premise-check"],"requestedby":"user"}' | jq -r .id)
GJ=$(curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems" \
  -d '{"name":"gate-reject","agent":"gatetest","priority":1,"lockdirs":["{topdir}/reqs/"]}' | jq -r .id)
for W in "$GR" "$GS" "$GT" "$GC" "$GJ"; do
  curl -sf "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT/workitems/$W/details/0" \
    -d '{"key":"request","valuetype":"text","value":"write the plan AND file its build items as workitems"}' >/dev/null || fail "gate request detail"
done
export GATE_DEP="$GR"
say "running engine with fake claude for the close gate"
(cd "$SAMPLE" && PATH="$FAKEBIN:$PATH" "$ENGINE_BIN" --config .iter/config.json --ticks 14 > "$SCRATCH/engine-gate.log" 2>&1) || true

st_of() { curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$1" | jq -r .state; }
gb_of() { curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$1" | jq -r .gate_bounces; }
keys_of() { curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$1/details" | jq -r '[.[].key]|join(",")'; }

[ "$(st_of "$GR")" = complete ] || { cat "$SCRATCH/engine-gate.log"; fail "gate-recovers state=$(st_of "$GR") (expected complete after one bounce)"; }
[ "$(gb_of "$GR")" = 1 ] || fail "gate-recovers gate_bounces=$(gb_of "$GR") (expected 1)"
keys_of "$GR" | grep -q "verify" || fail "gate-recovers has no verify detail row"
[ "$(grep -c "worker gate-recovers" "$GATE_LOG")" = 2 ] || fail "gate-recovers worker runs=$(grep -c "worker gate-recovers" "$GATE_LOG") (expected 2)"
pass "close gate: verifier bounced an unfinished item once; continuation run completed it"

[ "$(st_of "$GS")" = question ] || { cat "$SCRATCH/engine-gate.log"; fail "gate-stuck state=$(st_of "$GS") (expected question)"; }
[ "$(gb_of "$GS")" = 2 ] || fail "gate-stuck gate_bounces=$(gb_of "$GS") (expected 2)"
GSQ=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$GS/details" | jq -c '[.[]|select(.key=="question" and .value.gate=="close")]|last')
[ -n "$GSQ" ] && [ "$GSQ" != null ] || fail "gate-stuck has no close-gate question widget"
echo "$GSQ" | jq -e '.value.fields[0].key=="action"' >/dev/null || fail "gate widget shape"
pass "close gate: bounce budget exhausted -> question state with a gate widget"

[ "$(st_of "$GT")" = complete ] || { cat "$SCRATCH/engine-gate.log"; fail "gate-turncap state=$(st_of "$GT") (expected complete)"; }
[ "$(grep -c "verifier gate-turncap" "$GATE_LOG")" = 1 ] || fail "turn-cap run should skip the verifier (verifier calls=$(grep -c "verifier gate-turncap" "$GATE_LOG"), expected 1)"
pass "close gate: error_max_turns held deterministically (no verifier spend), continuation completed"

# ---- V2-parity prompt + agent CLI (2026-09-04) ----
P1=$(ls "$GATE_PROMPTS"/*-gate-cli.txt | head -1)
for m in "SHARED-RULES-MARKER 3" "# Capabilities" "_ask_the_human: ask the human a question" "# Project context" "reqs/techreq.md" "main.iter.md" "SOURCE-USER-MARKER" "# Work item" "Work item id: $GC" "# Context files" "src/src.code.iter.md" "top.code.iter.md" "# Step: prework:premise-check" "PREMISE-STEP-MARKER"; do
  grep -qF -- "$m" "$P1" || { echo "--- first prompt:"; head -60 "$P1"; fail "spin-up prompt lacks: $m"; }
done
N=$(ls "$GATE_PROMPTS"/*-gate-cli.txt | wc -l | tr -d ' ')
[ "$N" -ge 2 ] || fail "expected a multi-turn session for gate-cli (prework, mainwork, …), saw $N turn(s)"
grep -q "# Step: mainwork" "$(ls "$GATE_PROMPTS"/*-gate-cli.txt | sed -n 2p)" || fail "second turn is not mainwork"
pass "spin-up prompt: agent body + shared rules + capability index + project head + source + work item + context (marker chain) + prose prework as its own turn"
grep -q "_ask_the_human: ask the human a question" "$GATE_PROMPTS/capability-index.txt" || fail "iter capability index"
grep -q "ASK-DOC-BODY" "$GATE_PROMPTS/capability-doc.txt" || fail "iter capability <name>"
grep -q "^added " "$GATE_PROMPTS/add.txt" || { cat "$GATE_PROMPTS/add.txt"; fail "iter add"; }
CHILD=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems" | jq -r '.[]|select(.name=="cli child")|.id')
[ -n "$CHILD" ] || fail "cli child not created"
CJ=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$CHILD")
[ "$(echo "$CJ" | jq -r .createdby)" = "$GC" ] || fail "child createdby"
[ "$(echo "$CJ" | jq -r .requestedby)" = "agent:gatetest" ] || fail "child requestedby"
[ "$(echo "$CJ" | jq -r '.blockedby[0]')" = "$GR" ] || fail "child blockedby (12-char suffix resolution)"
grep -q "cannot depend on the item that creates it" "$GATE_PROMPTS/add-self.txt" || fail "self-dependency should be refused"
[ "$(echo "$CJ" | jq -r '.lockdirs[0]')" = "{topdir}/src" ] || fail "child lockdir mapping: $(echo "$CJ" | jq -c .lockdirs)"
[ "$(echo "$CJ" | jq -r '.context[0]')" = "{topdir}/README.md" ] || fail "child context"
[ "$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$CHILD/details" | jq -r '.[0].value')" = "child work" ] || fail "child request row"
grep -q "open work item" "$GATE_PROMPTS/status.txt" || fail "iter status"
grep -q "appended" "$GATE_PROMPTS/doc.txt" || fail "iter doc"
[ "$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$GC" | jq -r .state)" = question ] || fail "iter ask did not park the caller in question (got $(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$GC" | jq -r .state))"
curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$GC/details" | jq -e '[.[]|select(.key=="question" and .value.title=="Which color?")]|length==1' >/dev/null || fail "ask widget missing"
curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$GC/details" | jq -e '[.[]|select(.key=="response")]|length==1' >/dev/null || fail "response row not written for the asked item"
[ "$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$GJ" | jq -r .state)" = parked ] || fail "iter reject did not park the caller"
curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$GJ" | jq -r .lasterror | grep -q "rejected: premise" || fail "reject reason not recorded"
pass "agent CLI: capability index/doc, add (child of caller, deep dep, lockdir + context), doc, status, ask -> question kept at close, reject -> parked"

# ---- local-file verbs served by V3 itself (2026-09-04: no V2 binary, no config.iter.json) ----
mkdir -p "$SAMPLE/src/test" "$SAMPLE/usecases"
printf 'echo "ITER_RESULT pass=2 fail=0 total=2"\nexit 0\n' > "$SAMPLE/src/test/t-ok.sh"
printf 'echo "ITER_RESULT pass=0 fail=1 total=1"\nexit 1\n' > "$SAMPLE/src/test/t-bad.sh"
chmod +x "$SAMPLE/src/test/"*.sh
cat > "$SAMPLE/src/test/testgroup.iter.md" <<'TG'
# sample tests

<!-- iterapp:testgroups
{"label":"green","desc":"passes","testlist":[{"id":"t-ok","name":"ok","desc":"","shell":"t-ok.sh"}]}
{"label":"mixed","desc":"one red","testlist":[{"id":"t-ok","name":"ok","desc":"","shell":"t-ok.sh"},{"id":"t-bad","name":"bad","desc":"","shell":"t-bad.sh"}]}
-->
TG
cat > "$SAMPLE/usecases/greet.usecase.iter.md" <<'UC'
---
name: greet
children:
  codenodes: []
---
# greet
A user asks for a greeting.
UC
LV=(env ITER_TOPDIR="$SAMPLE" ITER_DATA_URL="$BASE" ITER_ENGINE_TOKEN="$ENGINE_TOKEN" ITER_PROJECT="$PROJECT" "$ENGINE_BIN" cli)
# runtests: neutral green -> 0, mixed -> 1; the block's result/counts update
"${LV[@]}" runtests --group green > "$SCRATCH/rt-green.txt" 2>&1 || fail "runtests green exit $? : $(cat "$SCRATCH/rt-green.txt")"
grep -q 'tests 2/2 100% — testgroup "green" PASSED' "$SCRATCH/rt-green.txt" || { cat "$SCRATCH/rt-green.txt"; fail "runtests green output"; }
RC=0; "${LV[@]}" runtests --group mixed > "$SCRATCH/rt-mixed.txt" 2>&1 || RC=$?; [ "$RC" = 1 ] || { cat "$SCRATCH/rt-mixed.txt"; fail "runtests mixed exit $RC (expected 1)"; }
grep -q '"label":"mixed"' "$SAMPLE/src/test/testgroup.iter.md" && grep -q '"result":"failed"' "$SAMPLE/src/test/testgroup.iter.md" || fail "testgroup block not updated with the run"
# claims inside a run: --fixed on a red group records a FALSE claim on the calling item (the close gate holds it)
CL=$(curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems" -d '{"name":"claim holder","agent":"gatetest","priority":9,"lockdirs":["{topdir}/claim/"]}' | jq -r .id)
RC=0; ITER_WORKID="$CL" "${LV[@]}" runtests --group mixed --fixed > "$SCRATCH/rt-fixed.txt" 2>&1 || RC=$?; [ "$RC" = 3 ] || { cat "$SCRATCH/rt-fixed.txt"; fail "false --fixed claim exit $RC (expected 3)"; }
[ "$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$CL/details" | jq -r '[.[]|select(.key=="claim")]|last|.value|"\(.claim) \(.upheld) \(.outcome)"')" = "fixed false failed" ] || fail "claim row not recorded"
ITER_WORKID="$CL" "${LV[@]}" runtests --group green --fixed > /dev/null 2>&1 || fail "true --fixed claim should exit 0"
# --broken on a green group = stale: parked with the reason
RC=0; ITER_WORKID="$CL" "${LV[@]}" runtests --group green --broken > "$SCRATCH/rt-broken.txt" 2>&1 || RC=$?; [ "$RC" = 3 ] || fail "false --broken exit $RC (expected 3)"
[ "$(st_of "$CL")" = parked ] || fail "stale --broken claim did not park the item (state $(st_of "$CL"))"
# validate: template by role; a scan of the sample
"${LV[@]}" validate --file "$SAMPLE/src/new.code.iter.md" --template > "$SCRATCH/val-tpl.txt" 2>&1 || fail "validate --template"
grep -q "^---" "$SCRATCH/val-tpl.txt" || { cat "$SCRATCH/val-tpl.txt"; fail "template has no frontmatter"; }
"${LV[@]}" validate > "$SCRATCH/val.txt" 2>&1 || true
grep -q "^validate: [0-9]* file(s) checked" "$SCRATCH/val.txt" || { cat "$SCRATCH/val.txt"; fail "validate did not run"; }
# markers: the scan as json; teststate --list; usecase --add/--list
"${LV[@]}" markers > "$SCRATCH/markers.json" 2>&1 || fail "markers"
jq -e '.testgroups|length>=1' "$SCRATCH/markers.json" >/dev/null 2>&1 || jq -e '.nodes' "$SCRATCH/markers.json" >/dev/null || { head -20 "$SCRATCH/markers.json"; fail "markers json"; }
"${LV[@]}" teststate --list > "$SCRATCH/teststate.txt" 2>&1 || fail "teststate --list"
grep -q "teststate (flag → effective)" "$SCRATCH/teststate.txt" || fail "teststate output"
"${LV[@]}" usecase --file usecases/greet.usecase.iter.md --add "src/src.code.iter.md" > /dev/null 2>&1 || fail "usecase --add"
[ "$("${LV[@]}" usecase --file usecases/greet.usecase.iter.md --list 2>/dev/null)" = "src/src.code.iter.md" ] || fail "usecase --list"
# add --question @file stores the FILE's text (not the path) — the widget's title is its first line
printf 'Situation. The cache has no memory alarm.\n\nThe decision needed: may it open a metrics port?\n' > "$SCRATCH/q.md"
QF=$("${LV[@]}" add --type code --title "question by file" --mainwork "build it" --question "@$SCRATCH/q.md" 2>&1 | sed -n 's/^added \([^ ]*\).*/\1/p' | head -1)
[ -n "$QF" ] || fail "add --question @file did not add"
QW=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems" | jq -r --arg n "question by file" '.[]|select(.name==$n)|.id' | head -1)
[ "$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$QW/details" | jq -r '[.[]|select(.key=="question")][0].value.title')" = "Situation. The cache has no memory alarm." ] || fail "question widget stored the path, not the file text"
[ "$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$QW/details" | jq -r '[.[]|select(.key=="question")][0].value.detail' | grep -c "metrics port")" = 1 ] || fail "question detail lacks the file body"
# the V2-only verbs say so; nothing V2 is on the agent's environment
{ "${LV[@]}" testsweep 2>&1 || true; } | grep -q "retired with the V2 binary" || fail "retired verb message"
grep -q "ITER_V2" "$P1" && fail "prompt/env still mentions V2" || true
pass "local-file verbs native to V3: runtests (neutral/claims -> claim rows, stale parks), validate (+template), markers, teststate, usecase; V2 verbs retired"
# answered question flows back into the next run's mainwork
QO=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$GC/details" | jq -r '[.[]|select(.key=="question")]|last|.order')
curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$GC/details" | jq -c "[.[]|select(.key==\"question\")]|last|.value.fields[0].value=\"blue\"|{key:\"question\",valuetype:\"json\",value:.value}" \
  | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT/workitems/$GC/details/$QO" -d @- >/dev/null || fail "answer widget"
FRESH=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$GC")
echo "$FRESH" | jq '.state="queued"|.name="gate-answered"' | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT/workitems/$GC?expect_version=$(echo "$FRESH" | jq -r .version)" -d @- >/dev/null
(cd "$SAMPLE" && PATH="$FAKEBIN:$PATH" "$ENGINE_BIN" --config .iter/config.json --ticks 4 > "$SCRATCH/engine-answered.log" 2>&1) || true
PA=$(grep -l "# Step: mainwork" "$GATE_PROMPTS"/*-gate-answered.txt 2>/dev/null | head -1)
[ -n "$PA" ] || { cat "$SCRATCH/engine-answered.log"; fail "no mainwork turn after the answer"; }
grep -q "A question on this work item was answered" "$PA" && grep -q "Answer: blue" "$PA" || fail "answered question not surfaced in mainwork"
pass "answered question surfaces at the top of the next mainwork turn"
# agent delete (admin only) + engine self-register
CODE=$(curl -s -o /dev/null -w '%{http_code}' -H "authorization: Bearer $ENGINE_TOKEN" -X DELETE "$BASE/api/agents/gatetest")
[ "$CODE" = 403 ] || fail "engine role deleted an agent (HTTP $CODE)"
curl -sf "${AUTH[@]}" -X PUT "$BASE/api/agents/throwaway" -d '{"desc":"x","promptbody":"y"}' >/dev/null
curl -sf "${AUTH[@]}" -X DELETE "$BASE/api/agents/throwaway" | jq -e '.deleted==true' >/dev/null || fail "agent delete"
pass "agent delete: admin only"
cat > "$SAMPLE/.iter/config-new.json" <<EOF
{"data_url":"$BASE","token_envar":"ITER_ENGINE_TOKEN","engine_name":"EngineNew","env_file":"$SAMPLE/.env"}
EOF
(cd "$SAMPLE" && "$ENGINE_BIN" --config .iter/config-new.json --ticks 2 > "$SCRATCH/engine-selfreg.log" 2>&1) || true
grep -q "registered 'EngineNew'" "$SCRATCH/engine-selfreg.log" || { cat "$SCRATCH/engine-selfreg.log"; fail "engine did not self-register"; }
[ "$(curl -sf "${AUTH[@]}" "$BASE/api/engines/EngineNew" | jq -r .name)" = EngineNew ] || fail "self-registered engine record missing"
pass "engine self-registers on first start"
# delete: refused while heartbeating (409), allowed once stale; the project's engines list drops it
curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT" | jq '.engines += ["Ghost"]' | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT" -d @- >/dev/null
curl -sf "${AUTH[@]}" -X PUT "$BASE/api/engines/Ghost" -d "{\"host\":\"x\",\"state\":\"Running\",\"ticksec\":5,\"last_seen\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",\"projects\":{}}" >/dev/null || fail "ghost engine put"
CODE=$(curl -s -o /dev/null -w '%{http_code}' "${AUTH[@]}" -X DELETE "$BASE/api/engines/Ghost"); [ "$CODE" = 409 ] || fail "deleting a heartbeating engine should be 409 (got $CODE)"
curl -sf "${AUTH[@]}" "$BASE/api/engines/Ghost" | jq '.last_seen="2000-01-01T00:00:00Z"' | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/engines/Ghost" -d @- >/dev/null
CODE=$(curl -s -o /dev/null -w '%{http_code}' -H "authorization: Bearer $ENGINE_TOKEN" -X DELETE "$BASE/api/engines/Ghost"); [ "$CODE" = 403 ] || fail "engine role deleted an engine (HTTP $CODE)"
curl -sf "${AUTH[@]}" -X DELETE "$BASE/api/engines/Ghost" >/dev/null || fail "delete stale engine"
[ "$(curl -s -o /dev/null -w '%{http_code}' "${AUTH[@]}" "$BASE/api/engines/Ghost")" = 404 ] || fail "ghost engine still there"
curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT" | jq -e '.engines|index("Ghost")|not' >/dev/null || fail "project still lists the deleted engine"
pass "engine record delete: admin-only, refused while heartbeating, project engine lists cleaned"

# ---- spend recording + daily cap (2026-09-04) ----
SP=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/spend")
[ "$(echo "$SP" | jq 'length')" -ge 1 ] || fail "no spend rows after agent runs"
[ "$(echo "$SP" | jq -r '.[0].usd > 0')" = true ] || fail "spend usd not accumulated: $(echo "$SP" | jq -c '.[0]')"
curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$GR/details" | jq -e '[.[]|select(.key=="spend" and .value.usd>0)]|length>=1' >/dev/null || fail "no spend detail row on a run item"
pass "spend: per-run row on the item + project daily total from claude's cost report"
curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT" | jq '.maxdailycost=0' | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT" -d @- >/dev/null
BC=$(curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems" -d '{"name":"budget blocked","agent":"exec","exec_shell":"echo b > out_budget.txt","priority":0,"lockdirs":["{topdir}/bud/"]}' | jq -r .id)
(cd "$SAMPLE" && PATH="$FAKEBIN:$PATH" "$ENGINE_BIN" --config .iter/config.json --ticks 3 > "$SCRATCH/engine-budget.log" 2>&1) || true
[ ! -f "$SAMPLE/out_budget.txt" ] || fail "engine picked work with maxdailycost=0"
grep -q "daily budget is zero" "$SCRATCH/engine-budget.log" || fail "budget hold not logged"
curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT" | jq '.maxdailycost=null' | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT" -d @- >/dev/null
pass "maxdailycost=0 holds all picks (null = unlimited restored)"

# ---- account rotation with long-lived tokens (2026-09-04) ----
# two accounts by env-var NAME; usage files drive the ladder; the fake claude records which token it was given
export ACCT_A_TOKEN="tok-A-1yr" ACCT_B_TOKEN="tok-B-1yr"
curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT" | jq '.accounts=[{"name":"A","token_envar":"ACCT_A_TOKEN","order":1,"switch":80,"stop":99},{"name":"B","token_envar":"ACCT_B_TOKEN","order":2,"switch":80,"stop":99}]' \
  | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT" -d @- >/dev/null || fail "accounts update"
FUT=$(( $(date +%s) + 86400 ))
snap() { printf '{"ts":"%s","rate_limits":{"five_hour":{"used_percentage":%s,"resets_at":%s},"seven_day":{"used_percentage":%s,"resets_at":%s}}}\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$2" "$FUT" "$3" "$FUT" > "$ITER_USAGE_DIR/iter3-usage-$1.json"; }
snap A 85 10; snap B 5 5      # A over its switch% -> B must be used
: > "$GATE_PROMPTS/tokens.txt"
R1=$(curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems" -d '{"name":"gate-rot1","agent":"gatetest","priority":0,"lockdirs":["{topdir}/rot1/"]}' | jq -r .id)
(cd "$SAMPLE" && PATH="$FAKEBIN:$PATH" ITER_USAGE_DIR="$ITER_USAGE_DIR" "$ENGINE_BIN" --config .iter/config.json --ticks 5 > "$SCRATCH/engine-rot1.log" 2>&1) || true
grep -q "token=tok-B-1yr name=gate-rot1" "$GATE_PROMPTS/tokens.txt" || { cat "$GATE_PROMPTS/tokens.txt"; cat "$SCRATCH/engine-rot1.log"; fail "engine did not route the run to account B's token"; }
[ "$(curl -sf "${AUTH[@]}" "$BASE/api/engines/Engine01" | jq -r .account)" = B ] || fail "engine record does not show account B"
[ "$(curl -sf "${AUTH[@]}" "$BASE/api/engines/Engine01" | jq -r .usage.account)" = B ] || fail "usage snapshot on the chip is not B's"
[ "$(jq -r '.rate_limits.five_hour.used_percentage|round' "$ITER_USAGE_DIR/iter3-usage-B.json")" = 33 ] || fail "the run's rate_limit_event was not written to B's snapshot: $(cat "$ITER_USAGE_DIR/iter3-usage-B.json")"
[ "$(jq -r .source "$ITER_USAGE_DIR/iter3-usage-B.json")" = stream ] || fail "snapshot source is not 'stream'"
pass "per-run usage: the session's stream-json rate_limit_event (5h 33%) landed in the billed account's snapshot"
snap A 85 10; snap B 90 5     # both over switch%, both under stop% -> pass 2 picks A (lowest order)
: > "$GATE_PROMPTS/tokens.txt"
R2=$(curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems" -d '{"name":"gate-rot2","agent":"gatetest","priority":0,"lockdirs":["{topdir}/rot2/"]}' | jq -r .id)
(cd "$SAMPLE" && PATH="$FAKEBIN:$PATH" ITER_USAGE_DIR="$ITER_USAGE_DIR" "$ENGINE_BIN" --config .iter/config.json --ticks 5 > "$SCRATCH/engine-rot2.log" 2>&1) || true
grep -q "token=tok-A-1yr name=gate-rot2" "$GATE_PROMPTS/tokens.txt" || { cat "$GATE_PROMPTS/tokens.txt"; fail "second pass did not fall back to account A"; }
snap A 99 10; snap B 99 5     # both at stop% -> nothing runs
R3=$(curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems" -d '{"name":"gate-rot3","agent":"exec","exec_shell":"echo r3 > out_rot3.txt","priority":0,"lockdirs":["{topdir}/rot3/"]}' | jq -r .id)
(cd "$SAMPLE" && PATH="$FAKEBIN:$PATH" ITER_USAGE_DIR="$ITER_USAGE_DIR" "$ENGINE_BIN" --config .iter/config.json --ticks 3 > "$SCRATCH/engine-rot3.log" 2>&1) || true
[ ! -f "$SAMPLE/out_rot3.txt" ] || fail "engine ran with every account at stop%"
grep -q "all accounts at stop%" "$SCRATCH/engine-rot3.log" || fail "stop-hold not logged"
pass "account rotation: A over switch -> B's token used and reported; both over switch -> lowest order under stop; all at stop -> hold"
curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT" | jq '.accounts=[{"name":"TestAcct","token_envar":"FAKE_TOKEN","order":1,"switch":80,"stop":99}]' \
  | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT" -d @- >/dev/null
snap TestAcct 10 5

# connectivity test: webui POSTs /test, the engine nudges (fake claude) and reports back with usage
curl -sf "${AUTH[@]}" -X POST "$BASE/api/engines/Engine01/test" -d '{}' >/dev/null || fail "engine test request"
[ -n "$(curl -sf "${AUTH[@]}" "$BASE/api/engines/Engine01" | jq -r .test_requested)" ] || fail "test_requested not set"
(cd "$SAMPLE" && PATH="$FAKEBIN:$PATH" ITER_USAGE_DIR="$ITER_USAGE_DIR" "$ENGINE_BIN" --config .iter/config.json --ticks 2 > "$SCRATCH/engine-test.log" 2>&1) || true
grep -q "connectivity test OK" "$SCRATCH/engine-test.log" || { cat "$SCRATCH/engine-test.log"; fail "engine did not run the connectivity test"; }
ENG=$(curl -sf "${AUTH[@]}" "$BASE/api/engines/Engine01")
[ "$(echo "$ENG" | jq -r .test_requested)" = "" ] || fail "test_requested not cleared"
[ "$(echo "$ENG" | jq -r .test_result.ok)" = true ] || fail "test_result not ok: $(echo "$ENG" | jq -c .test_result)"
[ "$(echo "$ENG" | jq -r .usage.account)" = TestAcct ] || fail "usage snapshot not reported on heartbeat: $(echo "$ENG" | jq -c .usage)"
[ "$(echo "$ENG" | jq -r '.usage.five_hour_pct|floor')" = 10 ] || fail "usage pct wrong: $(echo "$ENG" | jq -c .usage)"
[ "$(grep -c "verifier\|worker" "$GATE_LOG")" -ge 1 ] || true
pass "engine test: nudge ran on haiku via fake claude, result + usage reported to iter_data"

# ---- idle usage probe: direct 1-token call, numbers from the headers (2026-09-04) ----
curl -sf "${AUTH[@]}" "$BASE/api/engines/Engine01" | jq '.probe_stale_min=1' | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/engines/Engine01" -d @- >/dev/null || fail "engine probe_stale_min"
STALE=$(date -u -v-2H +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -d '2 hours ago' +%Y-%m-%dT%H:%M:%SZ)
stale_snap() { printf '{"ts":"%s","rate_limits":{"five_hour":{"used_percentage":10,"resets_at":%s},"seven_day":{"used_percentage":5,"resets_at":%s}}}\n' "$STALE" "$FUTURE" "$FUTURE" > "$ITER_USAGE_DIR/iter3-usage-TestAcct.json"; }
stale_snap
export FAKE_TOKEN="tok-test-1yr"
: > "$PROBE_LOG"
(cd "$SAMPLE" && PATH="$FAKEBIN:$PATH" ITER_USAGE_DIR="$ITER_USAGE_DIR" "$ENGINE_BIN" --config .iter/config.json --ticks 3 > "$SCRATCH/engine-probe.log" 2>&1) || true
grep -q "usage probe 'TestAcct': 5h 42% 7d 10% (allowed_warning)" "$SCRATCH/engine-probe.log" || { cat "$SCRATCH/engine-probe.log"; fail "idle probe did not run"; }
grep -q "auth=Bearer tok-test-1yr beta=oauth-2025-04-20" "$PROBE_LOG" || { cat "$PROBE_LOG"; fail "probe did not send the account token as Bearer"; }
[ "$(jq -r '.rate_limits.five_hour.used_percentage|round' "$ITER_USAGE_DIR/iter3-usage-TestAcct.json")" = 42 ] || fail "probe snapshot not written"
[ "$(jq -r .source "$ITER_USAGE_DIR/iter3-usage-TestAcct.json")" = probe ] || fail "snapshot source is not 'probe'"
[ "$(curl -sf "${AUTH[@]}" "$BASE/api/engines/Engine01" | jq -r '.usage.five_hour_pct|floor')" = 42 ] || fail "probed usage not on the heartbeat"
# --probe from the shell does the same
(cd "$SAMPLE" && "$ENGINE_BIN" --config .iter/config.json --probe > "$SCRATCH/probe-cli.log" 2>&1) || true
grep -q "usage: 5h 42% 7d 10%" "$SCRATCH/probe-cli.log" || { cat "$SCRATCH/probe-cli.log"; fail "iter_engine --probe did not report usage"; }
pass "idle usage probe: direct 1-token call with the account token; 5h/7d from the headers -> snapshot + heartbeat; --probe from the shell"
# hard limit: the server rejects with 429 but still sends the headers -> the account reads 100% and nothing is picked
touch "$PROBE_LOG.reject"
stale_snap
curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems" -d '{"name":"probe-429 work","agent":"exec","exec_shell":"echo x > out_probe429.txt","priority":0,"lockdirs":["{topdir}/p429/"]}' >/dev/null || fail "probe-429 item"
(cd "$SAMPLE" && PATH="$FAKEBIN:$PATH" ITER_USAGE_DIR="$ITER_USAGE_DIR" "$ENGINE_BIN" --config .iter/config.json --ticks 3 > "$SCRATCH/engine-probe429.log" 2>&1) || true
grep -q "usage probe 'TestAcct': 5h 100% 7d 10% (rejected)" "$SCRATCH/engine-probe429.log" || { cat "$SCRATCH/engine-probe429.log"; fail "429 probe did not yield 100%"; }
[ ! -f "$SAMPLE/out_probe429.txt" ] || fail "engine ran work on an account the server rejects"
grep -q "all accounts at stop%" "$SCRATCH/engine-probe429.log" || fail "hard-limit hold not logged"
rm -f "$PROBE_LOG.reject"
snap TestAcct 10 5
curl -sf "${AUTH[@]}" "$BASE/api/engines/Engine01" | jq '.probe_stale_min=0' | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/engines/Engine01" -d @- >/dev/null
pass "hard limit: a 429 with headers reads as 100% -> no work picked, hold until a window resets"

# human answers the stuck item's widget with "accept" -> engine closes it without a run
GSO=$(echo "$GSQ" | jq -r .order)
echo "$GSQ" | jq '.value.fields[0].value="accept" | {key:.key,valuetype:.valuetype,value:.value}' \
  | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT/workitems/$GS/details/$GSO" -d @- >/dev/null || fail "answer gate widget"
FRESH=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$GS")
echo "$FRESH" | jq '.state="queued"' | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT/workitems/$GS?expect_version=$(echo "$FRESH" | jq -r .version)" -d @- >/dev/null || fail "requeue after answer"
BEFORE=$(grep -c "worker gate-stuck" "$GATE_LOG")
(cd "$SAMPLE" && PATH="$FAKEBIN:$PATH" "$ENGINE_BIN" --config .iter/config.json --ticks 4 > "$SCRATCH/engine-gate2.log" 2>&1) || true
[ "$(st_of "$GS")" = complete ] || { cat "$SCRATCH/engine-gate2.log"; fail "gate-stuck after accept state=$(st_of "$GS") (expected complete)"; }
[ "$(grep -c "worker gate-stuck" "$GATE_LOG")" = "$BEFORE" ] || fail "accept should close without running an agent"
pass "close gate: human 'accept' closes the item complete without spending a run"

# ---- ELI5 / explain (2026-09-04): on a CLOSED item, at once, outside the cap ----
curl -sf "${AUTH[@]}" -X PUT "$BASE/api/agents/explain" -d '{"model":"sonnet","max":1,"timeoutsec":300,"promptbody":"# Agent Definition: explain\n\nYou are the **explain** agent.\n"}' >/dev/null || fail "explain agent put"
GRX=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$GR"); [ "$(echo "$GRX" | jq -r .state)" = complete ] || fail "gate-recovers is not closed"
# Engine01 heartbeated seconds ago (previous run): age its record so no engine counts as live for this first request
curl -sf "${AUTH[@]}" "$BASE/api/engines/Engine01" | jq '.last_seen="2000-01-01T00:00:00Z"' | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/engines/Engine01" -d @- >/dev/null
R=$(curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems/$GR/explain" -d '{}') || fail "explain request"
[ -n "$(echo "$R" | jq -r .requested)" ] || fail "explain not stamped: $R"
[ "$(curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems/$GR/explain" -d '{}' | jq -r .already)" = true ] || fail "second explain request should report already"
[ -n "$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$GR" | jq -r .explain_requested)" ] || fail "explain_requested not on the item"
# no engine is heartbeating right now -> unassigned; a rival engine claims it, then Engine01 must be refused (409) and run nothing
[ "$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$GR" | jq -r .explain_engine)" = "" ] || fail "explain assigned with no live engine"
curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems/$GR/explain/claim" -d '{"engine":"Rival"}' >/dev/null || fail "rival claim"
CODE=$(curl -s -o /dev/null -w '%{http_code}' "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems/$GR/explain/claim" -d '{"engine":"Engine01"}')
[ "$CODE" = 409 ] || fail "second engine's claim should be 409 (got $CODE)"
(cd "$SAMPLE" && PATH="$FAKEBIN:$PATH" "$ENGINE_BIN" --config .iter/config.json --ticks 2 > "$SCRATCH/engine-eli5-rival.log" 2>&1) || true
grep -q "explained " "$SCRATCH/engine-eli5-rival.log" && fail "Engine01 ran an ELI5 assigned to another engine"
# release it (rival never ran): clear + re-request; with Engine01 heartbeating from the last run it is the one live engine -> assigned to it
curl -sf "${AUTH[@]}" -X DELETE "$BASE/api/projects/$PROJECT/workitems/$GR/explain" >/dev/null
curl -sf "${AUTH[@]}" "$BASE/api/engines/Engine01" | jq --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" '.last_seen=$ts|.state="Running"' | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/engines/Engine01" -d @- >/dev/null
R=$(curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems/$GR/explain" -d '{}')
[ "$(echo "$R" | jq -r .engine)" = Engine01 ] || fail "explain not assigned to the live engine: $R"
# the project is Stopped-or-Running either way; cap 0 must not matter: set maxagents else=0 for this run
curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT" | jq '.maxagents={"else":0}' | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT" -d @- >/dev/null
(cd "$SAMPLE" && PATH="$FAKEBIN:$PATH" "$ENGINE_BIN" --config .iter/config.json --ticks 3 > "$SCRATCH/engine-eli5.log" 2>&1) || true
curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT" | jq '.maxagents={"else":4}' | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT" -d @- >/dev/null
grep -q "ELI5 requested .* explaining now, outside the cap" "$SCRATCH/engine-eli5.log" || { cat "$SCRATCH/engine-eli5.log"; fail "engine did not pick up the ELI5 request"; }
grep -q "explained .* 'gate-recovers'" "$SCRATCH/engine-eli5.log" || { cat "$SCRATCH/engine-eli5.log"; fail "engine did not report the explanation"; }
EX=$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$GR/details" | jq -r '[.[]|select(.key=="explained")]|last|.value')
grep -q "EXPLAINED-MARKER" <<<"$EX" || fail "no 'explained' detail row on the closed item: $(keys_of "$GR")"
[ "$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$GR" | jq -r '.explain_requested+.explain_engine')" = "" ] || fail "explain_requested/explain_engine not cleared"
[ "$(st_of "$GR")" = complete ] || fail "explain changed the item's state"
grep -q -- "--allowedTools Read,Glob,Grep --disallowedTools Bash,Edit,Write" "$GATE_LOG" || { grep -A3 "explain tools" "$GATE_LOG" | tail -5; fail "explain session was not read-only"; }
for m in "# Explain this work item simply (ELI5)" "READ-ONLY" "Title: gate-recovers" "### request" "### response" "main.iter.md" "reqs/techreq.md"; do
  grep -qF -- "$m" "$GATE_PROMPTS/eli5-prompt.txt" || { head -40 "$GATE_PROMPTS/eli5-prompt.txt"; fail "ELI5 prompt lacks: $m"; }
done
grep -q "### spend" "$GATE_PROMPTS/eli5-prompt.txt" && fail "ELI5 prompt should not carry spend rows"
[ "$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$GR/details" | jq '[.[]|select(.key=="spend" and .value.agent=="explain")]|length')" = 1 ] || fail "explain spend row missing"
pass "ELI5: one engine only (random live engine at request, claim 409s a rival) -> explains a closed item at once with cap 0 (read-only tools), 'Explained Simply' row appended, flags cleared, spend recorded"

# webui served
[ "$(curl -sf "$BASE/" | grep -c "ITER")" -ge 1 ] || fail "webui not served"
pass "webui static page served"

[ "$BACKEND" = "dynamodb" ] && say "cleaning up iter3_e2e_* tables (also done on failure)"

say "ALL E2E CHECKS PASSED ($BACKEND) — logs in $SCRATCH"
