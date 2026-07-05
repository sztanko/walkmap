use crate::config::{City, FeatureType};
use crate::osm::Building;
use anyhow::Result;
use rayon::prelude::*;
use std::fmt::Write as FmtWrite;
use std::io::Write;
use std::path::Path;

/// Fixed 6-colour palette (colour-blind aware); partitions are graph-coloured
/// so no two adjacent partitions share a palette entry.
pub const PALETTE: [&str; 6] = ["#4e79a7", "#f28e2b", "#59a14f", "#e15759", "#b07aa1", "#edc948"];

fn hex_rgb(hex: &str) -> (f64, f64, f64) {
    let v = u32::from_str_radix(&hex[1..], 16).unwrap();
    (((v >> 16) & 255) as f64 / 255.0, ((v >> 8) & 255) as f64 / 255.0, (v & 255) as f64 / 255.0)
}

fn rgb_to_hsl(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.0;
    if max == min {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        60.0 * (((g - b) / d).rem_euclid(6.0))
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h, s, l)
}

fn palette_hsl(color: u8) -> (f64, f64, f64) {
    let (r, g, b) = hex_rgb(PALETTE[color as usize % PALETTE.len()]);
    rgb_to_hsl(r, g, b)
}

/// Greedy graph colouring, highest degree first. The partition adjacency is
/// planar, so 6 colours always suffice; the fallback picks the least-used
/// colour among neighbours just in case.
pub fn assign_colors(n: usize, adjacency: &[(u32, u32)]) -> Vec<u8> {
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &(a, b) in adjacency {
        if (a as usize) < n && (b as usize) < n {
            adj[a as usize].push(b as usize);
            adj[b as usize].push(a as usize);
        }
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(adj[i].len()));
    let k = PALETTE.len();
    let mut color = vec![0u8; n];
    let mut assigned = vec![false; n];
    for &i in &order {
        let mut used = vec![false; k];
        let mut counts = vec![0u32; k];
        for &j in &adj[i] {
            if assigned[j] {
                used[color[j] as usize] = true;
                counts[color[j] as usize] += 1;
            }
        }
        color[i] = (0..k)
            .find(|&c| !used[c])
            .unwrap_or_else(|| (0..k).min_by_key(|&c| counts[c]).unwrap()) as u8;
        assigned[i] = true;
    }
    color
}

pub fn hsl_hex(h: f64, s: f64, l: f64) -> String {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let to8 = |v: f64| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    format!("#{:02x}{:02x}{:02x}", to8(r), to8(g), to8(b))
}

/// Building colour: the partition's palette colour, darkened with walking
/// time (2D colour coding). Unknown time → muted palette hue.
pub fn building_color(color: u8, t_s: Option<u32>) -> String {
    let (h, s, l) = palette_hsl(color);
    match t_s {
        Some(t) => {
            let f = (t.min(1800) as f64) / 1800.0; // cap the ramp at 30 min
            hsl_hex(h, (s * (1.0 - 0.25 * f)).clamp(0.0, 1.0), (l + 0.16 - 0.42 * f).clamp(0.15, 0.85))
        }
        None => hsl_hex(h, s * 0.25, 0.60),
    }
}

pub fn partition_color(color: u8) -> &'static str {
    PALETTE[color as usize % PALETTE.len()]
}

fn push_coord(s: &mut String, p: [f64; 2]) {
    let _ = write!(s, "[{:.6},{:.6}]", p[0], p[1]);
}

fn push_ring(s: &mut String, ring: &[[f64; 2]]) {
    s.push('[');
    for (i, p) in ring.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        push_coord(s, *p);
    }
    if ring.first() != ring.last() {
        s.push(',');
        push_coord(s, ring[0]);
    }
    s.push(']');
}

