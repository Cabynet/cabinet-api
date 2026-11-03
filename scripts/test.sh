#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
suffix="$$-$RANDOM"
postgres="cabinet-test-pg-$suffix"
redis="cabinet-test-redis-$suffix"
log=$(mktemp)
storage_pid=""
cleanup() {
  if [ -n "$storage_pid" ]; then kill "$storage_pid" 2>/dev/null || true; fi
  docker stop "$postgres" "$redis" >/dev/null 2>&1 || true
  rm -f "$log"
}
trap cleanup EXIT INT TERM
docker run --rm -d --name "$postgres" -e POSTGRES_PASSWORD=isolated-test-only -e POSTGRES_DB=cabinet_test -p 127.0.0.1::5432 postgres:17-alpine >/dev/null
docker run --rm -d --name "$redis" -p 127.0.0.1::6379 redis:alpine >/dev/null
for _ in {1..60}; do
  if docker exec "$postgres" pg_isready -U postgres >/dev/null 2>&1; then break; fi
  sleep 1
done
pg_port=$(docker port "$postgres" 5432/tcp | awk -F: '{print $NF}')
redis_port=$(docker port "$redis" 6379/tcp | awk -F: '{print $NF}')
export TEST_DATABASE_URL="postgresql://postgres:isolated-test-only@127.0.0.1:$pg_port/cabinet_test"
export TEST_REDIS_URL="redis://127.0.0.1:$redis_port"
export DATABASE_URL="$TEST_DATABASE_URL" REDIS_URL="$TEST_REDIS_URL"
export API_URL="http://localhost:4000" FRONTEND_URL="http://localhost:3000" COOKIE_DOMAIN=""
for migration in migrations/*.sql; do docker exec -i "$postgres" psql -U postgres -d cabinet_test -v ON_ERROR_STOP=1 < "$migration" >/dev/null; done
TEST_STORAGE_PORT=0 node scripts/storage-fixture.mjs > "$log" 2>&1 & storage_pid=$!
for _ in {1..50}; do if grep -q '^STORAGE_PORT=' "$log"; then break; fi; sleep 0.1; done
storage_port=$(awk -F= '/^STORAGE_PORT=/{print $2}' "$log")
if [ -z "$storage_port" ]; then printf 'Storage fixture failed; see %s\n' "$log"; exit 1; fi
export R2_ENDPOINT="http://127.0.0.1:$storage_port" R2_PUBLIC_URL="http://127.0.0.1:$storage_port/cabinet"
export R2_ACCESS_KEY_ID="test-access-key" R2_SECRET_ACCESS_KEY="test-secret-key" R2_BUCKET_NAME="cabinet"
cargo test --locked "$@" -- --test-threads=1
