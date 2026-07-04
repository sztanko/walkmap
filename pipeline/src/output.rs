use crate::config::{City, FeatureType};
use crate::osm::Building;
use anyhow::Result;
use rayon::prelude::*;
use std::fmt::Write as FmtWrite;
use std::io::Write;
use std::path::Path;

/// Golden-angle hue: well-spread distinct colours for adjacent partitions.
pub fn pid_hue(pid: u32) -> f64 {
    (pid as f64 * 137.50776405) % 360.0
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

/// Building colour: partition hue, lightness darkening with walking time
/// (2D colour coding). Unknown time → muted partition hue.
pub fn building_color(pid: u32, t_s: Option<u32>) -> String {
    let h = pid_hue(pid);
    match t_s {
        Some(t) => {
            let f = (t.min(1800) as f64) / 1800.0; // cap the ramp at 30 min
            hsl_hex(h, 0.60 - 0.15 * f, 0.70 - 0.42 * f)
        }
        None => hsl_hex(h, 0.15, 0.55),
    }
}

pub fn partition_color(pid: u32) -> String {
    hsl_hex(pid_hue(pid), 0.62, 0.55)
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
pub fn building_geom_strings(buildings: &[Building]) -> Vec<String> {
    buildings
        .par_iter()
        .map(|b| {
            let mut s = String::with_capacity(b.ring.len() * 22 + 40);
            s.push_str("{\"type\":\"Polygon\",\"coordinates\":[");
            push_ring(&mut s, &b.ring);
            s.push_str("]}");
            s
        })
        .collect()
}

/// buildings.geojsonl for one feature type. `pid_t` per building:
/// None = no partition (skipped), Some((pid, None)) = partition without a
/// reachable walking time.
pub fn write_buildings_geojsonl(
    path: &Path,
    geoms: &[String],
    pid_t: &[Option<(u32, Option<u32>)>],
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
            let _ = write!(s, ",\"c\":\"{}\"", building_color(pid, t));
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
        let _ = write!(s, ",\"c\":\"{}\"", partition_color(p.pid));
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
}

/// Compact site index for search + pid→name lookup: {"sites":[[pid,name,lng,lat],…]}
pub fn write_sites_json(path: &Path, sites: &[SiteOut]) -> Result<()> {
    let arr: Vec<serde_json::Value> = sites
        .iter()
        .map(|s| {
            serde_json::json!([
                s.pid,
                s.name,
                (s.ll[0] * 1e6).round() / 1e6,
                (s.ll[1] * 1e6).round() / 1e6
            ])
        })
        .collect();
    std::fs::write(path, serde_json::to_string(&serde_json::json!({ "sites": arr }))?)?;
    Ok(())
}

pub fn write_city_meta(path: &Path, city: &City, bbox: [f64; 4]) -> Result<()> {
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
            "types": have.iter().map(|t| t.id.clone()).collect::<Vec<_>>(),
        }));
    }
    std::fs::create_dir_all(web_data_dir)?;
    let manifest = serde_json::json!({
        "dataUrlTemplate": data_url_template,
        "types": types.iter().map(|t| serde_json::json!({"id": t.id, "name": t.name})).collect::<Vec<_>>(),
        "cities": out_cities,
    });
    std::fs::write(web_data_dir.join("manifest.json"), serde_json::to_string_pretty(&manifest)?)?;
    Ok(())
}
