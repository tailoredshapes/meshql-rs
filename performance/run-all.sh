#!/usr/bin/env bash
set -euo pipefail

BASE_URL="${1:-http://localhost:5088}"
PROFILE="${2:-load}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PASS=0
FAIL=0

echo "=== MeshQL k6 Performance Suite ==="
echo "BASE_URL: $BASE_URL"
echo "PROFILE:  $PROFILE"
echo ""

# Health check with retry
echo "Waiting for server at $BASE_URL ..."
for i in $(seq 1 30); do
  if curl -sf "$BASE_URL/farm/api" > /dev/null 2>&1; then
    echo "Server is ready."
    break
  fi
  if [ "$i" -eq 30 ]; then
    echo "ERROR: Server not reachable after 30 attempts."
    exit 1
  fi
  sleep 2
done

echo ""

# --- Validation gate ---
echo "--- Running: validate (correctness gate) ---"
if k6 run \
  -e "BASE_URL=$BASE_URL" \
  "$SCRIPT_DIR/tests/validate.js"; then
  echo "PASS: validate"
else
  echo "FAIL: validate"
  echo ""
  echo "=== ABORTING: correctness validation failed ==="
  echo "Performance results would be meaningless. Fix the server first."
  exit 1
fi
echo ""

# --- Performance tests ---
TESTS=(
  "rest-crud"
  "graphql-queries"
  "federation-depth"
  "mixed-workload"
)

for test in "${TESTS[@]}"; do
  echo "--- Running: $test ---"
  if k6 run \
    -e "BASE_URL=$BASE_URL" \
    -e "PROFILE=$PROFILE" \
    "$SCRIPT_DIR/tests/${test}.js"; then
    echo "PASS: $test"
    PASS=$((PASS + 1))
  else
    echo "FAIL: $test"
    FAIL=$((FAIL + 1))
  fi
  echo ""
done

echo "=== Results: $PASS passed, $FAIL failed (validation gate passed) ==="

if [ "$FAIL" -gt 0 ]; then
  exit 1
fi
