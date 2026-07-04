mod config;
mod dijkstra;
mod elevation;
mod graph;
mod grid;
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
        /// comma-separated feature type ids (default: all)
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
    let types = config::load_feature_types(&root.join("config"))?;
    if types.len() > 32 {
        bail!("at most 32 feature types are supported");
    }
    match cli.cmd {
        Cmd::Run { city, types: only, force, skip_tiles } => {
            let city = cities
                .iter()
                .find(|c| c.id == city)
                .with_context(|| format!("unknown city '{city}'"))?;
            let selected: Vec<usize> = match &only {
                None => (0..types.len()).collect(),
                Some(list) => list
                    .split(',')
                    .map(|id| {
                        types
                            .iter()
                            .position(|t| t.id == id)
                            .with_context(|| format!("unknown feature type '{id}'"))
                    })
                    .collect::<Result<_>>()?,
            };
            run_city(&root, city, &types, &selected, force, skip_tiles)?;
            output::write_manifest(
                &root.join("web/data"),
                &root.join("data/out"),
                &cities,
                &types,
                DATA_URL_TEMPLATE,
            )?;
        }
        Cmd::Manifest => {
            output::write_manifest(
                &root.join("web/data"),
                &root.join("data/out"),
                &cities,
                &types,
                DATA_URL_TEMPLATE,
            )?;
            eprintln!("wrote web/data/manifest.json");
        }
    }
    Ok(())
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

