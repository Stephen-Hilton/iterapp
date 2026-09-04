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
[ "$(curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$DG" | jq -r .state)" = parked ] || fail "failed-blocker dependent not parked"
curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT/workitems/$DG" | jq -r .lasterror | grep -q "closed failed" || fail "parked dependent lacks the failed-dependency note"
pass "dependencies are deep: waited on the blocker's child; shallow opt-out ran; failed blocker parked its dependent"
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
FUTURE=$(( $(date +%s) + 86400 ))
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
cat > "$FAKEBIN/claude" <<'FAKE'
#!/usr/bin/env bash
# fake claude for e2e: -p <prompt> ... ; prints a Claude Code json result object
prompt=""; while [ $# -gt 0 ]; do case "$1" in -p) prompt="$2"; shift;; esac; shift; done
name=$(grep -o -m1 'Workitem: gate-[a-z]*' <<<"$prompt" | sed 's/Workitem: //')
emit() { printf '{"type":"result","subtype":"%s","is_error":false,"num_turns":3,"result":%s}\n' "$1" "$(jq -Rn --arg t "$2" '$t')"; }
if grep -q "iter close-gate verifier" <<<"$prompt"; then
  echo "verifier $name" >> "$GATE_LOG"
  if grep -q "DONE-MARKER" <<<"$prompt"; then emit success '{"verdict":"complete","open":[],"reason":"all obligations met"}'
  else emit success '{"verdict":"incomplete","open":["file the ten build items"],"reason":"the message says it is waiting on a review"}'; fi
  exit 0
fi
echo "worker $name" >> "$GATE_LOG"
case "$name" in
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

curl -sf "${AUTH[@]}" -X PUT "$BASE/api/agents/gatetest" \
  -d '{"desc":"close-gate test agent","max":3,"timeoutsec":60,"model":"","promptbody":"You are the gate test agent.","closegate":{"verify":"haiku","max_bounces":1}}' >/dev/null || fail "gatetest agent put"
curl -sf "${AUTH[@]}" "$BASE/api/projects/$PROJECT" | jq '.agents.gatetest={"max":3}' \
  | curl -sf "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT" -d @- >/dev/null || fail "project gatetest override"
mk_gate_item() {
  curl -sf "${AUTH[@]}" -X POST "$BASE/api/projects/$PROJECT/workitems" \
    -d "{\"name\":\"$1\",\"agent\":\"gatetest\",\"priority\":2}" | jq -r .id
}
GR=$(mk_gate_item gate-recovers); GS=$(mk_gate_item gate-stuck); GT=$(mk_gate_item gate-turncap)
for W in "$GR" "$GS" "$GT"; do
  curl -sf "${AUTH[@]}" -X PUT "$BASE/api/projects/$PROJECT/workitems/$W/details/0" \
    -d '{"key":"request","valuetype":"text","value":"write the plan AND file its build items as workitems"}' >/dev/null || fail "gate request detail"
done
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

# webui served
[ "$(curl -sf "$BASE/" | grep -c "ITER")" -ge 1 ] || fail "webui not served"
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
