#!/usr/bin/env bash
# Required MCP surface tier: one real SQLite backend, actual stdio, shared cases.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
contract=meshql-cert/tests/features/contract
[ -f "$contract/cert/mcp/run.py" ] || { echo "MCP tier requires the pinned shared contract submodule" >&2; exit 1; }
output=${1:-target/mcp-cert/results.json}
mkdir -p "$(dirname "$output")"
# Never open an existing shared lock for writing (fs.protected_regular).
python3 - <<'PY'
import os
try:
    fd = os.open('/tmp/fibsuite.lock', os.O_CREAT | os.O_EXCL | os.O_RDONLY, 0o666)
except FileExistsError:
    pass
else:
    os.close(fd)
PY
exec 9</tmp/fibsuite.lock
flock -x 9
# A failed or interrupted rebuild must not leave a reusable old pass.
rm -f -- "$output"
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2}
cargo test --locked -p meshql-mcp --features cert-fixture --lib
cargo build --locked -p meshql-mcp --features cert-fixture --bins
python3 "$contract/cert/mcp/run.py" --adapter "$root/target/debug/mcp-cert-adapter" --output "$output"