fn cache<T: serde::Serialize + serde::de::DeserializeOwned>(
    path: &Path,
    force: bool,
    make: impl FnOnce() -> Result<T>,
) -> Result<T> {
    if !force && path.exists() {
        if let Ok(v) = bincode::deserialize_from(std::io::BufReader::new(std::fs::File::open(path)?))
        {
            return Ok(v);
        }
        eprintln!("  (stale cache {} — rebuilding)", path.display());
    }
    let v = make()?;
    bincode::serialize_into(std::io::BufWriter::new(std::fs::File::create(path)?), &v)?;
    Ok(v)
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

    // 2. extract (cached)
    eprintln!("[{}] extracting…", city.id);
    if force {
        for f in ["extract.bin", "elev.bin"] {
            let _ = std::fs::remove_file(work.join(f));
        }
        for e in std::fs::read_dir(&work)? {
            let p = e?.path();
            if p.file_name().and_then(|s| s.to_str()).is_some_and(|s| s.starts_with("grid_")) {
                let _ = std::fs::remove_file(p);
            }
        }
    }
    let data = cache(&work.join("extract.bin"), false, || osm::extract(&pbf, city, types))?;

    // 3. prune weak components
    let (node_ll, segments) = graph::prune(data.node_ll.clone(), data.segments.clone(), MIN_COMPONENT);
    if node_ll.is_empty() {
        bail!("empty walking graph");
    }

    // 4. elevation (cached; validated against node count)
    let elev: Vec<f32> = {
        let cached: Option<Vec<f32>> = (!force && work.join("elev.bin").exists())
            .then(|| {
                bincode::deserialize_from(std::io::BufReader::new(
                    std::fs::File::open(work.join("elev.bin")).ok()?,
                ))
                .ok()
            })
            .flatten()
            .filter(|v: &Vec<f32>| v.len() == node_ll.len());
        match cached {
            Some(v) => v,
            None => {
                eprintln!("[{}] sampling elevation…", city.id);
                let dem = elevation::Dem::load_for(&dem_dir, &node_ll)?;
                let v = dem.sample_all(&node_ll);
                bincode::serialize_into(
                    std::io::BufWriter::new(std::fs::File::create(work.join("elev.bin"))?),
                    &v,
                )?;
                v
            }
        }
    };
    {
        let (mut mn, mut mx) = (f32::MAX, f32::MIN);
        for &e in &elev {
            mn = mn.min(e);
            mx = mx.max(e);
        }
        eprintln!("[{}] elevation range: {:.0}–{:.0} m", city.id, mn, mx);
    }

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

    // 7. nearest-node grid (cached)
    let grid_file = work.join(format!("grid_{}.bin", city.grid_m));
    let g: grid::Grid = {
        let cached: Option<grid::Grid> = (!force && grid_file.exists())
            .then(|| {
                bincode::deserialize_from(std::io::BufReader::new(
                    std::fs::File::open(&grid_file).ok()?,
                ))
                .ok()
            })
            .flatten()
            .filter(|g: &grid::Grid| {
                g.nearest.iter().all(|&n| n == grid::NODATA || (n as usize) < node_ll.len())
            });
        match cached {
            Some(g) => g,
            None => {
                eprintln!("[{}] building {}m nearest-node grid…", city.id, city.grid_m);
                let g = grid::Grid::build(&snapper, bbox, city.grid_m, GRID_MAX_M);
                bincode::serialize_into(
                    std::io::BufWriter::new(std::fs::File::create(&grid_file)?),
                    &g,
                )?;
                g
            }
        }
    };
    eprintln!(
        "[{}] grid {}×{} ({:.1}M cells), {:.0}% covered",
        city.id,
        g.w,
        g.h,
        (g.w as f64 * g.h as f64) / 1e6,
        100.0 * g.nearest.iter().filter(|&&n| n != grid::NODATA).count() as f64
            / g.nearest.len() as f64
    );

    // 8. building geometries (reused across types)
    let geoms = output::building_geom_strings(&data.buildings);

    // 9. per feature type
    for &ti in selected {
        let ft = &types[ti];
        let tt = Instant::now();
        let feats = &data.features[ti];

        // snap features to sites (one site per graph node; nearest feature wins)
        let mut by_node: rustc_hash::FxHashMap<u32, (usize, f64)> = rustc_hash::FxHashMap::default();
        let mut unsnapped = 0usize;
        for (fi, f) in feats.iter().enumerate() {
            match snapper.nearest(f.ll, SNAP_FEATURE_M) {
                Some((node, d)) => {
                    let e = by_node.entry(node).or_insert((fi, d));
                    if d < e.1 {
                        *e = (fi, d);
                    }
                }
                None => unsnapped += 1,
            }
        }
        if by_node.is_empty() {
            eprintln!("[{}] {}: no snappable features — skipped", city.id, ft.id);
            continue;
        }
        let mut sites: Vec<output::SiteOut> = Vec::with_capacity(by_node.len());
        let mut seeds: Vec<(u32, u32)> = Vec::with_capacity(by_node.len());
        for (node, (fi, _)) in &by_node {
            let pid = sites.len() as u32;
            sites.push(output::SiteOut {
                pid,
                name: feats[*fi].name.clone(),
                ll: feats[*fi].ll,
            });
            seeds.push((*node, pid));
        }

        // multi-source dijkstra on the reversed graph
        let (label, dist) = dijkstra::partition(&csr, &seeds);
        let reached = label.iter().filter(|&&l| l != dijkstra::UNREACHED).count();

        // partition polygons from the grid
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
        let polys = polygonize::polygonize(&cell_labels, g.w as usize, g.h as usize);

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

        let part_path = work.join(format!("{}.partitions.geojsonl", ft.id));
        let bld_path = work.join(format!("{}.buildings.geojsonl", ft.id));
        output::write_partitions_geojsonl(&part_path, &parts)?;
        let n_bld = output::write_buildings_geojsonl(&bld_path, &geoms, &pid_t)?;
        output::write_sites_json(&out.join(format!("{}.sites.json", ft.id)), &sites)?;

        // stats
        let mut ts: Vec<u32> = pid_t.iter().filter_map(|x| x.and_then(|(_, t)| t)).collect();
        ts.sort_unstable();
        let med = ts.get(ts.len() / 2).copied().unwrap_or(0);
        let p90 = ts.get(ts.len() * 9 / 10).copied().unwrap_or(0);
        eprintln!(
            "[{}] {}: {} sites ({} unsnapped feats), {:.0}% nodes reached, {} buildings (median {:.0} min, p90 {:.0} min) [{:.0}s]",
            city.id,
            ft.id,
            sites.len(),
            unsnapped,
            100.0 * reached as f64 / label.len() as f64,
            n_bld,
            med as f64 / 60.0,
            p90 as f64 / 60.0,
            tt.elapsed().as_secs_f64(),
        );

        if !skip_tiles {
            let tile_path = out.join(format!("{}.pmtiles", ft.id));
            tiles::tippecanoe(&tile_path, &part_path, &bld_path)?;
            let _ = std::fs::remove_file(&part_path);
            let _ = std::fs::remove_file(&bld_path);
            eprintln!(
                "[{}] {}: tiles {} MB [total {:.0}s]",
                city.id,
                ft.id,
                std::fs::metadata(&tile_path)?.len() / 1_048_576,
                tt.elapsed().as_secs_f64()
            );
        }
    }

    output::write_city_meta(&out.join("meta.json"), city, g.bbox())?;
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
