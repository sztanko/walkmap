# walkmap

Natural division of cities into areas based on walking time to the nearest
amenity of communal importance — pubs, supermarkets, post offices, bus stops…

A network Voronoi diagram over the OSM walking graph, where the distance
metric is elevation-aware walking time (Tobler's hiking function) on a
directed graph, rendered as an interactive vector-tile map.

**Live map:** https://sztanko.github.io/walkmap/

## How it works

1. **Extract** — download a geofabrik OSM extract per city; parse walkable
   ways, building footprints and amenity features (tag rules in
   [config/feature_types.yaml](config/feature_types.yaml)).
2. **Graph** — build a directed graph of the walking network; sample
   elevation for every node from Copernicus GLO-30; weight each directed
   edge with Tobler's hiking function (uphill ≠ downhill).
3. **Partition** — one multi-source Dijkstra per feature type over reversed
   edges gives, for every node, the nearest feature *by walking time to it*
   and the time itself.
4. **Polygonize** — assign a fine grid (10 m) to nearest graph nodes, trace
   partition boundaries, and emit partition polygons + buildings annotated
   with `(partition, walking time)`.
5. **Tile** — tippecanoe → PMTiles, one archive per city per feature type,
   published to per-city GitHub Pages data repos (`walkmap-data-{city}`) and
   read via HTTP range requests by the static MapLibre site in [web/](web/).
   (GitHub Release assets serve no CORS headers, so Pages it is.)

## Development

See [ARCHITECTURE.md](ARCHITECTURE.md) for the full design, data flow, and
hard-won gotchas (read it before extending the pipeline or UI).

## Running the pipeline

Requirements: Rust (stable), tippecanoe ≥ 2.17, ~2–20 GB disk per city.

```sh
cd pipeline
cargo run --release -- run funchal        # small city, minutes
cargo run --release -- run london         # large city, hours (mostly tippecanoe)
```

Add a city: append an entry to [config/cities.yaml](config/cities.yaml)
(id, name, geofabrik `pbf_url`, optional `bbox`), run the pipeline, upload
the release (`scripts/release.sh <city>`).

## License

Code: MIT. Map data © OpenStreetMap contributors (ODbL).
Elevation: Copernicus DEM GLO-30 © ESA.