/// Pre-serialized GeoJSON geometry per building, reused across feature types.
/// rings[0] = outer, rest = holes (courtyards of multipolygon buildings).
pub fn building_geom_strings(buildings: &[Building]) -> Vec<String> {
    buildings
        .par_iter()
        .map(|b| {
            let n: usize = b.rings.iter().map(|r| r.len()).sum();
            let mut s = String::with_capacity(n * 22 + 40);
            s.push_str("{\"type\":\"Polygon\",\"coordinates\":[");
            for (i, ring) in b.rings.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                push_ring(&mut s, ring);
            }
            s.push_str("]}");
            s
        })
        .collect()
}

/// buildings.geojsonl for one feature type. `pid_t` per building:
/// None = no partition (skipped), Some((pid, None)) = partition without a
/// reachable walking time. `colors` maps pid → palette index.
pub fn write_buildings_geojsonl(
    path: &Path,
    geoms: &[String],
    pid_t: &[Option<(u32, Option<u32>)>],
    colors: &[u8],
) -> Result<usize> {
    let lines: Vec<String> = geoms
        .par_iter()
        .zip(pid_t.par_iter())
        .filter_map(|(g, pt)| {
            let (pid, t) = (*pt)?;
            let mut s = String::with_capacity(g.len() + 140);
            s.push_str("{\"type\":\"Feature\",\"tippecanoe\":{\"layer\":\"buildings\",\"minzoom\":13},\"properties\":{\"pid\":");
            let _ = write!(s, "{}", pid);
            match t {
                Some(t) => {
                    let _ = write!(s, ",\"t\":{}", t);
                }
                None => s.push_str(",\"t\":null"),
            }
            let _ = write!(s, ",\"c\":\"{}\"", building_color(colors[pid as usize], t));
            s.push_str("},\"geometry\":");
            s.push_str(g);
            s.push('}');
            Some(s)
        })
        .collect();
    let mut out = std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(path)?);
    for l in &lines {
        out.write_all(l.as_bytes())?;
        out.write_all(b"\n")?;
    }
    out.flush()?;
    Ok(lines.len())
}

pub struct PartitionOut {
    pub pid: u32,
    pub name: String,
    pub t_max_s: u32,
    /// palette index from assign_colors
    pub color: u8,
    /// polygons -> rings -> points (lng/lat)
    pub polys: Vec<Vec<Vec<[f64; 2]>>>,
}

pub fn write_partitions_geojsonl(path: &Path, parts: &[PartitionOut]) -> Result<()> {
    let mut out = std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(path)?);
    for p in parts {
        if p.polys.is_empty() {
            continue;
        }
        let mut s = String::with_capacity(4096);
        s.push_str("{\"type\":\"Feature\",\"tippecanoe\":{\"layer\":\"partitions\"},\"properties\":{\"pid\":");
        let _ = write!(s, "{}", p.pid);
        s.push_str(",\"name\":");
        let _ = write!(s, "{}", serde_json::to_string(&p.name)?);
        let _ = write!(s, ",\"t_max\":{}", p.t_max_s);
        let _ = write!(s, ",\"c\":\"{}\"", partition_color(p.color));
        s.push_str("},\"geometry\":{\"type\":\"MultiPolygon\",\"coordinates\":[");
        for (i, poly) in p.polys.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push('[');
            for (j, ring) in poly.iter().enumerate() {
                if j > 0 {
                    s.push(',');
                }
                push_ring(&mut s, ring);
            }
            s.push(']');
        }
        s.push_str("]}}\n");
        out.write_all(s.as_bytes())?;
    }
    out.flush()?;
    Ok(())
}

pub struct SiteOut {
    pub pid: u32,
    pub name: Option<String>,
    pub ll: [f64; 2],
    /// members grouped into this site (2 = paired directional bus stops)
    pub k: u32,
}

/// Compact site index for search + pid→name lookup:
/// {"sites":[[pid,name,lng,lat,k],…]}
pub fn write_sites_json(path: &Path, sites: &[SiteOut]) -> Result<()> {
    let arr: Vec<serde_json::Value> = sites
        .iter()
        .map(|s| {
            serde_json::json!([
                s.pid,
                s.name,
                (s.ll[0] * 1e6).round() / 1e6,
                (s.ll[1] * 1e6).round() / 1e6,
                s.k
            ])
        })
        .collect();
    std::fs::write(path, serde_json::to_string(&serde_json::json!({ "sites": arr }))?)?;
    Ok(())
}

