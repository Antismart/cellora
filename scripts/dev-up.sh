#!/usr/bin/env bash
# Bring the Cellora dev stack up, wait for every service to become healthy,
# and apply database migrations. Safe to re-run.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

compose() { docker compose "$@"; }

echo "==> starting docker compose services"
compose up -d postgres redis ckb ckb-miner

echo "==> waiting for postgres + ckb to become healthy"
deadline=$(( $(date +%s) + 120 ))
while :; do
    ready=true
    for svc in postgres ckb; do
        state=$(compose ps --format json "${svc}" | head -n1 || true)
        if ! echo "${state}" | grep -q '"Health":"healthy"'; then
            ready=false
        fi
    done
    if ${ready}; then
        break
    fi
    if [ "$(date +%s)" -gt "${deadline}" ]; then
        echo "timed out waiting for services to become healthy" >&2
        compose ps
        exit 1
    fi
    sleep 2
done
echo "==> services healthy"

echo "==> applying database migrations"
if ! command -v sqlx >/dev/null 2>&1; then
    echo "sqlx-cli not found; installing with cargo" >&2
    cargo install --locked sqlx-cli --no-default-features --features rustls,postgres --version '~0.8'
fi
DATABASE_URL="${CELLORA_DATABASE_URL:-postgres://cellora:cellora@localhost:5432/cellora}" \
    sqlx migrate run --source "${repo_root}/migrations"

echo "==> cellora dev stack ready"
