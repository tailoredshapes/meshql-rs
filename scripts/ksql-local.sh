#!/usr/bin/env bash
# Start a local Kafka + REST Proxy + ksqlDB stack and print the environment the
# ksql certification needs.
#
# The ksql adapter used to skip its own certification when no Confluent Cloud
# cluster was configured, which meant it reported success on every machine that
# had never configured one — and hid two real defects. It now fails instead, so
# this script exists to make running it the easy path.
#
#   eval "$(scripts/ksql-local.sh)"
#   cargo test -p meshql-ksql
#
# Stop it with: docker compose -f scripts/ksql-local.yml -p meshqlksql down -v
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
project=meshqlksql

docker compose -f "$here/ksql-local.yml" -p "$project" up -d >&2

for _ in $(seq 1 90); do
    if curl -sf http://localhost:18083/kafka/v3/clusters >/dev/null 2>&1 \
       && [ "$(curl -s -o /dev/null -w '%{http_code}' http://localhost:8088/info)" = "200" ]; then
        break
    fi
    sleep 2
done

cluster_id="$(curl -s http://localhost:18083/kafka/v3/clusters \
    | python3 -c 'import sys,json; print(json.load(sys.stdin)["data"][0]["cluster_id"])')"

if [ -z "$cluster_id" ]; then
    echo "ksql-local: stack did not become reachable" >&2
    exit 1
fi

cat <<ENV
export CONFLUENT_KAFKA_REST_URL=http://localhost:18083
export CONFLUENT_KAFKA_CLUSTER_ID=$cluster_id
export CONFLUENT_KAFKA_API_KEY=local
export CONFLUENT_KAFKA_API_SECRET=local
export CONFLUENT_KSQLDB_URL=http://localhost:8088
export CONFLUENT_KSQLDB_API_KEY=local
export CONFLUENT_KSQLDB_API_SECRET=local
ENV