/// Gzipped raw u8 direction raster (0 = terminal/none, 1–8 = N,NE,…,NW).
pub fn write_dirs_gz(path: &Path, dirs: &[u8]) -> Result<()> {
    let f = std::fs::File::create(path)?;
    let mut enc = flate2::write::GzEncoder::new(std::io::BufWriter::new(f), flate2::Compression::new(6));
    enc.write_all(dirs)?;
    enc.finish()?;
    Ok(())
}

pub fn write_city_meta(
    path: &Path,
    city: &City,
    bbox: [f64; 4],
    path_grid: &crate::grid::Grid,
) -> Result<()> {
    std::fs::write(
        path,
        serde_json::to_string_pretty(&serde_json::json!({
            "id": city.id,
            "name": city.name,
            "bbox": [
                (bbox[0] * 1e5).round() / 1e5,
                (bbox[1] * 1e5).round() / 1e5,
                (bbox[2] * 1e5).round() / 1e5,
                (bbox[3] * 1e5).round() / 1e5,
            ],
            // geometry of the {type}.dirs.gz direction rasters
            "pathGrid": {
                "w": path_grid.w,
                "h": path_grid.h,
                "west": path_grid.west,
                "north": path_grid.north,
                "dlng": path_grid.dlng,
                "dlat": path_grid.dlat,
            },
        }))?,
    )?;
    Ok(())
}

/// web/data/manifest.json — lists every city that has tiles in data/out.
pub fn write_manifest(
    web_data_dir: &Path,
    out_dir: &Path,
    cities: &[City],
    types: &[FeatureType],
    data_url_template: &str,
) -> Result<()> {
    let mut out_cities = Vec::new();
    for c in cities {
        let dir = out_dir.join(&c.id);
        let have: Vec<&FeatureType> =
            types.iter().filter(|t| dir.join(format!("{}.pmtiles", t.id)).exists()).collect();
        if have.is_empty() {
            continue;
        }
        let meta: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("meta.json"))?)?;
        out_cities.push(serde_json::json!({
            "id": c.id,
            "name": c.name,
            "bbox": meta["bbox"],
            "pathGrid": meta["pathGrid"],
            "types": have.iter().map(|t| t.id.clone()).collect::<Vec<_>>(),
        }));
    }
    std::fs::create_dir_all(web_data_dir)?;
    let manifest = serde_json::json!({
        // version cache-buster: rewritten PMTiles must never be read through
        // a stale directory cache (appended as ?v= by the UI)
        "v": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        "dataUrlTemplate": data_url_template,
        "types": types.iter().map(|t| serde_json::json!({"id": t.id, "name": t.name})).collect::<Vec<_>>(),
        "cities": out_cities,
    });
    std::fs::write(web_data_dir.join("manifest.json"), serde_json::to_string_pretty(&manifest)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colouring_no_adjacent_duplicates() {
        // ring of 7 + a hub adjacent to all (wheel graph: planar, chromatic 4)
        let n = 8u32;
        let mut adj: Vec<(u32, u32)> = (0..7).map(|i| (i, (i + 1) % 7)).collect();
        adj.extend((0..7).map(|i| (7, i)));
        let colors = assign_colors(n as usize, &adj);
        for &(a, b) in &adj {
            assert_ne!(colors[a as usize], colors[b as usize], "{a}-{b} share a colour");
        }
        assert!(colors.iter().all(|&c| (c as usize) < PALETTE.len()));
    }

    #[test]
    fn building_colors_darken_with_time() {
        let near = building_color(0, Some(0));
        let far = building_color(0, Some(1800));
        let l = |hex: &str| {
            let (r, g, b) = hex_rgb(hex);
            rgb_to_hsl(r, g, b).2
        };
        assert!(l(&near) > l(&far) + 0.2, "near {near} must be much lighter than far {far}");
    }
}
