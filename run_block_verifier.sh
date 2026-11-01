#!/usr/bin/env bash
# Runs block-verifier on the host, without Docker. Postgres must already be
# reachable at DATABASE_URL. Ctrl-C stops it gracefully.
set -euo pipefail

cd "$(dirname "$0")"

if [[ ! -f .env.block-verifier ]]; then
    echo "missing .env.block-verifier: copy .env.block-verifier.example and fill it in" >&2
    exit 1
fi

set -a
. ./.env.block-verifier
set +a

exec cargo run --release -p block-verifier
