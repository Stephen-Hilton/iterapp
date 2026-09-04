#!/usr/bin/env bash
# Deploy iter_data as an AWS Lambda (arm64, provided.al2023) behind an API
# Gateway HTTP API, against the production DynamoDB tables (prefix iter3_).
# Idempotent: creates the role/function/API on first run, updates after.
# (A Lambda function URL was tried first and answered 403 in this account
# despite a correct resource policy; the HTTP API works and is what we use.)
#
# Needs: cargo-lambda (pip3 install cargo-lambda), aws cli with admin creds
# in the repo .env (AWS_*), and ITER_JWT_SECRET + ITER_ADMIN_PASSWORD there.
# The JWT secret MUST be the same one local iter_data uses, so tokens minted
# by either are valid on both.
#
# Usage: iter3/deploy_lambda.sh [--no-build]
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
FN="${ITER_LAMBDA_NAME:-iter3_data}"
ROLE="${ITER_LAMBDA_ROLE:-iter3_data_lambda}"
PREFIX="${ITER_PREFIX:-iter3_}"
export PATH="$HOME/.cargo/bin:$PATH"

# creds + secrets from the repo .env (KEY=VALUE lines only)
set -a
eval "$(grep -E '^(AWS_[A-Z_]+|ITER_JWT_SECRET|ITER_ADMIN_PASSWORD)=' "$REPO/.env" | sed 's/^\(.*\)$/\1/')"
set +a
REGION="${AWS_REGION:-${AWS_DEFAULT_REGION:-us-west-2}}"
ACCOUNT=$(aws sts get-caller-identity --query Account --output text)
[ -n "${ITER_JWT_SECRET:-}" ] || { echo "ITER_JWT_SECRET missing from .env"; exit 2; }

say() { printf '\033[36m[lambda]\033[0m %s\n' "$*"; }

if [ "${1:-}" != "--no-build" ]; then
  say "building iter_data for arm64 lambda"
  (cd "$REPO" && cargo lambda build --release --arm64 -p iter_data)
fi
BOOT="$REPO/target/lambda/iter_data/bootstrap"
[ -f "$BOOT" ] || { echo "missing $BOOT"; exit 1; }
ZIP="$REPO/target/lambda/iter_data/iter_data.zip"
(cd "$(dirname "$BOOT")" && rm -f "$ZIP" && zip -q -j "$ZIP" bootstrap)
say "bundle: $(du -h "$ZIP" | cut -f1)"

# ---- IAM role: basic execution + DynamoDB on iter3_* tables only ----
if ! aws iam get-role --role-name "$ROLE" >/dev/null 2>&1; then
  say "creating role $ROLE"
  aws iam create-role --role-name "$ROLE" --assume-role-policy-document '{
    "Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"lambda.amazonaws.com"},"Action":"sts:AssumeRole"}]}' >/dev/null
  aws iam attach-role-policy --role-name "$ROLE" --policy-arn arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole
  sleep 8   # IAM propagation before the function can assume it
fi
aws iam put-role-policy --role-name "$ROLE" --policy-name iter3-dynamodb --policy-document "{
  \"Version\":\"2012-10-17\",\"Statement\":[
    {\"Effect\":\"Allow\",\"Action\":\"dynamodb:ListTables\",\"Resource\":\"*\"},
    {\"Effect\":\"Allow\",\"Action\":\"dynamodb:*\",\"Resource\":[
      \"arn:aws:dynamodb:$REGION:$ACCOUNT:table/${PREFIX}*\",
      \"arn:aws:dynamodb:$REGION:$ACCOUNT:table/${PREFIX}*/index/*\"]}]}"
ROLE_ARN="arn:aws:iam::$ACCOUNT:role/$ROLE"

ENVJSON="{\"Variables\":{\"ITER_PREFIX\":\"$PREFIX\",\"ITER_JWT_SECRET\":\"$ITER_JWT_SECRET\",\"ITER_ADMIN_PASSWORD\":\"${ITER_ADMIN_PASSWORD:-}\",\"RUST_LOG\":\"info\"}}"

# ---- function: create or update ----
if aws lambda get-function --function-name "$FN" >/dev/null 2>&1; then
  say "updating $FN code"
  aws lambda update-function-code --function-name "$FN" --zip-file "fileb://$ZIP" --architectures arm64 >/dev/null
  aws lambda wait function-updated --function-name "$FN"
  aws lambda update-function-configuration --function-name "$FN" --environment "$ENVJSON" \
    --memory-size 512 --timeout 30 --role "$ROLE_ARN" >/dev/null
  aws lambda wait function-updated --function-name "$FN"
else
  say "creating $FN"
  for attempt in 1 2 3 4 5 6; do
    if aws lambda create-function --function-name "$FN" --runtime provided.al2023 --architectures arm64 \
         --handler bootstrap --role "$ROLE_ARN" --zip-file "fileb://$ZIP" --memory-size 512 --timeout 30 \
         --environment "$ENVJSON" --description "iter V3 iter_data (axum via lambda_http) on DynamoDB $PREFIX" >/dev/null 2>"$REPO/target/lambda/create.err"; then
      break
    fi
    grep -q "role" "$REPO/target/lambda/create.err" && [ "$attempt" -lt 6 ] && { say "role not assumable yet, retrying"; sleep 10; continue; }
    cat "$REPO/target/lambda/create.err"; exit 1
  done
  aws lambda wait function-active --function-name "$FN"
fi

# ---- API Gateway HTTP API (public; the app does its own JWT auth) ----
API_ID=$(aws apigatewayv2 get-apis --query "Items[?Name=='$FN'].ApiId | [0]" --output text)
if [ -z "$API_ID" ] || [ "$API_ID" = None ]; then
  say "creating HTTP API $FN"
  API_ID=$(aws apigatewayv2 create-api --name "$FN" --protocol-type HTTP \
    --target "arn:aws:lambda:$REGION:$ACCOUNT:function:$FN" --query ApiId --output text)
fi
aws lambda add-permission --function-name "$FN" --statement-id "apigw-invoke-$API_ID" \
  --action lambda:InvokeFunction --principal apigateway.amazonaws.com \
  --source-arn "arn:aws:execute-api:$REGION:$ACCOUNT:$API_ID/*" >/dev/null 2>&1 || true
URL="$(aws apigatewayv2 get-api --api-id "$API_ID" --query ApiEndpoint --output text)/"

say "smoke: ${URL}health"
for i in $(seq 1 20); do
  if H=$(curl -sf --max-time 20 "${URL}health"); then echo "  $H"; break; fi
  [ "$i" = 20 ] && { echo "health check failed"; aws logs tail "/aws/lambda/$FN" --since 5m 2>/dev/null | tail -20; exit 1; }
  sleep 3
done
say "iter_data lambda is up"
echo
echo "LOGIN URL: $URL"
