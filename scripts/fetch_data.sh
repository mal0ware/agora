#!/usr/bin/env sh
# Download one Nasdaq TotalView-ITCH 5.0 sample day into data/ (3.5 GB).
# The file is never committed: the directory states no licence.
set -eu

DAY=12302019.NASDAQ_ITCH50
URL="https://emi.nasdaq.com/ITCH/Nasdaq%20ITCH/$DAY.gz"
SIZE=3524013057
# Nasdaq's own .md5sum for this file returns 404, so this is the MD5 of the
# copy downloaded on 2026-11-23. It guards against a changed or damaged file;
# it is not the publisher's checksum.
MD5=23514ef98821448a3f8f4c5b4095f3f5

cd "$(dirname "$0")/.."
mkdir -p data
curl --fail --location --retry 5 --continue-at - --output "data/$DAY.gz" "$URL"

got=$(wc -c < "data/$DAY.gz" | tr -d ' ')
if [ "$got" != "$SIZE" ]; then
    echo "error: data/$DAY.gz is $got bytes, expected $SIZE" >&2
    exit 1
fi
if command -v md5sum >/dev/null 2>&1; then
    sum=$(md5sum "data/$DAY.gz" | cut -d ' ' -f 1)
elif command -v md5 >/dev/null 2>&1; then
    sum=$(md5 -q "data/$DAY.gz")
else
    sum=$MD5
    echo "warning: no md5 tool found; size checked only" >&2
fi
if [ "$sum" != "$MD5" ]; then
    echo "error: data/$DAY.gz has MD5 $sum, expected $MD5" >&2
    exit 1
fi
echo "data/$DAY.gz is in place. Next: scripts/reproduce.sh"
