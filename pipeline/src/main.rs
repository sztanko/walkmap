mod config;
mod dijkstra;
mod mvt;
mod mvtbench;
mod pmt;
mod elevation;
mod graph;
mod grid;
mod group;
mod osm;
mod output;
mod polygonize;
mod snap;
mod tiles;
mod weights;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::time::Instant;

const DATA_URL_TEMPLATE: &str = "https://sztanko.github.io/walkmap-data-{city}/";
const SNAP_FEATURE_M: f64 = 200.0;
const SNAP_BUILDING_M: f64 = 300.0;
const GRID_MAX_M: f64 = 250.0;
const MIN_COMPONENT: usize = 30;
const MIN_ISLAND_M2: f64 = 1200.0; // absorb partition islands smaller than this
const LAST_LEG_MS: f64 = 1.39; // m/s for the snap-distance last leg

#[derive(Parser)]
#[command(about = "walkmap pipeline: OSM → walking-time network Voronoi → PMTiles")]
struct Cli {
    /// repo root (auto-detected by walking up to find config/cities.yaml)
    #[arg(long, default_value = ".")]
    root: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the full pipeline for one city
    Run {
        city: String,
        /// comma-separated feature type ids (default: all configured for the city)
        #[arg(long)]
        types: Option<String>,
        /// re-extract from the PBF even if cached
        #[arg(long)]
        force: bool,
        /// stop before tippecanoe (debugging)
        #[arg(long)]
        skip_tiles: bool,
    },
    /// Regenerate web/data/manifest.json from config + data/out
    Manifest,
    /// Count candidate features for EVERY catalogue type in a city —
    /// research input for choosing which groups make sense there
    Analyze { city: String },
    /// EXPERIMENT: benchmark a bespoke MVT encoder on a city's buildings
    BenchMvt { city: String },
}

