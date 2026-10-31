#!/usr/bin/env bash
# Runs block-saver on the host, without Docker. Postgres must already be
# reachable at DATABASE_URL. Ctrl-C stops it gracefully.
set -euo pipefail

cd "$(dirname "$0")"

if [[ ! -f .env.block-saver ]]; then
    echo "missing .env.block-saver: copy .env.block-saver.example and fill it in" >&2
    exit 1
fi

set -a
. ./.env.block-saver
set +a

exec cargo run --release -p block-saver
