#!/usr/bin/env bash
# Publish one city's tiles to its GitHub Pages data repo: walkmap-data-<city>.
#
# GitHub release assets serve no CORS headers, so the browser can't read them;
# github.io serves `access-control-allow-origin: *` and supports Range requests,
# which is exactly what PMTiles needs. Hence: one small Pages repo per city
# (limits: 100 MB/file hard, ~1 GB/site soft).
set -euo pipefail

CITY="${1:?usage: release.sh <city-id>}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/data/out/$CITY"
REPO="walkmap-data-$CITY"
OWNER="sztanko"
STAGE="$ROOT/data/publish/$REPO"

[ -d "$OUT" ] || { echo "no output for $CITY (run the pipeline first)"; exit 1; }

# refuse files over the GitHub 100MB hard limit
big=$(find "$OUT" -name '*.pmtiles' -size +99M | head -1 || true)
[ -z "$big" ] || { echo "ERROR: $big exceeds GitHub's 100MB file limit"; exit 1; }

if ! gh repo view "$OWNER/$REPO" >/dev/null 2>&1; then
  echo "creating $OWNER/$REPO"
  gh repo create "$OWNER/$REPO" --public \
    --description "walkmap tiles for $CITY (PMTiles served via GitHub Pages)"
fi

rm -rf "$STAGE"
mkdir -p "$STAGE"
git -C "$STAGE" init -q -b main
git -C "$STAGE" config user.name "Demeter Sztanko"
git -C "$STAGE" config user.email "sztanko@gmail.com"

cp "$OUT"/*.pmtiles "$OUT"/*.sites.json "$OUT"/meta.json "$STAGE/"
touch "$STAGE/.nojekyll"
cat > "$STAGE/README.md" <<EOF
# walkmap data: $CITY

Generated PMTiles + site indexes for [walkmap](https://github.com/$OWNER/walkmap).
Served via GitHub Pages at https://$OWNER.github.io/$REPO/

Map data © OpenStreetMap contributors (ODbL). Elevation: Copernicus DEM GLO-30 © ESA.
EOF

git -C "$STAGE" add -A
git -C "$STAGE" commit -q -m "Publish $CITY tiles $(date -u +%Y-%m-%d)"
git -C "$STAGE" remote add origin "git@github.com:$OWNER/$REPO.git"
git -C "$STAGE" push -q -f origin main

# enable branch-based Pages (idempotent)
gh api -X POST "repos/$OWNER/$REPO/pages" \
  -f "source[branch]=main" -f "source[path]=/" >/dev/null 2>&1 \
  || gh api -X PUT "repos/$OWNER/$REPO/pages" \
       -f "source[branch]=main" -f "source[path]=/" >/dev/null 2>&1 || true

echo "published: https://$OWNER.github.io/$REPO/"