fn find_root(start: &Path) -> Result<PathBuf> {
    let mut p = start.canonicalize()?;
    loop {
        if p.join("config/cities.yaml").exists() {
            return Ok(p);
        }
        if !p.pop() {
            bail!("config/cities.yaml not found in any parent of the given --root");
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = find_root(&cli.root)?;
    let cities = config::load_cities(&root.join("config"))?;
    let catalogue = config::load_feature_types(&root.join("config"))?;
    match cli.cmd {
        Cmd::Run { city, types: only, force, skip_tiles } => {
            let city = cities
                .iter()
                .find(|c| c.id == city)
                .with_context(|| format!("unknown city '{city}'"))?;
            let types = config::resolve_types(city, &catalogue)?;
            if types.len() > 32 {
                bail!("at most 32 feature types per city are supported");
            }
            let selected: Vec<usize> = match &only {
                None => (0..types.len()).collect(),
                Some(list) => list
                    .split(',')
                    .map(|id| {
                        types
                            .iter()
                            .position(|t| t.id == id)
                            .with_context(|| format!("type '{id}' not configured for {}", city.id))
                    })
                    .collect::<Result<_>>()?,
            };
            run_city(&root, city, &types, &selected, force, skip_tiles)?;
            output::write_manifest(
                &root.join("web/data"),
                &root.join("data/out"),
                &cities,
                &catalogue,
                DATA_URL_TEMPLATE,
            )?;
        }
        Cmd::Manifest => {
            output::write_manifest(
                &root.join("web/data"),
                &root.join("data/out"),
                &cities,
                &catalogue,
                DATA_URL_TEMPLATE,
            )?;
            eprintln!("wrote web/data/manifest.json");
        }
        Cmd::Analyze { city } => {
            let city = cities
                .iter()
                .find(|c| c.id == city)
                .with_context(|| format!("unknown city '{city}'"))?;
            analyze_city(&root, city, &catalogue)?;
        }
        Cmd::BenchMvt { city } => {
            let city = cities
                .iter()
                .find(|c| c.id == city)
                .with_context(|| format!("unknown city '{city}'"))?;
            let types = config::resolve_types(city, &catalogue)?;
            let cache = root
                .join("data/work")
                .join(&city.id)
                .join(format!("extract_{:016x}.bin", config::types_hash(&types)));
            let data = load_cache::<osm::CityData>(&cache, |_| true)
                .context("no cached extract — run the city first")?;
            mvtbench::bench(&data.buildings)?;
        }
    }
    Ok(())
}

/// Extract every catalogue type (with the city's variants) and report raw
/// counts, grouped site counts, and the median nearest-neighbour spacing —
/// the numbers that say whether a type makes a *nice* partition here.
fn analyze_city(root: &Path, city: &config::City, catalogue: &[config::FeatureType]) -> Result<()> {
    let mut all = city.clone();
    all.types = catalogue.iter().map(|t| t.id.clone()).collect();
    let types = config::resolve_types(&all, &catalogue.to_vec())?;

    let pbf_dir = root.join("data/pbf");
    let work = root.join("data/work").join(&city.id);
    std::fs::create_dir_all(&pbf_dir)?;
    std::fs::create_dir_all(&work)?;
    let pbf = pbf_dir.join(city.pbf_url.rsplit('/').next().unwrap());
    download_pbf(&city.pbf_url, &pbf)?;
    let cache = work.join(format!("extract_{:016x}.bin", config::types_hash(&types)));
    let data = match load_cache::<osm::CityData>(&cache, |_| true) {
        Some(d) => d,
        None => {
            eprintln!("[{}] extracting (all catalogue types)…", city.id);
            let d = osm::extract(&pbf, city, &types)?;
            store_cache(&cache, &d)?;
            d
        }
    };

    println!("\n{}: candidate feature groups", city.name);
    println!("{:<18} {:>6} {:>7} {:>10}   verdict", "type", "raw", "sites", "median-NN");
    for (i, t) in types.iter().enumerate() {
        let groups = group::group_features(&data.features[i]);
        let nn = median_nn_m(&groups);
        let verdict = if groups.len() < 6 {
            "too few — skip"
        } else if groups.len() < 15 {
            "marginal"
        } else {
            "good"
        };
        println!(
            "{:<18} {:>6} {:>7} {:>9}m   {} {}",
            t.id,
            data.features[i].len(),
            groups.len(),
            nn.map(|d| format!("{:.0}", d)).unwrap_or_else(|| "—".into()),
            verdict,
            if city.types.contains(&t.id) { "(configured)" } else { "" },
        );
    }
    Ok(())
}

fn median_nn_m(groups: &[group::SiteGroup]) -> Option<f64> {
    if groups.len() < 2 {
        return None;
    }
    let lls: Vec<[f64; 2]> = groups.iter().map(|g| g.ll).collect();
    let mut ds: Vec<f64> = lls
        .par_iter()
        .enumerate()
        .map(|(i, g)| {
            let mut best = f64::MAX;
            for (j, o) in lls.iter().enumerate() {
                if i != j {
                    best = best.min(weights::haversine_m(*g, *o));
                }
            }
            best
        })
        .collect();
    ds.sort_by(|a, b| a.partial_cmp(b).unwrap());
    ds.get(ds.len() / 2).copied()
}

fn download_pbf(url: &str, dest: &Path) -> Result<()> {
    if dest.exists() {
        return Ok(());
    }
    eprintln!("downloading {url}");
    elevation::curl_download(url, dest)?;
    eprintln!("  saved {} ({} MB)", dest.display(), std::fs::metadata(dest)?.len() / 1_048_576);
    Ok(())
}

/// bincode-cached value with validation.
fn load_cache<T: serde::de::DeserializeOwned>(path: &Path, valid: impl Fn(&T) -> bool) -> Option<T> {
    let f = std::fs::File::open(path).ok()?;
    let v: T = bincode::deserialize_from(std::io::BufReader::new(f)).ok()?;
    valid(&v).then_some(v)
}

fn store_cache<T: serde::Serialize>(path: &Path, v: &T) -> Result<()> {
    bincode::serialize_into(std::io::BufWriter::new(std::fs::File::create(path)?), v)?;
    Ok(())
}

/// Fingerprint of the node set. Grid caches store node INDICES, which are
/// only meaningful for the exact node array they were built against — a grid
/// reused across extracts scrambles every partition label.
fn nodes_fingerprint(node_ll: &[[f64; 2]]) -> u64 {
    use std::hash::Hasher;
    let mut h = rustc_hash::FxHasher::default();
    h.write_usize(node_ll.len());
    let step = (node_ll.len() / 997).max(1);
    for p in node_ll.iter().step_by(step) {
        h.write_u64(p[0].to_bits());
        h.write_u64(p[1].to_bits());
    }
    h.finish()
}

fn cached_grid(
    file: &Path,
    snapper: &snap::Snapper,
    bbox: [f64; 4],
    cell_m: f64,
    node_ll: &[[f64; 2]],
    force: bool,
) -> Result<grid::Grid> {
    let fp = nodes_fingerprint(node_ll);
    if !force {
        if let Some((_, g)) = load_cache::<(u64, grid::Grid)>(file, |(cached_fp, g)| {
            *cached_fp == fp && g.nearest.len() == g.dist_dm.len()
        }) {
            return Ok(g);
        }
    }
    eprintln!("  building {cell_m}m nearest-node grid…");
    let g = grid::Grid::build(snapper, bbox, cell_m, GRID_MAX_M);
    store_cache(file, &(fp, &g))?;
    Ok(g)
}

fn run_city(
    root: &Path,
    city: &config::City,
    types: &[config::FeatureType],
    selected: &[usize],
    force: bool,
    skip_tiles: bool,
) -> Result<()> {
    let t0 = Instant::now();
    let pbf_dir = root.join("data/pbf");
    let dem_dir = root.join("data/dem");
    let work = root.join("data/work").join(&city.id);
    let out = root.join("data/out").join(&city.id);
    for d in [&pbf_dir, &dem_dir, &work, &out] {
        std::fs::create_dir_all(d)?;
    }

    // 1. fetch
    let fname = city.pbf_url.rsplit('/').next().unwrap();
    let pbf = pbf_dir.join(fname);
    download_pbf(&city.pbf_url, &pbf)?;

    // 2. extract (cache keyed by the resolved feature-type config)
    let extract_file = work.join(format!("extract_{:016x}.bin", config::types_hash(types)));
    if force {
        let _ = std::fs::remove_file(&extract_file);
    }
    // stale extract caches from older configs just waste disk — clean them
    for e in std::fs::read_dir(&work)? {
        let p = e?.path();
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if (name.starts_with("extract") && p != extract_file)
            || name.starts_with("grid_") // pre-v2: no dist_dm
            || name.starts_with("gridv2_") // pre-v3: no node-set fingerprint
        {
            let _ = std::fs::remove_file(p);
        }
    }
    let data = match load_cache::<osm::CityData>(&extract_file, |_| true) {
        Some(d) => d,
        None => {
            eprintln!("[{}] extracting…", city.id);
            let d = osm::extract(&pbf, city, types)?;
            store_cache(&extract_file, &d)?;
            d
        }
    };

    // 3. prune weak components
    let (node_ll, segments) =
        graph::prune(data.node_ll.clone(), data.segments.clone(), MIN_COMPONENT);
    if node_ll.is_empty() {
        bail!("empty walking graph");
    }

    // 4. elevation (cached; node-set fingerprinted — it's index-aligned)
    let node_fp = nodes_fingerprint(&node_ll);
    let elev_file = work.join("elevv2.bin");
    if force {
        let _ = std::fs::remove_file(&elev_file);
    }
    let _ = std::fs::remove_file(work.join("elev.bin")); // pre-v2: unfingerprinted
    let elev: Vec<f32> = match load_cache::<(u64, Vec<f32>)>(&elev_file, |(fp, v)| {
        *fp == node_fp && v.len() == node_ll.len()
    }) {
        Some((_, v)) => v,
        None => {
            eprintln!("[{}] sampling elevation…", city.id);
            let dem = elevation::Dem::load_for(&dem_dir, &node_ll)?;
            let v = dem.sample_all(&node_ll);
            store_cache(&elev_file, &(node_fp, &v))?;
            v
        }
    };

    // 5. reversed weighted graph
    let csr = graph::build_rev_csr(&node_ll, &elev, &segments);

    // 6. snapping structures
    let bbox = city.bbox.unwrap_or_else(|| extent(&node_ll));
    let lat0 = (bbox[1] + bbox[3]) / 2.0;
    let snapper = snap::Snapper::new(&node_ll, lat0);
    let bld_snap: Vec<Option<(u32, f64)>> = data
        .buildings
        .par_iter()
        .map(|b| snapper.nearest(b.centroid, SNAP_BUILDING_M))
        .collect();

    // 7. nearest-node grids: fine (polygons/buildings) + coarse (path rasters)
    let g = cached_grid(
        &work.join(format!("gridv3_{}.bin", city.grid_m)),
        &snapper,
        bbox,
        city.grid_m,
        &node_ll,
        force,
    )?;
    // the path raster shares the fine grid: a coarser one snaps hover points
    // to different nodes than the area raster, making traces appear to start
    // in (and cross) the wrong catchment
    eprintln!(
        "[{}] grid {}×{} ({:.1}M cells), {:.0}% covered",
        city.id,
        g.w,
        g.h,
        (g.w as f64 * g.h as f64) / 1e6,
        100.0 * g.nearest.iter().filter(|&&n| n != grid::NODATA).count() as f64
            / g.nearest.len() as f64,
    );

    // 8. per feature type (compute + FGB writes; tiling jobs run in parallel after)
    let mut tile_jobs: Vec<tiles::TileJob> = Vec::new();
    let mut type_attrs: Vec<mvt::TypeAttrs> = Vec::new();
    for &ti in selected {
        let ft = &types[ti];
        let tt = Instant::now();

        // group near-duplicate features (paired directional stops etc.),
        // then seed EVERY member's snapped node with the group's pid
        let groups = group::group_features(&data.features[ti]);
        let mut sites: Vec<output::SiteOut> = Vec::with_capacity(groups.len());
        let mut seeds: Vec<(u32, u32)> = Vec::new();
        let mut seen_nodes: rustc_hash::FxHashSet<u32> = rustc_hash::FxHashSet::default();
        let mut unsnapped = 0usize;
        for gr in &groups {
            let pid = sites.len() as u32;
            let mut any = false;
            for ll in &gr.member_lls {
                if let Some((node, _)) = snapper.nearest(*ll, SNAP_FEATURE_M) {
                    if seen_nodes.insert(node) {
                        seeds.push((node, pid));
                    }
                    any = true;
                }
            }
            if !any {
                unsnapped += 1;
            }
            sites.push(output::SiteOut {
                pid,
                name: gr.name.clone(),
                ll: gr.ll,
                k: gr.member_lls.len() as u32,
            });
        }
        if seeds.is_empty() {
            eprintln!("[{}] {}: no snappable features — skipped", city.id, ft.id);
            continue;
        }

        // multi-source dijkstra on the reversed graph
        let (label, dist, next_hop) = dijkstra::partition(&csr, &seeds);
        let reached = label.iter().filter(|&&l| l != dijkstra::UNREACHED).count();

        // partition polygons + adjacency from the fine grid
        let cell_labels: Vec<u32> = g
            .nearest
            .par_iter()
            .map(|&n| {
                if n == grid::NODATA {
                    polygonize::NODATA
                } else {
                    let l = label[n as usize];
                    if l == dijkstra::UNREACHED {
                        polygonize::NODATA
                    } else {
                        l
                    }
                }
            })
            .collect();
        let min_island = (MIN_ISLAND_M2 / (city.grid_m * city.grid_m)).round() as usize;
        let (polys, adjacency) =
            polygonize::polygonize(&cell_labels, g.w as usize, g.h as usize, true, min_island);
        let colors = output::assign_colors(sites.len(), &adjacency);

        let mut t_max = vec![0u32; sites.len()];
        for (i, &l) in label.iter().enumerate() {
            if l != dijkstra::UNREACHED && dist[i] != u32::MAX {
                t_max[l as usize] = t_max[l as usize].max(dist[i] / 10);
            }
        }
        let parts: Vec<output::PartitionOut> = polys
            .into_iter()
            .map(|lp| output::PartitionOut {
                pid: lp.label,
                name: sites[lp.label as usize]
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("{} #{}", ft.name, lp.label)),
                t_max_s: t_max[lp.label as usize],
                color: colors[lp.label as usize],
                polys: lp
                    .polys
                    .into_iter()
                    .map(|rings| {
                        rings
                            .into_iter()
                            .map(|r| r.into_iter().map(|p| g.corner_ll(p[0], p[1])).collect())
                            .collect()
                    })
                    .collect(),
            })
            .collect();

        // buildings: pid + walking time
        let pid_t: Vec<Option<(u32, Option<u32>)>> = data
            .buildings
            .par_iter()
            .zip(bld_snap.par_iter())
            .map(|(b, snap)| {
                if let Some((node, d)) = snap {
                    let l = label[*node as usize];
                    if l != dijkstra::UNREACHED {
                        let t = dist[*node as usize] / 10 + (d / LAST_LEG_MS) as u32;
                        return Some((l, Some(t)));
                    }
                }
                // fall back to the grid label at the centroid (no time)
                let x = ((b.centroid[0] - g.west) / g.dlng).floor();
                let y = ((g.north - b.centroid[1]) / g.dlat).floor();
                if x >= 0.0 && y >= 0.0 && (x as u32) < g.w && (y as u32) < g.h {
                    let n = g.nearest[y as usize * g.w as usize + x as usize];
                    if n != grid::NODATA {
                        let l = label[n as usize];
                        if l != dijkstra::UNREACHED {
                            return Some((l, None));
                        }
                    }
                }
                None
            })
            .collect();

        let part_path = work.join(format!("{}.partitions.fgb", ft.id));
        output::write_partitions_fgb(&part_path, &parts)?;
        let n_bld = pid_t.iter().flatten().count();
        output::write_sites_json(&out.join(format!("{}.sites.json", ft.id)), &sites)?;

        // walk-path direction raster (same grid as the area polygons)
        let dirs = g.direction_field(&node_ll, &next_hop, &dist);
        output::write_dirs_gz(&out.join(format!("{}.dirs.gz", ft.id)), &dirs)?;

        // stats
        let grouped = sites.iter().filter(|s| s.k > 1).count();
        let mut ts: Vec<u32> = pid_t.iter().filter_map(|x| x.and_then(|(_, t)| t)).collect();
        ts.sort_unstable();
        let med = ts.get(ts.len() / 2).copied().unwrap_or(0);
        eprintln!(
            "[{}] {}: {} sites ({} grouped, {} unsnapped), {:.0}% nodes reached, {} buildings (median {:.0} min) [{:.0}s]",
            city.id,
            ft.id,
            sites.len(),
            grouped,
            unsnapped,
            100.0 * reached as f64 / label.len() as f64,
            n_bld,
            med as f64 / 60.0,
            tt.elapsed().as_secs_f64(),
        );

        if !skip_tiles {
            tile_jobs.push(tiles::TileJob {
                label: format!("{}/{}", city.id, ft.id),
                out: out.join(format!("{}.pmtiles", ft.id)),
                partitions_fgb: part_path,
                buildings_pmtiles: work.join(format!("{}.blds.pmtiles", ft.id)),
            });
            type_attrs.push(mvt::TypeAttrs { id: ft.id.clone(), pid_t, colors });
        }
    }

    if !tile_jobs.is_empty() {
        let tt = Instant::now();
        eprintln!("[{}] encoding buildings (bespoke MVT, {} types)…", city.id, type_attrs.len());
        mvt::build_buildings_archives(&data.buildings, &type_attrs, &work, g.bbox())?;
        eprintln!(
            "[{}] buildings encoded in {:.0}s; tiling {} partition archives ({} in parallel)…",
            city.id,
            tt.elapsed().as_secs_f64(),
            tile_jobs.len(),
            tiles::PARALLEL_JOBS
        );
        tiles::run_jobs(tile_jobs)?;
        eprintln!("[{}] tiling done in {:.0}s", city.id, tt.elapsed().as_secs_f64());
    }

    // drop outputs of types no longer configured for this city
    for e in std::fs::read_dir(&out)? {
        let p = e?.path();
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if let Some(tid) = name
            .strip_suffix(".pmtiles")
            .or_else(|| name.strip_suffix(".sites.json"))
            .or_else(|| name.strip_suffix(".dirs.gz"))
        {
            if !city.types.iter().any(|t| t == tid) {
                eprintln!("[{}] removing stale output {}", city.id, name);
                let _ = std::fs::remove_file(p);
            }
        }
    }
    output::write_city_meta(&out.join("meta.json"), city, g.bbox(), &g)?;
    eprintln!("[{}] done in {:.0}s", city.id, t0.elapsed().as_secs_f64());
    Ok(())
}

fn extent(pts: &[[f64; 2]]) -> [f64; 4] {
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for p in pts {
        b[0] = b[0].min(p[0]);
        b[1] = b[1].min(p[1]);
        b[2] = b[2].max(p[0]);
        b[3] = b[3].max(p[1]);
    }
    b
}
