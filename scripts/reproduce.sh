#!/usr/bin/env sh
# Rebuild results/ from the sample day in data/.
#   scripts/reproduce.sh          full day (run scripts/fetch_data.sh first)
#   scripts/reproduce.sh --fast   committed synthetic stream, a few seconds
set -eu
cd "$(dirname "$0")/.."
exec cargo run --release --locked -- reproduce "$@"
