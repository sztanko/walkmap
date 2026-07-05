#!/usr/bin/env bash
# Rebuild (pipeline run + publish + manifest) one or more cities.
#
#   scripts/rebuild.sh london            # one city
#   scripts/rebuild.sh funchal calheta   # several
#   scripts/rebuild.sh all               # every city in config/cities.yaml
#
# Flags (before city ids):
#   --no-publish   run the pipeline only, skip the data-repo publish
#   --analyze      run `walkmap analyze` instead of the full pipeline
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
mkdir -p data/logs

PUBLISH=1
MODE=run
while [[ "${1:-}" == --* ]]; do
  case "$1" in
    --no-publish) PUBLISH=0 ;;
    --analyze) MODE=analyze ;;
    *) echo "unknown flag $1"; exit 1 ;;
  esac
  shift
done

[ $# -ge 1 ] || { echo "usage: rebuild.sh [--no-publish] [--analyze] <city ...|all>"; exit 1; }

if [ "$1" = "all" ]; then
  CITIES=$(grep '^- id:' config/cities.yaml | awk '{print $3}')
else
  CITIES="$*"
fi

(cd pipeline && cargo build --release 2>&1 | grep -E '^error|Finished')

fail=0
for c in $CITIES; do
  echo "=== $c ==="
  if [ "$MODE" = "analyze" ]; then
    ./pipeline/target/release/walkmap analyze "$c" || fail=1
    continue
  fi
  if ./pipeline/target/release/walkmap run "$c" > "data/logs/run_$c.log" 2>&1; then
    tail -2 "data/logs/run_$c.log"
    if [ "$PUBLISH" = 1 ]; then
      ./scripts/release.sh "$c" > "data/logs/rel_$c.log" 2>&1 \
        && echo "published $c" || { echo "PUBLISH FAILED $c (data/logs/rel_$c.log)"; fail=1; }
    fi
  else
    echo "RUN FAILED $c (data/logs/run_$c.log)"; fail=1
  fi
done

./pipeline/target/release/walkmap manifest
echo "done. commit web/data/manifest.json + push to deploy the site."
exit $fail
